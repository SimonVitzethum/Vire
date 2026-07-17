//! Bounds-Check-Elision via globales Value-Numbering (GVN).
//!
//! Array-Zugriffe, deren Index beweisbar in `[0, arr.length)` liegt, werden auf
//! `checked: false` gesetzt — das Backend emittiert sie dann inline ohne
//! Bounds-/NPE-Prüfung (throw-frei → die pending-Prüfung fällt weg). Das ist
//! der Weg, über den auch Rusts LLVM `arr[i]` in Schleifen von den Checks
//! befreit; hier beweist es der Solver explizit unter Closed World.
//!
//! Warum GVN? Das Mittel-IR ist nicht in SSA: der javac-Stackverkehr recycelt
//! Slots aggressiv, sodass Index, Schranke und Array am Schleifenwächter in
//! *anderen* Locals liegen als am Zugriff — obwohl es dieselben Werte sind. Eine
//! lokal-basierte Analyse verliert die Verbindung. GVN vergibt jedem *Wert* eine
//! stabile Nummer (Sym): Kopien erben die Nummer, Merges erzeugen ein Phi-Sym.
//! Damit ist „Index-Wert < Längen-Wert" slot-unabhängig entscheidbar.
//!
//! Drei Schritte:
//! 1. GVN-Fixpunkt: `env[b]` = Local → Sym am Blockeingang (pessimistisch: eine
//!    konkrete Nummer nur bei Übereinstimmung aller Preds, sonst Phi).
//! 2. Nichtnegativität als globale Eigenschaft der Syms (größter Fixpunkt):
//!    const≥0, Add(nn,≥0), Mul(nn,nn), Länge, Phi(alle-nn).
//! 3. Flusssensitive Must-Analyse `lt` über Sym-Paare (Wert < Wert), erzeugt an
//!    Branch-Kanten. Ein Zugriff `arr[i]` wird unchecked, wenn
//!    `(sym(i), len_of[sym(arr)]) ∈ lt` und `sym(i)` nichtnegativ ist.
//! Sound: `len` ist eine Array-Länge (< 2^31), also verhindert `i < len` den
//! Überlauf — `nn` bedeutet an diesem Punkt tatsächlich `i >= 0`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use fastllvm_ir::*;

pub fn elide_bounds(program: &mut Program) -> usize {
    let mut total = 0;
    for f in &mut program.functions {
        total += run(f);
    }
    total
}

/// Wertnummer-Ausdruck. Jede Variante ist über ihre Felder eindeutig, sodass
/// der Interner strukturell gleiche Werte auf dieselbe Nummer abbildet.
#[derive(Clone, PartialEq, Eq, Hash)]
enum SymExpr {
    Const(i64),
    Param(u32),
    /// Undurchsichtiger Wert, identifiziert durch seine Definitionsstelle.
    Opaque(u32),
    /// Phi an einem Blockeingang: (Block, Local).
    Phi(u32, u32),
    /// Array-Identität (NewArray), identifiziert durch die Definitionsstelle.
    Array(u32),
    /// Länge des Arrays mit Sym-Id.
    Len(u32),
    /// Sym-Id + Konstante (Induktionsschritt).
    Add(u32, i64),
    /// Summe zweier Syms (kanonisch: id1 <= id2) — nicht-konstanter Schritt
    /// wie `j += i`.
    Add2(u32, u32),
    /// Produkt zweier Syms (kanonisch: id1 <= id2).
    Mul(u32, u32),
}

#[derive(Default)]
struct Interner {
    map: HashMap<SymExpr, u32>,
    exprs: Vec<SymExpr>,
}

impl Interner {
    fn intern(&mut self, e: SymExpr) -> u32 {
        if let Some(&i) = self.map.get(&e) {
            return i;
        }
        let i = self.exprs.len() as u32;
        self.map.insert(e.clone(), i);
        self.exprs.push(e);
        i
    }
}

