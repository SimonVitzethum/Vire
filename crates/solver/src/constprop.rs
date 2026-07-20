//! Sparse constant propagation of scalar locals.
//!
//! A local whose *only* definition is `Assign(l, Use(Const))` in the entry block
//! (block 0, which — when it has no predecessor — dominates every use) is
//! replaced by that constant at all use sites. The big lever: a divisor that was
//! a runtime local (`mut vn = 200000; … x % vn`) becomes a literal, so the
//! backend emits a native `srem`/magic-multiply instead of a `jrt_lrem` runtime
//! call (see `emit_binary`'s `const_nonzero`). Also turns variable array sizes
//! and loop bounds into constants for downstream passes.
//!
//! Soundness: only single-assignment (`defcount == 1`) constants located in a
//! predecessor-free entry block are propagated — that assignment dominates the
//! whole function and re-defines nothing, so substituting its value is exact. No
//! constant folding is performed (leaving `Binary(Const, Const)` in place is
//! always as safe as the original and keeps Vire's checked-overflow trapping).

use fastllvm_ir::*;

/// Apply `f` to every input operand of a statement (definitions excluded).
fn stmt_operands_mut(st: &mut Statement, mut f: impl FnMut(&mut Operand)) {
    match st {
        Statement::Assign(_, rv) => match rv {
            Rvalue::Use(o) | Rvalue::Neg(o) | Rvalue::Convert(o) => f(o),
            Rvalue::Binary(_, a, b) => {
                f(a);
                f(b);
            }
        },
        Statement::Call { args, .. }
        | Statement::CallGuarded { args, .. }
        | Statement::CallVirtual { args, .. }
        | Statement::CallPoly { args, .. } => {
            for a in args {
                f(a);
            }
        }
        Statement::GetField { obj, .. } => f(obj),
        Statement::PutField { obj, value, .. } => {
            f(obj);
            f(value);
        }
        Statement::PutStatic { value, .. } => f(value),
        Statement::CheckCast { obj, .. } => f(obj),
        Statement::InstanceOf { obj, .. } => f(obj),
        Statement::NewArray { len, .. } | Statement::RegionNewArray { len, .. } => f(len),
        Statement::ArrayLen { arr, .. } => f(arr),
        Statement::ArrayLoad { arr, index, .. } => {
            f(arr);
            f(index);
        }
        Statement::ArrayStore { arr, index, value, .. } => {
            f(arr);
            f(index);
            f(value);
        }
        Statement::New { .. }
        | Statement::StackNew { .. }
        | Statement::StackNewArray { .. }
        | Statement::GetStatic { .. }
        | Statement::InstanceOfPending { .. }
        | Statement::DebugLine(_) => {}
    }
}

fn term_operands_mut(t: &mut Terminator, mut f: impl FnMut(&mut Operand)) {
    match t {
        Terminator::Branch { cond, .. } => f(cond),
        Terminator::Switch { value, .. } => f(value),
        Terminator::Return(Some(o)) => f(o),
        _ => {}
    }
}

/// The local a statement defines (writes), if any.
fn def_local(st: &Statement) -> Option<Local> {
    match st {
        Statement::Assign(d, _) => Some(*d),
        Statement::Call { dest, .. }
        | Statement::CallGuarded { dest, .. }
        | Statement::CallVirtual { dest, .. }
        | Statement::CallPoly { dest, .. } => *dest,
        Statement::New { dest, .. }
        | Statement::StackNew { dest, .. }
        | Statement::GetField { dest, .. }
        | Statement::GetStatic { dest, .. }
        | Statement::InstanceOfPending { dest, .. }
        | Statement::InstanceOf { dest, .. }
        | Statement::NewArray { dest, .. }
        | Statement::StackNewArray { dest, .. }
        | Statement::RegionNewArray { dest, .. }
        | Statement::ArrayLen { dest, .. }
        | Statement::ArrayLoad { dest, .. } => Some(*dest),
        Statement::PutField { .. }
        | Statement::PutStatic { .. }
        | Statement::CheckCast { .. }
        | Statement::ArrayStore { .. }
        | Statement::DebugLine(_) => None,
    }
}

fn as_scalar_const(o: &Operand) -> Option<Operand> {
    match o {
        Operand::ConstI32(_) | Operand::ConstI64(_) | Operand::ConstF32(_) | Operand::ConstF64(_) => Some(o.clone()),
        _ => None,
    }
}

/// Propagate entry-block scalar constants to their uses. Returns the number of
/// operands rewritten.
pub fn propagate_const_scalars(program: &mut Program) -> usize {
    let mut total = 0;
    for f in &mut program.functions {
        total += prop_fn(f);
    }
    total
}

