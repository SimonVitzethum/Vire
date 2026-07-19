//! Bounds-check elision via global value numbering (GVN).
//!
//! Array accesses whose index provably lies in `[0, arr.length)` are set to
//! `checked: false` — the backend then emits them inline without a
//! bounds/NPE check (throw-free → the pending check falls away). This is
//! the same route by which Rust's LLVM frees `arr[i]` in loops from the
//! checks; here the solver proves it explicitly under closed world.
//!
//! Why GVN? The mid-level IR is not in SSA: the javac stack traffic recycles
//! slots aggressively, so index, bound, and array at the loop guard live in
//! *different* locals than at the access — even though they are the same values. A
//! local-based analysis loses the connection. GVN assigns each *value* a
//! stable number (sym): copies inherit the number, merges create a phi sym.
//! This makes "index value < length value" decidable independently of the slot.
//!
//! Three steps:
//! 1. GVN fixpoint: `env[b]` = local → sym at the block entry (pessimistic: a
//!    concrete number only when all preds agree, otherwise phi).
//! 2. Non-negativity as a global property of the syms (greatest fixpoint):
//!    const≥0, Add(nn,≥0), Mul(nn,nn), length, Phi(all-nn).
//! 3. Flow-sensitive must-analysis `lt` over sym pairs (value < value), created at
//!    branch edges. An access `arr[i]` becomes unchecked when
//!    `(sym(i), len_of[sym(arr)]) ∈ lt` and `sym(i)` is non-negative.
//! Sound: `len` is an array length (< 2^31), so `i < len` prevents the
//! overflow — at this point `nn` really does mean `i >= 0`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use fastllvm_ir::*;

pub fn elide_bounds(program: &mut Program) -> usize {
    let mut total = 0;
    for f in &mut program.functions {
        total += run(f);
    }
    total
}

/// Value-number expression. Each variant is unique through its fields, so
/// the interner maps structurally equal values to the same number.
#[derive(Clone, PartialEq, Eq, Hash)]
enum SymExpr {
    Const(i64),
    Param(u32),
    /// Opaque value, identified by its definition site.
    Opaque(u32),
    /// Phi at a block entry: (block, local).
    Phi(u32, u32),
    /// Array identity (NewArray), identified by the definition site.
    Array(u32),
    /// Length of the array with sym id.
    Len(u32),
    /// Sym id + constant (induction step).
    Add(u32, i64),
    /// Sum of two syms (canonical: id1 <= id2) — non-constant step
    /// like `j += i`.
    Add2(u32, u32),
    /// Product of two syms (canonical: id1 <= id2).
    Mul(u32, u32),
    /// `x & m` with mask m ≥ 0 — the result provably lies in [0, m].
    And(u32, i64),
    /// `x / d` with a constant divisor d ≥ 1 (also `x >> 1` for d = 2). For a
    /// non-negative x the result lies in `[0, x/d]` ⊆ `[0, x]` — the midpoint
    /// `(lo+hi)/2` of binary search / partition.
    Div(u32, i64),
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

/// Definition site → stable u32 (block, statement index).
fn site(b: usize, si: usize) -> u32 {
    ((b as u32) << 16) | (si as u32 & 0xFFFF)
}

type Env = BTreeMap<u32, u32>; // local → sym id

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
        // Integer conversion is value-transparent (same sym) when source
        // and target are integers: `(int)j`/`(long)i` do not change a value
        // lying in `[0,len)` (len < 2^31). The later lt+nn check guarantees
        // exactly this range, so the truncation is lossless.
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
        // Subtraction of a constant `x - c` = `Add(x, -c)` — the `len - 1` /
        // `hi - 1` / `mid - 1` idioms that drive shrinking search intervals.
        Rvalue::Binary(BinOp::Sub, a @ Operand::Copy(_), b) => {
            let c = match b {
                Operand::ConstI32(c) => Some(*c as i64),
                Operand::ConstI64(c) => Some(*c),
                _ => None,
            };
            match c {
                Some(c) => {
                    let x = sym_of_operand(a, env, it);
                    it.intern(SymExpr::Add(x, -c))
                }
                None => it.intern(SymExpr::Opaque(s)),
            }
        }
        // Division by a positive constant `x / d` (d ≥ 1): for non-negative x the
        // result is in [0, x/d] ⊆ [0, x]. The binary-search / partition midpoint
        // `(lo+hi)/2`. Non-negativity of x is checked separately (compute_nonneg).
        Rvalue::Binary(BinOp::Div, a @ Operand::Copy(_), b) => {
            let d = match b {
                Operand::ConstI32(c) => Some(*c as i64),
                Operand::ConstI64(c) => Some(*c),
                _ => None,
            };
            match d {
                Some(d) if d >= 1 => {
                    let x = sym_of_operand(a, env, it);
                    it.intern(SymExpr::Div(x, d))
                }
                _ => it.intern(SymExpr::Opaque(s)),
            }
        }
        // Arithmetic/logical right shift by 1 = floor division by 2 for a
        // non-negative value (`(lo+hi) >> 1`).
        Rvalue::Binary(BinOp::Shr | BinOp::UShr, a @ Operand::Copy(_), Operand::ConstI32(1)) => {
            let x = sym_of_operand(a, env, it);
            it.intern(SymExpr::Div(x, 2))
        }
        // Bit masking `x & m` (m ≥ 0): result in [0, m] — often as
        // index `arr[i & (len-1)]`/`sh[i & 1]` (power-of-2 ring buffer). The
        // mask can be an inline constant OR a const-valued local (javac
        // loads `iconst` into a slot); therefore resolve it via the sym number.
        Rvalue::Binary(BinOp::And, a, b) => {
            let sa = sym_of_operand(a, env, it);
            let sb = sym_of_operand(b, env, it);
            let const_of = |sym: u32, it: &Interner| match it.exprs[sym as usize] {
                SymExpr::Const(c) => Some(c),
                _ => None,
            };
            let (x, m) = match (const_of(sa, it), const_of(sb, it)) {
                (Some(m), _) => (sb, m),
                (_, Some(m)) => (sa, m),
                _ => (0, -1),
            };
            if m >= 0 {
                it.intern(SymExpr::And(x, m))
            } else {
                it.intern(SymExpr::Opaque(s))
            }
        }
        _ => it.intern(SymExpr::Opaque(s)),
    }
}