/// Definitionsstelle → stabile u32 (Block, Statement-Index).
fn site(b: usize, si: usize) -> u32 {
    ((b as u32) << 16) | (si as u32 & 0xFFFF)
}

type Env = BTreeMap<u32, u32>; // Local → Sym-Id

fn sym_of_operand(op: &Operand, env: &Env, it: &mut Interner) -> u32 {
    match op {
        Operand::Copy(l) => match env.get(&l.0) {
            Some(&s) => s,
            None => it.intern(SymExpr::Opaque(0xF000_0000 | l.0)),
        },
        Operand::ConstI32(c) => it.intern(SymExpr::Const(*c as i64)),
        Operand::ConstI64(c) => it.intern(SymExpr::Const(*c)),
        Operand::ConstF32(_) => it.intern(SymExpr::Opaque(0xF320_0000)),
        Operand::ConstF64(_) => it.intern(SymExpr::Opaque(0xF640_0000)),
        Operand::ConstStr(s) => it.intern(SymExpr::Opaque(0x5000_0000 | (*s & 0x0FFF_FFFF))),
        Operand::ConstClass(_) => it.intern(SymExpr::Opaque(0xC000_0000)),
        Operand::ConstNull => it.intern(SymExpr::Opaque(0x0000_0001)),
    }
}

fn is_int(t: Ty) -> bool {
    matches!(t, Ty::I32 | Ty::I64)
}

fn sym_of_rvalue(rv: &Rvalue, env: &Env, it: &mut Interner, s: u32, dst: Ty, locals: &[Ty]) -> u32 {
    match rv {
        Rvalue::Use(op) => sym_of_operand(op, env, it),
        // Ganzzahl-Konvertierung ist werttransparent (gleiches Sym), wenn Quelle
        // und Ziel Ganzzahlen sind: `(int)j`/`(long)i` ändern den in `[0,len)`
        // (len < 2^31) liegenden Wert nicht. Die spätere lt+nn-Prüfung stellt
        // genau diesen Bereich sicher, sodass die Trunkierung verlustfrei ist.
        Rvalue::Convert(Operand::Copy(l)) if is_int(dst) && is_int(*locals.get(l.0 as usize).unwrap_or(&Ty::Ref)) => {
            sym_of_operand(&Operand::Copy(*l), env, it)
        }
        Rvalue::Binary(BinOp::Add, a, b) => match (a, b) {
            (Operand::Copy(_), Operand::ConstI32(c)) => {
                let x = sym_of_operand(a, env, it);
                it.intern(SymExpr::Add(x, *c as i64))
            }
            (Operand::ConstI32(c), Operand::Copy(_)) => {
                let x = sym_of_operand(b, env, it);
                it.intern(SymExpr::Add(x, *c as i64))
            }
            (Operand::Copy(_), Operand::ConstI64(c)) => {
                let x = sym_of_operand(a, env, it);
                it.intern(SymExpr::Add(x, *c))
            }
            (Operand::ConstI64(c), Operand::Copy(_)) => {
                let x = sym_of_operand(b, env, it);
                it.intern(SymExpr::Add(x, *c))
            }
            (Operand::Copy(_), Operand::Copy(_)) => {
                let x = sym_of_operand(a, env, it);
                let y = sym_of_operand(b, env, it);
                let (lo, hi) = if x <= y { (x, y) } else { (y, x) };
                it.intern(SymExpr::Add2(lo, hi))
            }
            _ => it.intern(SymExpr::Opaque(s)),
        },
        Rvalue::Binary(BinOp::Mul, a, b) => match (a, b) {
            (Operand::Copy(_), Operand::Copy(_)) => {
                let x = sym_of_operand(a, env, it);
                let y = sym_of_operand(b, env, it);
                let (lo, hi) = if x <= y { (x, y) } else { (y, x) };
                it.intern(SymExpr::Mul(lo, hi))
            }
            _ => it.intern(SymExpr::Opaque(s)),
        },
        _ => it.intern(SymExpr::Opaque(s)),
    }
}