/// Dominator sets: `dom[b]` = blocks that dominate `b` (block 0 is the entry).
/// Standard iterative dataflow over a reducible CFG; converges quickly.
fn dominators(f: &Function) -> Vec<std::collections::HashSet<usize>> {
    let n = f.blocks.len();
    let succ = |bb: &BasicBlock| -> Vec<usize> {
        match &bb.terminator {
            Terminator::Goto(b) => vec![b.0 as usize],
            Terminator::Branch { then_blk, else_blk, .. } => vec![then_blk.0 as usize, else_blk.0 as usize],
            Terminator::Switch { default, cases, .. } => {
                let mut v: Vec<usize> = cases.iter().map(|(_, b)| b.0 as usize).collect();
                v.push(default.0 as usize);
                v
            }
            Terminator::Return(_) => vec![],
        }
    };
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (u, bb) in f.blocks.iter().enumerate() {
        for v in succ(bb) {
            if v < n {
                preds[v].push(u);
            }
        }
    }
    let all: std::collections::HashSet<usize> = (0..n).collect();
    let mut dom: Vec<std::collections::HashSet<usize>> = vec![all.clone(); n];
    dom[0] = std::iter::once(0).collect();
    loop {
        let mut changed = false;
        for b in 1..n {
            let mut new: Option<std::collections::HashSet<usize>> = None;
            for &p in &preds[b] {
                new = Some(match new {
                    None => dom[p].clone(),
                    Some(acc) => acc.intersection(&dom[p]).copied().collect(),
                });
            }
            let mut new = new.unwrap_or_default();
            new.insert(b);
            if new != dom[b] {
                dom[b] = new;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    dom
}

fn prop_fn(f: &mut Function) -> usize {
    if f.blocks.is_empty() {
        return 0;
    }
    // Definition count per local across the whole function, and the (block, index)
    // of each local's single definition (if it is `Assign(l, Use(const))`).
    let mut defcount = vec![0u32; f.locals.len()];
    let mut def_site: std::collections::HashMap<u32, (usize, usize, Operand)> = std::collections::HashMap::new();
    for (bi, bb) in f.blocks.iter().enumerate() {
        for (si, st) in bb.statements.iter().enumerate() {
            if let Some(d) = def_local(st) {
                defcount[d.0 as usize] += 1;
                if let Statement::Assign(_, Rvalue::Use(o)) = st {
                    if let Some(c) = as_scalar_const(o) {
                        def_site.insert(d.0, (bi, si, c));
                        continue;
                    }
                }
                def_site.remove(&d.0);
            }
        }
    }
    // Candidate constants: exactly one definition, and it is a constant `Assign`.
    let mut candidates: Vec<u32> = def_site
        .keys()
        .copied()
        .filter(|l| defcount[*l as usize] == 1)
        .collect();
    if candidates.is_empty() {
        return 0;
    }
    // Collect every use site (block, statement index; usize::MAX = terminator).
    let mut uses: std::collections::HashMap<u32, Vec<(usize, usize)>> = std::collections::HashMap::new();
    for (bi, bb) in f.blocks.iter().enumerate() {
        for (si, st) in bb.statements.iter().enumerate() {
            let mut clone = st.clone();
            stmt_operands_mut(&mut clone, |op| {
                if let Operand::Copy(l) = op {
                    uses.entry(l.0).or_default().push((bi, si));
                }
            });
        }
        let mut t = bb.terminator.clone();
        term_operands_mut(&mut t, |op| {
            if let Operand::Copy(l) = op {
                uses.entry(l.0).or_default().push((bi, usize::MAX));
            }
        });
    }
    let dom = dominators(f);
    // Keep a candidate only if its definition dominates every use: the def block
    // dominates the use block, and for a use in the same block the def precedes it.
    candidates.retain(|&l| {
        let (db, ds, _) = def_site[&l];
        uses.get(&l).map(|us| {
            us.iter().all(|&(ub, us_idx)| {
                if ub == db {
                    us_idx == usize::MAX || us_idx > ds
                } else {
                    dom[ub].contains(&db)
                }
            })
        }).unwrap_or(true)
    });
    if candidates.is_empty() {
        return 0;
    }
    let const_val: std::collections::HashMap<u32, Operand> =
        candidates.iter().map(|&l| (l, def_site[&l].2.clone())).collect();
    let mut n = 0;
    for bb in &mut f.blocks {
        for st in &mut bb.statements {
            stmt_operands_mut(st, |op| {
                if let Operand::Copy(l) = op {
                    if let Some(c) = const_val.get(&l.0) {
                        *op = c.clone();
                        n += 1;
                    }
                }
            });
        }
        term_operands_mut(&mut bb.terminator, |op| {
            if let Operand::Copy(l) = op {
                if let Some(c) = const_val.get(&l.0) {
                    *op = c.clone();
                    n += 1;
                }
            }
        });
    }
    n
}