/// Transfer of a block: env at entry → env at exit. Builds `len_of`
/// (array sym → length sym) as a side effect.
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

/// Predecessors per block.
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

/// Merge (pessimistic): concrete number only when all preds carry the local
/// and agree; otherwise Phi(b, local).
fn merge_in(f: &Function, b: usize, preds: &[usize], env_out: &[Env], it: &mut Interner) -> Env {
    if preds.is_empty() {
        // Entry: preload parameters.
        let mut env = Env::new();
        for i in 0..f.params.len() as u32 {
            let s = it.intern(SymExpr::Param(i));
            env.insert(i, s);
        }
        return env;
    }
    // Consider all locals defined in any pred.
    let mut locals: BTreeSet<u32> = BTreeSet::new();
    for &p in preds {
        locals.extend(env_out[p].keys().copied());
    }
    let mut env = Env::new();
    for l in locals {
        // Concrete only when ALL preds carry the local and agree.
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

    // --- Step 1: GVN fixpoint (Gauss-Seidel, capped). ---
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
        return 0; // conservative: not converged → no elision
    }
    // Reconstruct env_in per block finally.
    let env_in: Vec<Env> = (0..nb).map(|b| merge_in(f, b, &preds[b], &env_out, &mut it)).collect();

    // --- Step 2: non-negativity (greatest fixpoint over syms). ---
    // phi_inc: incoming syms per phi sym (from the final env). If the local is
    // missing in *any* pred, the phi is "incomplete" (an undefined/other
    // input) → neither collapsible nor nn-provable.
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
    // Phi collapse (optimistic): a phi whose only non-self input
    // is a value S is ≡ S (loop-invariant). Necessary because the pessimistic
    // GVN would otherwise hold invariant values flowing around the loop as a phi.
    let repr = compute_repr(&it, &phi_inc, &incomplete);
    let nn = compute_nonneg(&it, &phi_inc, &repr, &incomplete);
    // Constant upper bound per sym: Const(c≥0) → c, And(_,m≥0) → m. For
    // indices without a loop guard (`sh[i & 1]`): in-bounds against a constant
    // length, if this bound < length.
    let ub_const: Vec<Option<i64>> = it
        .exprs
        .iter()
        .map(|e| match e {
            SymExpr::Const(c) if *c >= 0 => Some(*c),
            SymExpr::And(_, m) if *m >= 0 => Some(*m),
            _ => None,
        })
        .collect();

    // --- Step 3: flow-sensitive lt-analysis over sym pairs. ---
    // Edge facts: (from_block, to_block) → strict (x<y) pairs.
    let mut edge_facts: HashMap<(usize, usize), BTreeSet<(u32, u32)>> = HashMap::new();
    let mut universe: BTreeSet<(u32, u32)> = BTreeSet::new();
    for b in 0..nb {
        let Terminator::Branch { cond: Operand::Copy(c), then_blk, else_blk } = &f.blocks[b].terminator
        else {
            continue;
        };
        // Find the comparison definition of the cond local in the same block (last).
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

    // Must fixpoint: in[entry]=∅, otherwise ⊤=universe; in[b] = ∩_p (in[p] ∪ facts(p→b)).
    let mut lt_in: Vec<BTreeSet<(u32, u32)>> = vec![universe.clone(); nb];
    // Entry is the block without preds (usually block 0).
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

    // --- Mark accesses. ---
    let mut count = 0;
    for b in 0..nb {
        let mut lt = lt_in[b].clone();
        saturate_lt(&mut lt, &it, &repr, &nn);
        // Carry env along at each statement.
        let mut env = env_in[b].clone();
        let mut dummy_len = len_of.clone();
        let stmts = &mut f.blocks[b].statements;
        for si in 0..stmts.len() {
            let elide = match &stmts[si] {
                // Ref loads may be elided (pure GEP); ref stores not,
                // since `jrt_?astore` carries the covariance check (ArrayStoreException)
                // that the inline path would not have.
                Statement::ArrayLoad { arr, index, checked, .. } if *checked => {
                    provably_in_bounds(arr, index, &env, &len_of, &lt, &nn, &ub_const, &repr, &mut it)
                }
                Statement::ArrayStore { arr, index, kind, checked, .. } if *checked && !kind.is_ref() => {
                    provably_in_bounds(arr, index, &env, &len_of, &lt, &nn, &ub_const, &repr, &mut it)
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
            // Advance env past this statement (only sym definitions).
            step_env(&f_stmt(stmts, si), b, si, &mut env, &mut it, &mut dummy_len, &locals);
        }
    }
    count
}

// Helper clone of a statement for advancing env (borrow workaround).
fn f_stmt(stmts: &[Statement], si: usize) -> Statement {
    stmts[si].clone()
}

/// Advances env past a single statement (like transfer_block, but
/// individually — for the access marking).
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

#[allow(clippy::too_many_arguments)]
fn provably_in_bounds(
    arr: &Operand,
    index: &Operand,
    env: &Env,
    len_of: &HashMap<u32, u32>,
    lt: &BTreeSet<(u32, u32)>,
    nn: &BTreeSet<u32>,
    ub_const: &[Option<i64>],
    repr: &[u32],
    it: &mut Interner,
) -> bool {
    let asym = canon(repr, sym_of_operand(arr, env, it));
    let Some(&lensym0) = len_of.get(&asym) else { return false };
    let lensym = canon(repr, lensym0);
    let isym = canon(repr, sym_of_operand(index, env, it));
    // Path 1: loop-guard fact `i < len` + non-negativity.
    if lt.contains(&(isym, lensym)) && nn.contains(&isym) {
        return true;
    }
    // Path 2: constant length L, index with constant bound u ∈ [0, L).
    if let SymExpr::Const(l) = it.exprs[lensym as usize] {
        if let Some(Some(u)) = ub_const.get(isym as usize) {
            return *u >= 0 && *u < l;
        }
    }
    false
}

/// Saturate the strict-less-than set with SOUND derived facts, to a fixpoint:
///
/// - **subtract axiom**: if `(x, y)` and a sym `Add(x, c)` with `c ≤ 0` exists,
///   then `x + c ≤ x < y`, so `(Add(x,c), y)`. And `(Add(x,c), x)` for any `c < 0`
///   (`x - k < x`, the `len - 1 < len` idiom).
/// - **transitivity**: `(a,b) ∧ (b,c) ⟹ (a,c)`.
/// - **midpoint**: `(Div(Add2(a,b), 2), L)` when `(a,L) ∧ (b,L)`, `a,b ≥ 0`, and
///   `L` is a constant `l` with `2·l ≤ i32::MAX` (so `a+b` cannot overflow i32 —
///   `(a+b)/2 < l` then holds). This is the binary-search / partition midpoint.
///
/// All rules are pure integer-order reasoning (no width change), so the elision
/// stays sound. Works over canonicalized syms.
fn saturate_lt(lt: &mut BTreeSet<(u32, u32)>, it: &Interner, repr: &[u32], nn: &BTreeSet<u32>) {
    // Precompute: syms of shape Add(base,c) and Div(Add2(a,b),2), canonicalized.
    let n = it.exprs.len();
    let is_const_len = |s: u32| matches!(it.exprs.get(s as usize), Some(SymExpr::Const(l)) if *l >= 0 && l.checked_mul(2).map(|x| x <= i32::MAX as i64).unwrap_or(false));
    // Seed `len - k < len` style axioms: Add(base, c<0) < base.
    for i in 0..n {
        if let SymExpr::Add(base, c) = it.exprs[i] {
            if c < 0 {
                let a = canon(repr, i as u32);
                let b = canon(repr, base);
                if a != b {
                    lt.insert((a, b));
                }
            }
        }
    }
    loop {
        let before = lt.len();
        // Subtract axiom: (x,y) & Add(x,c≤0) ⟹ (Add(x,c), y).
        for i in 0..n {
            if let SymExpr::Add(base, c) = it.exprs[i] {
                if c <= 0 {
                    let addx = canon(repr, i as u32);
                    let x = canon(repr, base);
                    let ys: Vec<u32> = lt.iter().filter(|(a, _)| *a == x).map(|(_, y)| *y).collect();
                    for y in ys {
                        if addx != y {
                            lt.insert((addx, y));
                        }
                    }
                }
            }
        }
        // Midpoint: Div(Add2(a,b),2) < L when (a,L),(b,L), a,b≥0, L const, no overflow.
        for i in 0..n {
            if let SymExpr::Div(inner, 2) = it.exprs[i] {
                if let Some(SymExpr::Add2(a0, b0)) = it.exprs.get(inner as usize).cloned() {
                    let (a, b) = (canon(repr, a0), canon(repr, b0));
                    let mid = canon(repr, i as u32);
                    if nn.contains(&a) && nn.contains(&b) {
                        // any L bounding both a and b, const & overflow-safe.
                        let ls: Vec<u32> = lt
                            .iter()
                            .filter(|(x, l)| *x == a && is_const_len(*l) && lt.contains(&(b, *l)))
                            .map(|(_, l)| *l)
                            .collect();
                        for l in ls {
                            if mid != l {
                                lt.insert((mid, l));
                            }
                        }
                    }
                }
            }
        }
        // Transitive closure.
        let pairs: Vec<(u32, u32)> = lt.iter().copied().collect();
        for &(a, b) in &pairs {
            let bcs: Vec<u32> = pairs.iter().filter(|(x, _)| *x == b).map(|(_, c)| *c).collect();
            for c in bcs {
                if a != c {
                    lt.insert((a, c));
                }
            }
        }
        if lt.len() == before {
            break;
        }
    }
}

/// Representative of a sym after phi collapse (path following, bounds-safe).
fn canon(repr: &[u32], mut s: u32) -> u32 {
    while (s as usize) < repr.len() && repr[s as usize] != s {
        s = repr[s as usize];
    }
    s
}

/// Phi collapse: repr[p] = S if all non-self inputs of p (after
/// canonicalization) are the same value S. Fixpoint.
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
                continue; // already collapsed
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

/// Searches in block b for the comparison definition of the cond local and returns
/// (comparison kind, sym of the left operand, sym of the right operand).
fn find_cmp(f: &Function, b: usize, cond: u32, env_in: &Env, it: &mut Interner) -> Option<(BinOp, u32, u32)> {
    // Carry env up to the comparison definition; use the last matching def.
    let mut env = env_in.clone();
    let mut result = None;
    let mut dummy_len = HashMap::new();
    // Long comparisons are lowered as `jrt_lcmp(x,y) <op> 0` (lcmp yields
    // sign(x−y)). `sign(x−y) op 0 ⟺ x op y`, so we resolve the lcmp call.
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
                // `lcmp(x,y) <op> 0` → (op, x, y), otherwise a direct comparison.
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

/// Strict (x<y) facts for the then- resp. else-edge of a `Branch{cond}`,
/// where cond = Cmp(op, a, b), sa/sb the syms. Branch takes then when cond!=0.
fn strict_facts(op: BinOp, sa: u32, sb: u32) -> (Vec<(u32, u32)>, Vec<(u32, u32)>) {
    match op {
        // a<b: then ⟹ a<b; else ⟹ b<=a (not strict → nothing).
        BinOp::CmpLt => (vec![(sa, sb)], vec![]),
        // a>b: then ⟹ b<a; else ⟹ a<=b (nothing).
        BinOp::CmpGt => (vec![(sb, sa)], vec![]),
        // a>=b: then ⟹ b<=a (nothing); else ⟹ a<b.
        BinOp::CmpGe => (vec![], vec![(sa, sb)]),
        // a<=b: then ⟹ a<=b (nothing); else ⟹ b<a.
        BinOp::CmpLe => (vec![], vec![(sb, sa)]),
        _ => (vec![], vec![]),
    }
}

/// Non-negative syms (greatest fixpoint): const≥0, Add(nn,≥0), Mul(nn,nn),
/// length, Phi(all-inputs nn). Everything else counts as possibly negative.
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
                SymExpr::And(_, m) => *m >= 0,
                SymExpr::Div(x, d) => *d >= 1 && nn[canon(repr, *x) as usize],
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
    // nn also holds for collapsed syms via their representative.
    (0..n as u32).filter(|&i| nn[canon(repr, i) as usize]).collect()
}