/// Transfer eines Blocks: env am Eingang → env am Ausgang. Baut nebenbei
/// `len_of` (Array-Sym → Längen-Sym) auf.
fn transfer_block(
    f: &Function,
    b: usize,
    env_in: &Env,
    it: &mut Interner,
    len_of: &mut HashMap<u32, u32>,
) -> Env {
    let mut env = env_in.clone();
    for (si, st) in f.blocks[b].statements.iter().enumerate() {
        match st {
            Statement::Assign(d, rv) => {
                let dt = f.locals.get(d.0 as usize).copied().unwrap_or(Ty::Ref);
                let s = sym_of_rvalue(rv, &env, it, site(b, si), dt, &f.locals);
                env.insert(d.0, s);
            }
            Statement::NewArray { dest, len, .. } => {
                let lensym = sym_of_operand(len, &env, it);
                let asym = it.intern(SymExpr::Array(site(b, si)));
                len_of.insert(asym, lensym);
                env.insert(dest.0, asym);
            }
            Statement::ArrayLen { dest, arr } => {
                let asym = sym_of_operand(arr, &env, it);
                let lensym = match len_of.get(&asym) {
                    Some(&l) => l,
                    None => {
                        let l = it.intern(SymExpr::Len(asym));
                        len_of.insert(asym, l);
                        l
                    }
                };
                env.insert(dest.0, lensym);
            }
            Statement::New { dest, .. }
            | Statement::StackNew { dest, .. }
            | Statement::GetField { dest, .. }
            | Statement::GetStatic { dest, .. }
            | Statement::InstanceOf { dest, .. }
            | Statement::InstanceOfPending { dest, .. }
            | Statement::ArrayLoad { dest, .. } => {
                let s = it.intern(SymExpr::Opaque(site(b, si)));
                env.insert(dest.0, s);
            }
            Statement::Call { dest: Some(d), .. }
            | Statement::CallGuarded { dest: Some(d), .. }
            | Statement::CallVirtual { dest: Some(d), .. }
            | Statement::CallPoly { dest: Some(d), .. } => {
                let s = it.intern(SymExpr::Opaque(site(b, si)));
                env.insert(d.0, s);
            }
            _ => {}
        }
    }
    env
}

/// Prädezessoren je Block.
fn predecessors(f: &Function) -> Vec<Vec<usize>> {
    let nb = f.blocks.len();
    let mut preds = vec![Vec::new(); nb];
    for (b, bb) in f.blocks.iter().enumerate() {
        for s in succ_blocks(&bb.terminator) {
            preds[s].push(b);
        }
    }
    preds
}

fn succ_blocks(t: &Terminator) -> Vec<usize> {
    match t {
        Terminator::Goto(b) => vec![b.0 as usize],
        Terminator::Branch { then_blk, else_blk, .. } => vec![then_blk.0 as usize, else_blk.0 as usize],
        Terminator::Switch { default, cases, .. } => {
            let mut v = vec![default.0 as usize];
            v.extend(cases.iter().map(|(_, b)| b.0 as usize));
            v
        }
        Terminator::Return(_) => vec![],
    }
}

/// Merge (pessimistisch): konkrete Nummer nur, wenn alle Preds das Local führen
/// und übereinstimmen; sonst Phi(b, local).
fn merge_in(f: &Function, b: usize, preds: &[usize], env_out: &[Env], it: &mut Interner) -> Env {
    if preds.is_empty() {
        // Entry: Parameter vorbelegen.
        let mut env = Env::new();
        for i in 0..f.params.len() as u32 {
            let s = it.intern(SymExpr::Param(i));
            env.insert(i, s);
        }
        return env;
    }
    // Alle in irgendeinem Pred definierten Locals betrachten.
    let mut locals: BTreeSet<u32> = BTreeSet::new();
    for &p in preds {
        locals.extend(env_out[p].keys().copied());
    }
    let mut env = Env::new();
    for l in locals {
        // Konkret nur, wenn ALLE Preds das Local führen und sich einig sind.
        let first = env_out[preds[0]].get(&l).copied();
        let agree = first.is_some()
            && preds.iter().all(|&p| env_out[p].get(&l).copied() == first);
        let sym = match (agree, first) {
            (true, Some(s)) => s,
            _ => it.intern(SymExpr::Phi(b as u32, l)),
        };
        env.insert(l, sym);
    }
    env
}

