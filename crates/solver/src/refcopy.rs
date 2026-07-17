//! Elimination redundanter Referenz-Selbstkopien (RC-neutral).
//!
//! javac lädt eine Objekt-/Array-Referenz vor jedem Zugriff neu in einen
//! Stack-Slot (`aload`), was das Frontend als `Assign(d, Copy(s))` auf einem
//! Ref-Local materialisiert. Für Ref-Locals emittiert das Backend die
//! Owning-Slot-Disziplin: `retain(neu); store; release(alt)`. In heißen
//! Schleifen ist das ein retain/release-Paar **je Iteration** auf einer
//! schleifeninvarianten Referenz — Overhead, den Rust nicht hat.
//!
//! Beweist globales Value-Numbering, dass der Zielslot `d` an dieser Stelle
//! *bereits* den Wert von `s` hält (`env[d] == env[s]`), dann ist die Kopie ein
//! No-Op: `retain(x)` gefolgt von `release(alt = x)` hebt sich exakt auf, und
//! der Store schreibt denselben Wert zurück. Das Statement ist damit **RC-
//! neutral entfernbar** — unabhängig von Ownership, also ohne die Heap-Bilanz
//! (0 live) zu gefährden. Genau die Selbst-Refreshes in Schleifen verschwinden.
//!
//! GVN wie in `bounds`: pessimistischer Fixpunkt (konkrete Nummer nur bei
//! Pred-Einigkeit, sonst Phi) plus optimistischer Phi-Kollaps für
//! schleifeninvariante Referenzen.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use fastllvm_ir::*;