fn run(f: &mut Function) -> usize {
    let nb = f.blocks.len();
    if nb == 0 {
        return 0;
    }
    let preds = predecessors(f);
    let locals = f.locals.clone();
    let mut it = Interner::default();

    // --- Schritt 1: GVN-Fixpunkt (Gauss-Seidel, gedeckelt). ---
    let mut env_out: Vec<Env> = vec![Env::new(); nb];
    let mut len_of: HashMap<u32, u32> = HashMap::new();
    let mut converged = false;
    for _ in 0..200 {
        let mut changed = false;
        len_of.clear();
        for b in 0..nb {
            let env_in = merge_in(f, b, &preds[b], &env_out, &mut it);
            let out = transfer_block(f, b, &env_in, &mut it, &mut len_of);
            if out != env_out[b] {
                env_out[b] = out;
                changed = true;
            }
        }
        if !changed {
            converged = true;
            break;
        }
    }
    if !converged {
        return 0; // konservativ: unkonvergiert → keine Elision
    }
    // env_in je Block final rekonstruieren.
    let env_in: Vec<Env> = (0..nb).map(|b| merge_in(f, b, &preds[b], &env_out, &mut it)).collect();

    // --- Schritt 2: Nichtnegativität (größter Fixpunkt über Syms). ---
    // phi_inc: eingehende Syms je Phi-Sym (aus dem finalen env). Fehlt das Local
    // in *irgendeinem* Pred, ist das Phi „incomplete" (ein undefinierter/anderer
    // Eingang) → weder kollabierbar noch nn-beweisbar.
    let mut phi_inc: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut incomplete: BTreeSet<u32> = BTreeSet::new();
    for b in 0..nb {
        for (&l, &s) in &env_in[b] {
            if matches!(it.exprs[s as usize], SymExpr::Phi(pb, pl) if pb == b as u32 && pl == l) {
                let mut inc = Vec::new();
                for &p in &preds[b] {
                    match env_out[p].get(&l) {
                        Some(&v) => inc.push(v),
                        None => {
                            incomplete.insert(s);
                        }
                    }
                }
                phi_inc.entry(s).or_default().extend(inc);
            }
        }
    }
    // Phi-Kollaps (optimistisch): ein Phi, dessen einziger Nicht-Selbst-Eingang
    // ein Wert S ist, ist ≡ S (schleifeninvariant). Nötig, weil der pessimistische
    // GVN invariante Werte, die um die Schleife fließen, sonst als Phi festhält.
    let repr = compute_repr(&it, &phi_inc, &incomplete);
    let nn = compute_nonneg(&it, &phi_inc, &repr, &incomplete);

    // --- Schritt 3: flusssensitive lt-Analyse über Sym-Paare. ---
    // Kanten-Fakten: (from_block, to_block) → strikte (x<y)-Paare.
    let mut edge_facts: HashMap<(usize, usize), BTreeSet<(u32, u32)>> = HashMap::new();
    let mut universe: BTreeSet<(u32, u32)> = BTreeSet::new();
    for b in 0..nb {
        let Terminator::Branch { cond: Operand::Copy(c), then_blk, else_blk } = &f.blocks[b].terminator
        else {
            continue;
        };
        // Vergleichs-Definition des cond-Locals im selben Block finden (letzte).
        let Some((op, sa0, sb0)) = find_cmp(f, b, c.0, &env_in[b], &mut it) else {
            continue;
        };
        let (sa, sb) = (canon(&repr, sa0), canon(&repr, sb0));
        let (then_pairs, else_pairs) = strict_facts(op, sa, sb);
        let t = then_blk.0 as usize;
        let e = else_blk.0 as usize;
        for p in &then_pairs {
            universe.insert(*p);
        }
        for p in &else_pairs {
            universe.insert(*p);
        }
        edge_facts.entry((b, t)).or_default().extend(then_pairs);
        edge_facts.entry((b, e)).or_default().extend(else_pairs);
    }

    // Must-Fixpunkt: in[entry]=∅, sonst ⊤=universe; in[b] = ∩_p (in[p] ∪ facts(p→b)).
    let mut lt_in: Vec<BTreeSet<(u32, u32)>> = vec![universe.clone(); nb];
    // Entry ist der Block ohne Preds (üblich Block 0).
    for b in 0..nb {
        if preds[b].is_empty() {
            lt_in[b].clear();
        }
    }
    loop {
        let mut changed = false;
        for b in 0..nb {
            if preds[b].is_empty() {
                continue;
            }
            let mut new: Option<BTreeSet<(u32, u32)>> = None;
            for &p in &preds[b] {
                let mut contrib = lt_in[p].clone();
                if let Some(fs) = edge_facts.get(&(p, b)) {
                    contrib.extend(fs.iter().copied());
                }
                new = Some(match new {
                    None => contrib,
                    Some(acc) => acc.intersection(&contrib).copied().collect(),
                });
            }
            if let Some(n) = new {
                if n != lt_in[b] {
                    lt_in[b] = n;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // --- Zugriffe markieren. ---
    let mut count = 0;
    for b in 0..nb {
        let lt = lt_in[b].clone();
        // env am jeweiligen Statement mitführen.
        let mut env = env_in[b].clone();
        let mut dummy_len = len_of.clone();
        let stmts = &mut f.blocks[b].statements;
        for si in 0..stmts.len() {
            let elide = match &stmts[si] {
                Statement::ArrayLoad { arr, index, kind, checked, .. }
                | Statement::ArrayStore { arr, index, kind, checked, .. }
                    if *checked && !kind.is_ref() =>
                {
                    provably_in_bounds(arr, index, &env, &len_of, &lt, &nn, &repr, &mut it)
                }
                _ => false,
            };
            if elide {
                match &mut stmts[si] {
                    Statement::ArrayLoad { checked, .. } | Statement::ArrayStore { checked, .. } => {
                        *checked = false;
                        count += 1;
                    }
                    _ => {}
                }
            }
            // env um dieses Statement fortschreiben (nur Sym-Definitionen).
            step_env(&f_stmt(stmts, si), b, si, &mut env, &mut it, &mut dummy_len, &locals);
        }
    }
    count
}

// Hilfsklon eines Statements zum Fortschreiben von env (Borrow-Umgehung).
fn f_stmt(stmts: &[Statement], si: usize) -> Statement {
    stmts[si].clone()
}

/// Schreibt env um ein einzelnes Statement fort (wie transfer_block, aber
/// einzeln — für die Zugriffsmarkierung).
fn step_env(st: &Statement, b: usize, si: usize, env: &mut Env, it: &mut Interner, len_of: &mut HashMap<u32, u32>, locals: &[Ty]) {
    match st {
        Statement::Assign(d, rv) => {
            let dt = locals.get(d.0 as usize).copied().unwrap_or(Ty::Ref);
            let s = sym_of_rvalue(rv, env, it, site(b, si), dt, locals);
            env.insert(d.0, s);
        }
        Statement::NewArray { dest, len, .. } => {
            let lensym = sym_of_operand(len, env, it);
            let asym = it.intern(SymExpr::Array(site(b, si)));
            len_of.insert(asym, lensym);
            env.insert(dest.0, asym);
        }
        Statement::ArrayLen { dest, arr } => {
            let asym = sym_of_operand(arr, env, it);
            let lensym = match len_of.get(&asym) {
                Some(&l) => l,
                None => {
                    let l = it.intern(SymExpr::Len(asym));
                    len_of.insert(asym, l);
                    l
                }
            };
            env.insert(dest.0, lensym);
        }
        Statement::New { dest, .. }
        | Statement::StackNew { dest, .. }
        | Statement::GetField { dest, .. }
        | Statement::GetStatic { dest, .. }
        | Statement::InstanceOf { dest, .. }
        | Statement::InstanceOfPending { dest, .. }
        | Statement::ArrayLoad { dest, .. } => {
            let s = it.intern(SymExpr::Opaque(site(b, si)));
            env.insert(dest.0, s);
        }
        Statement::Call { dest: Some(d), .. }
        | Statement::CallGuarded { dest: Some(d), .. }
        | Statement::CallVirtual { dest: Some(d), .. }
        | Statement::CallPoly { dest: Some(d), .. } => {
            let s = it.intern(SymExpr::Opaque(site(b, si)));
            env.insert(d.0, s);
        }
        _ => {}
    }
}

fn provably_in_bounds(
    arr: &Operand,
    index: &Operand,
    env: &Env,
    len_of: &HashMap<u32, u32>,
    lt: &BTreeSet<(u32, u32)>,
    nn: &BTreeSet<u32>,
    repr: &[u32],
    it: &mut Interner,
) -> bool {
    let asym = canon(repr, sym_of_operand(arr, env, it));
    let Some(&lensym0) = len_of.get(&asym) else { return false };
    let lensym = canon(repr, lensym0);
    let isym = canon(repr, sym_of_operand(index, env, it));
    lt.contains(&(isym, lensym)) && nn.contains(&isym)
}

/// Repräsentant eines Syms nach Phi-Kollaps (Pfadverfolgung, bounds-sicher).
fn canon(repr: &[u32], mut s: u32) -> u32 {
    while (s as usize) < repr.len() && repr[s as usize] != s {
        s = repr[s as usize];
    }
    s
}

/// Phi-Kollaps: repr[p] = S, wenn alle Nicht-Selbst-Eingänge von p (nach
/// Kanonisierung) derselbe Wert S sind. Fixpunkt.
fn compute_repr(it: &Interner, phi_inc: &HashMap<u32, Vec<u32>>, incomplete: &BTreeSet<u32>) -> Vec<u32> {
    let n = it.exprs.len();
    let mut repr: Vec<u32> = (0..n as u32).collect();
    loop {
        let mut changed = false;
        for i in 0..n {
            if !matches!(it.exprs[i], SymExpr::Phi(..)) || incomplete.contains(&(i as u32)) {
                continue;
            }
            let ci = canon(&repr, i as u32);
            if ci != i as u32 {
                continue; // schon kollabiert
            }
            let Some(inc) = phi_inc.get(&(i as u32)) else { continue };
            let mut distinct: BTreeSet<u32> = BTreeSet::new();
            for &s in inc {
                let cs = canon(&repr, s);
                if cs != ci {
                    distinct.insert(cs);
                }
            }
            if distinct.len() == 1 {
                repr[ci as usize] = *distinct.iter().next().unwrap();
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    repr
}

/// Sucht im Block b die Vergleichs-Definition des cond-Locals und liefert
/// (Vergleichsart, Sym des linken, Sym des rechten Operanden).
fn find_cmp(f: &Function, b: usize, cond: u32, env_in: &Env, it: &mut Interner) -> Option<(BinOp, u32, u32)> {
    // env bis zur Vergleichsdefinition mitführen; letzte passende Def nutzen.
    let mut env = env_in.clone();
    let mut result = None;
    let mut dummy_len = HashMap::new();
    // Long-Vergleiche werden als `jrt_lcmp(x,y) <op> 0` gesenkt (lcmp liefert
    // sign(x−y)). `sign(x−y) op 0 ⟺ x op y`, also lösen wir den lcmp-Aufruf auf.
    let mut lcmp: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
    for (si, st) in f.blocks[b].statements.iter().enumerate() {
        if let Statement::Call { dest: Some(d), func, args } = st {
            if func == "jrt_lcmp" && args.len() == 2 {
                let x = sym_of_operand(&args[0], &env, it);
                let y = sym_of_operand(&args[1], &env, it);
                lcmp.insert(d.0, (x, y));
            }
        }
        if let Statement::Assign(d, Rvalue::Binary(op, a, c)) = st {
            if d.0 == cond && matches!(op, BinOp::CmpLt | BinOp::CmpGe | BinOp::CmpGt | BinOp::CmpLe) {
                // `lcmp(x,y) <op> 0` → (op, x, y), sonst direkter Vergleich.
                let lc = match (a, c) {
                    (Operand::Copy(l), Operand::ConstI32(0)) => lcmp.get(&l.0).copied(),
                    _ => None,
                };
                let (sa, sb) = match lc {
                    Some((x, y)) => (x, y),
                    None => (sym_of_operand(a, &env, it), sym_of_operand(c, &env, it)),
                };
                result = Some((*op, sa, sb));
            }
        }
        step_env(st, b, si, &mut env, it, &mut dummy_len, &f.locals);
    }
    result
}

/// Strikte (x<y)-Fakten für die then- bzw. else-Kante eines `Branch{cond}`,
/// wobei cond = Cmp(op, a, b), sa/sb die Syms. Branch nimmt then bei cond!=0.
fn strict_facts(op: BinOp, sa: u32, sb: u32) -> (Vec<(u32, u32)>, Vec<(u32, u32)>) {
    match op {
        // a<b: then ⟹ a<b; else ⟹ b<=a (nicht strikt → nichts).
        BinOp::CmpLt => (vec![(sa, sb)], vec![]),
        // a>b: then ⟹ b<a; else ⟹ a<=b (nichts).
        BinOp::CmpGt => (vec![(sb, sa)], vec![]),
        // a>=b: then ⟹ b<=a (nichts); else ⟹ a<b.
        BinOp::CmpGe => (vec![], vec![(sa, sb)]),
        // a<=b: then ⟹ a<=b (nichts); else ⟹ b<a.
        BinOp::CmpLe => (vec![], vec![(sb, sa)]),
        _ => (vec![], vec![]),
    }
}

/// Nichtnegative Syms (größter Fixpunkt): const≥0, Add(nn,≥0), Mul(nn,nn),
/// Länge, Phi(alle-Eingänge nn). Alles andere gilt als möglicherweise negativ.
fn compute_nonneg(it: &Interner, phi_inc: &HashMap<u32, Vec<u32>>, repr: &[u32], incomplete: &BTreeSet<u32>) -> BTreeSet<u32> {
    let n = it.exprs.len();
    let mut nn = vec![true; n];
    loop {
        let mut changed = false;
        for i in 0..n {
            if !nn[i] {
                continue;
            }
            let ok = match &it.exprs[i] {
                SymExpr::Const(c) => *c >= 0,
                SymExpr::Len(_) => true,
                SymExpr::Add(s, c) => *c >= 0 && nn[canon(repr, *s) as usize],
                SymExpr::Add2(a, b) => nn[canon(repr, *a) as usize] && nn[canon(repr, *b) as usize],
                SymExpr::Mul(a, b) => nn[canon(repr, *a) as usize] && nn[canon(repr, *b) as usize],
                SymExpr::Phi(..) => !incomplete.contains(&(i as u32))
                    && phi_inc
                        .get(&(i as u32))
                        .map(|inc| !inc.is_empty() && inc.iter().all(|&s| nn[canon(repr, s) as usize]))
                        .unwrap_or(false),
                SymExpr::Param(_) | SymExpr::Opaque(_) | SymExpr::Array(_) => false,
            };
            if !ok {
                nn[i] = false;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // nn gilt auch für kollabierte Syms über ihren Repräsentanten.
    (0..n as u32).filter(|&i| nn[canon(repr, i) as usize]).collect()
}