pub fn elide_redundant_ref_copies(program: &mut Program) -> usize {
    let mut total = 0;
    for f in &mut program.functions {
        total += run(f);
    }
    total
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum Sym {
    Null,
    Param(u32),
    /// Frisch erzeugte Referenz (New/Call/GetField/…), per Definitionsstelle.
    Def(u32),
    /// Phi an einem Blockeingang: (Block, Local).
    Phi(u32, u32),
}

#[derive(Default)]
struct Interner {
    map: HashMap<Sym, u32>,
    syms: Vec<Sym>,
}
impl Interner {
    fn intern(&mut self, s: Sym) -> u32 {
        if let Some(&i) = self.map.get(&s) {
            return i;
        }
        let i = self.syms.len() as u32;
        self.map.insert(s.clone(), i);
        self.syms.push(s);
        i
    }
}

fn site(b: usize, si: usize) -> u32 {
    ((b as u32) << 16) | (si as u32 & 0xFFFF)
}

type Env = BTreeMap<u32, u32>; // Ref-Local → Sym-Id

fn is_ref(f: &Function, l: u32) -> bool {
    f.locals.get(l as usize).copied() == Some(Ty::Ref)
}

/// Sym des von einem Statement definierten Ref-Locals (falls es eines gibt).
fn def_sym(f: &Function, st: &Statement, env: &Env, it: &mut Interner, b: usize, si: usize) -> Option<(u32, u32)> {
    let (d, sym) = match st {
        Statement::Assign(d, Rvalue::Use(Operand::Copy(s))) if is_ref(f, d.0) => {
            let s = env.get(&s.0).copied().unwrap_or_else(|| it.intern(Sym::Def(0xF000_0000 | s.0)));
            (d.0, s)
        }
        Statement::Assign(d, Rvalue::Use(Operand::ConstNull)) if is_ref(f, d.0) => (d.0, it.intern(Sym::Null)),
        Statement::Assign(d, _) if is_ref(f, d.0) => (d.0, it.intern(Sym::Def(site(b, si)))),
        Statement::New { dest, .. }
        | Statement::StackNew { dest, .. }
        | Statement::NewArray { dest, .. }
        | Statement::GetField { dest, .. }
        | Statement::GetStatic { dest, .. }
        | Statement::ArrayLoad { dest, .. }
            if is_ref(f, dest.0) =>
        {
            (dest.0, it.intern(Sym::Def(site(b, si))))
        }
        Statement::Call { dest: Some(d), .. }
        | Statement::CallGuarded { dest: Some(d), .. }
        | Statement::CallVirtual { dest: Some(d), .. }
        | Statement::CallPoly { dest: Some(d), .. }
            if is_ref(f, d.0) =>
        {
            (d.0, it.intern(Sym::Def(site(b, si))))
        }
        _ => return None,
    };
    Some((d, sym))
}

fn transfer_block(f: &Function, b: usize, env_in: &Env, it: &mut Interner) -> Env {
    let mut env = env_in.clone();
    for (si, st) in f.blocks[b].statements.iter().enumerate() {
        if let Some((d, sym)) = def_sym(f, st, &env, it, b, si) {
            env.insert(d, sym);
        }
    }
    env
}

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

fn merge_in(f: &Function, b: usize, preds: &[usize], env_out: &[Env], it: &mut Interner) -> Env {
    if preds.is_empty() {
        let mut env = Env::new();
        for i in 0..f.params.len() as u32 {
            if is_ref(f, i) {
                let s = it.intern(Sym::Param(i));
                env.insert(i, s);
            }
        }
        return env;
    }
    let mut locals: BTreeSet<u32> = BTreeSet::new();
    for &p in preds {
        locals.extend(env_out[p].keys().copied());
    }
    let mut env = Env::new();
    for l in locals {
        let first = env_out[preds[0]].get(&l).copied();
        let agree = first.is_some() && preds.iter().all(|&p| env_out[p].get(&l).copied() == first);
        let sym = match (agree, first) {
            (true, Some(s)) => s,
            _ => it.intern(Sym::Phi(b as u32, l)),
        };
        env.insert(l, sym);
    }
    env
}

fn canon(repr: &[u32], mut s: u32) -> u32 {
    while (s as usize) < repr.len() && repr[s as usize] != s {
        s = repr[s as usize];
    }
    s
}

/// Phi-Kollaps: repr[p] = S, wenn alle Nicht-Selbst-Eingänge gleich S sind.
fn compute_repr(it: &Interner, phi_inc: &HashMap<u32, Vec<u32>>, incomplete: &BTreeSet<u32>) -> Vec<u32> {
    let n = it.syms.len();
    let mut repr: Vec<u32> = (0..n as u32).collect();
    loop {
        let mut changed = false;
        for i in 0..n {
            if !matches!(it.syms[i], Sym::Phi(..)) || incomplete.contains(&(i as u32)) {
                continue;
            }
            let ci = canon(&repr, i as u32);
            if ci != i as u32 {
                continue;
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

fn run(f: &mut Function) -> usize {
    let nb = f.blocks.len();
    if nb == 0 {
        return 0;
    }
    let preds = predecessors(f);
    let mut it = Interner::default();

    // GVN-Fixpunkt.
    let mut env_out: Vec<Env> = vec![Env::new(); nb];
    let mut converged = false;
    for _ in 0..200 {
        let mut changed = false;
        for b in 0..nb {
            let env_in = merge_in(f, b, &preds[b], &env_out, &mut it);
            let out = transfer_block(f, b, &env_in, &mut it);
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
        return 0;
    }
    let env_in: Vec<Env> = (0..nb).map(|b| merge_in(f, b, &preds[b], &env_out, &mut it)).collect();

    // Phi-Eingänge sammeln. Fehlt das Local in *irgendeinem* Pred, hat das Phi
    // einen undefinierten/anderen Eingang → nicht kollabierbar (sonst würde es
    // fälschlich mit dem einen definierten Zweig gleichgesetzt).
    let mut phi_inc: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut incomplete: BTreeSet<u32> = BTreeSet::new();
    for b in 0..nb {
        for (&l, &s) in &env_in[b] {
            if matches!(it.syms[s as usize], Sym::Phi(pb, pl) if pb == b as u32 && pl == l) {
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
    let repr = compute_repr(&it, &phi_inc, &incomplete);

    // Redundante Selbstkopien entfernen: `Assign(d, Copy(s))` mit d,s Ref und
    // env[d] == env[s] unmittelbar davor.
    let mut removed = 0;
    for b in 0..nb {
        let mut env = env_in[b].clone();
        let mut kill: Vec<usize> = Vec::new();
        for (si, st) in f.blocks[b].statements.iter().enumerate() {
            if let Statement::Assign(d, Rvalue::Use(Operand::Copy(s))) = st {
                if is_ref(f, d.0) && is_ref(f, s.0) {
                    let ds = env.get(&d.0).map(|&x| canon(&repr, x));
                    let ss = env.get(&s.0).map(|&x| canon(&repr, x));
                    if ds.is_some() && ds == ss {
                        kill.push(si);
                    }
                }
            }
            if let Some((d, sym)) = def_sym(f, st, &env, &mut it, b, si) {
                env.insert(d, sym);
            }
        }
        if !kill.is_empty() {
            let killset: BTreeSet<usize> = kill.into_iter().collect();
            let mut idx = 0;
            f.blocks[b].statements.retain(|_| {
                let keep = !killset.contains(&idx);
                idx += 1;
                keep
            });
            removed += killset.len();
        }
    }
    removed
}
