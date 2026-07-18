//! Escape analysis (Choi et al. 1999, heavily simplified): objects that
//! provably never leave their function are stack-allocated (`StackNew`).
//!
//! This is the first memory-safety/ownership building block (DESIGN.md
//! §6a): a non-escaping object has exactly one owner — the
//! stack frame — and a statically proven lifetime, like a Rust value.
//! Runs after devirtualization + inlining: only through the inlining of
//! the constructors does the receiver store become visible instead of an
//! escaping call argument (synergy from DESIGN.md §4).
//!
//! Conservative escape sources:
//! - return (`Return`) of an alias
//! - argument of a call (except `jrt_null_check`) or virtual calls
//! - stored as a *value* in `putfield` (stores *into* the object are ok)
//!
//! Stack allocation only outside of loops: otherwise the alloca slot
//! would be reused across iterations while aliases from earlier
//! iterations could still be alive.

use std::collections::{BTreeMap, BTreeSet};

use fastllvm_ir::*;

pub fn stack_allocate(program: &mut Program) -> usize {
    // Interprocedural escape summaries: which ref parameters of each function
    // let their caller's object escape. This means an object passed to a call
    // no longer has to be blindly considered escaping — only when the callee
    // actually holds onto it. Precision boost (phase 5) → more stack allocation.
    let summaries = compute_param_summaries(&program.functions);
    // Classes with (inherited) ref fields — for the leak safety of the
    // interprocedural relaxation (the callee could write heap refs into them).
    let ref_field_classes = classes_with_ref_fields(&program.classes);
    let mut total = 0;
    for f in &mut program.functions {
        total += run_function(f, &summaries, &ref_field_classes);
    }
    total
}

/// Classes whose instances (including inherited fields) have at least one
/// ref field.
fn classes_with_ref_fields(classes: &[ClassInfo]) -> BTreeSet<String> {
    let class_of = |n: &str| classes.iter().find(|c| c.name == n);
    let mut out = BTreeSet::new();
    for c in classes {
        let mut cur = Some(c.name.clone());
        let mut guard = 0;
        while let Some(cn) = cur {
            guard += 1;
            if guard > 10_000 {
                break;
            }
            let Some(ci) = class_of(&cn) else { break };
            if ci.fields.iter().any(|f| f.ty == Ty::Ref) {
                out.insert(c.name.clone());
                break;
            }
            cur = ci.super_name.clone();
        }
    }
    out
}

/// Does argument `j` escape to the callee `func`? `jrt_null_check` never; known
/// functions per summary; external/runtime functions conservatively yes.
fn arg_escapes(func: &str, j: usize, summ: &BTreeMap<String, Vec<bool>>) -> bool {
    if func == "jrt_null_check" {
        return false;
    }
    match summ.get(func) {
        Some(s) => s.get(j).copied().unwrap_or(true),
        None => true,
    }
}

/// Does argument `j` escape to any of the (known) targets of a polymorphic
/// call? Only if it escapes at NO target is it safely local.
fn poly_arg_escapes(targets: &[(String, String)], j: usize, summ: &BTreeMap<String, Vec<bool>>) -> bool {
    targets.iter().any(|(_, sym)| arg_escapes(sym, j, summ))
}

/// Fixpoint over the call graph: for each function the ref parameters that
/// escape (return / field/static/array store / forwarding to a call
/// that lets them escape / virtual call with unknown target).
fn compute_param_summaries(functions: &[Function]) -> BTreeMap<String, Vec<bool>> {
    let mut summ: BTreeMap<String, Vec<bool>> = functions
        .iter()
        .map(|f| (f.name.clone(), vec![false; f.params.len()]))
        .collect();
    loop {
        let mut changed = false;
        for f in functions {
            for i in 0..f.params.len() {
                if f.params[i] != Ty::Ref || summ[&f.name][i] {
                    continue;
                }
                if param_escapes(f, Local(i as u32), &summ) {
                    summ.get_mut(&f.name).unwrap()[i] = true;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    summ
}

fn param_escapes(f: &Function, root: Local, summ: &BTreeMap<String, Vec<bool>>) -> bool {
    let aliases = alias_set(f, root);
    let is_alias = |op: &Operand| matches!(op, Operand::Copy(l) if aliases.contains(l));
    for bb in &f.blocks {
        for st in &bb.statements {
            match st {
                Statement::Call { func, args, .. } | Statement::CallGuarded { func, args, .. } => {
                    for (j, a) in args.iter().enumerate() {
                        if is_alias(a) && arg_escapes(func, j, summ) {
                            return true;
                        }
                    }
                }
                Statement::CallPoly { args, targets, .. } => {
                    for (j, a) in args.iter().enumerate() {
                        if is_alias(a) && poly_arg_escapes(targets, j, summ) {
                            return true;
                        }
                    }
                }
                Statement::CallVirtual { args, .. } => {
                    if args.iter().any(is_alias) {
                        return true;
                    }
                }
                Statement::PutField { value, .. }
                | Statement::PutStatic { value, .. }
                | Statement::ArrayStore { value, .. } => {
                    if is_alias(value) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        if let Terminator::Return(Some(op)) = &bb.terminator {
            if is_alias(op) {
                return true;
            }
        }
    }
    false
}

fn run_function(
    f: &mut Function,
    summ: &BTreeMap<String, Vec<bool>>,
    ref_field_classes: &BTreeSet<String>,
) -> usize {
    let cyclic = cyclic_blocks(f);

    // Objects = allocation sites. Position (bi, si) + target local + class.
    let news: Vec<(usize, usize, Local, String)> = f
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(bi, bb)| {
            bb.statements.iter().enumerate().filter_map(move |(si, st)| match st {
                Statement::New { dest, class } => Some((bi, si, *dest, class.clone())),
                _ => None,
            })
        })
        .collect();
    if news.is_empty() {
        return 0;
    }

    // Alias set per object (flow-insensitive copy fixpoint; due to
    // local-slot reuse conservatively over-estimated → only more escapes).
    let aliases: Vec<BTreeSet<Local>> = news.iter().map(|(_, _, d, _)| alias_set(f, *d)).collect();
    // Objects that an operand can reference.
    let objs_of = |op: &Operand| -> Vec<usize> {
        match op {
            Operand::Copy(l) => (0..news.len()).filter(|&i| aliases[i].contains(l)).collect(),
            _ => Vec::new(),
        }
    };

    // direct[o] = o escapes immediately; edges = undirected edges between
    // objects connected via a field (both-or-neither: container and
    // content are promoted only together). This way a stack container holds
    // exclusively immortal contents → no field release/leak possible.
    let n = news.len();
    let mut direct = vec![false; n];
    let mut edges: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    let mark = |set: &mut Vec<bool>, op: &Operand| {
        for oi in objs_of(op) {
            set[oi] = true;
        }
    };
    let is_ref_operand = |op: &Operand| matches!(op, Operand::Copy(l) if f.locals[l.0 as usize] == Ty::Ref);

    for bb in &f.blocks {
        for st in &bb.statements {
            match st {
                // Call arguments escape only if the callee holds onto them per
                // summary (interprocedural); direct + devirtualized
                // calls have a known target.
                Statement::Call { func, args, .. } | Statement::CallGuarded { func, args, .. } => {
                    for (j, a) in args.iter().enumerate() {
                        let esc = arg_escapes(func, j, summ);
                        for oi in objs_of(a) {
                            // Leak safety: the callee could write a heap ref into
                            // a ref field of O (invisible to us) —
                            // an O with ref fields that goes to a real call
                            // must therefore stay on the heap.
                            if esc || (func != "jrt_null_check" && ref_field_classes.contains(&news[oi].3)) {
                                direct[oi] = true;
                            }
                        }
                    }
                }
                // Polymorphic call with known targets: escapes only if it
                // escapes at at least one target (interprocedural). Leak
                // safety as with direct calls (ref-field objects → heap).
                Statement::CallPoly { args, targets, .. } => {
                    for (j, a) in args.iter().enumerate() {
                        let esc = poly_arg_escapes(targets, j, summ);
                        for oi in objs_of(a) {
                            if esc || ref_field_classes.contains(&news[oi].3) {
                                direct[oi] = true;
                            }
                        }
                    }
                }
                // Virtual call with unknown target → conservatively escaping.
                Statement::CallVirtual { args, .. } => {
                    for a in args {
                        mark(&mut direct, a);
                    }
                }
                Statement::PutStatic { value, .. } | Statement::ArrayStore { value, .. } => {
                    mark(&mut direct, value);
                }
                // Field sensitivity, `obj.field = value`:
                //  - value tracked, obj tracked  → undirected edge value↔obj
                //  - value tracked, obj unknown → value escapes (stored in a
                //    foreign container)
                //  - value unknown ref, obj tracked → obj escapes (an
                //    immortal stack container would otherwise hold a heap reference
                //    whose drop never runs → leak)
                Statement::PutField { obj, value, .. } => {
                    let vs = objs_of(value);
                    let os = objs_of(obj);
                    if !vs.is_empty() {
                        if os.is_empty() {
                            for ov in &vs {
                                direct[*ov] = true;
                            }
                        } else {
                            for &ov in &vs {
                                for &oo in &os {
                                    edges[ov].insert(oo);
                                    edges[oo].insert(ov);
                                }
                            }
                        }
                    } else if !os.is_empty() && is_ref_operand(value) {
                        for oo in &os {
                            direct[*oo] = true;
                        }
                    }
                }
                _ => {}
            }
        }
        if let Terminator::Return(Some(op)) = &bb.terminator {
            mark(&mut direct, op);
        }
    }

    // Loop safety (phase 3): an object in a cycle block may only be
    // stack-allocated (slot reused per iteration) if at the New
    // no alias from an earlier iteration is still alive. Otherwise it "escapes"
    // (stays on the heap). Treated as a direct escape source so the component
    // propagation below preserves the both-or-neither invariant: an unsafe
    // loop object drags its whole component onto the heap (prevents one
    // cycle partner being promoted while the other stays on the heap → dangling).
    if news.iter().any(|(bi, _, _, _)| cyclic[*bi]) {
        let live_in = liveness(f);
        for (idx, (bi, si, dest, _)) in news.iter().enumerate() {
            if cyclic[*bi] {
                let live = &live_in[*bi][*si];
                if aliases[idx].iter().any(|a| *a != *dest && live.contains(a)) {
                    direct[idx] = true;
                }
            }
        }
    }

    // Fixpoint: propagate escaping over the undirected edges — a
    // connected component escapes as soon as one member escapes.
    let mut escape = direct;
    loop {
        let mut changed = false;
        for a in 0..n {
            if !escape[a] && edges[a].iter().any(|&b| escape[b]) {
                escape[a] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Stack-allocate non-escaping objects (loop safety is already
    // in `escape`, see above).
    let mut count = 0;
    for (idx, (bi, si, _, _)) in news.iter().enumerate() {
        if escape[idx] {
            continue;
        }
        let Statement::New { dest, class } = f.blocks[*bi].statements[*si].clone() else {
            unreachable!()
        };
        f.blocks[*bi].statements[*si] = Statement::StackNew { dest, class };
        count += 1;
    }
    count
}

/// Backward liveness: `live_in[block][stmt]` = the locals alive before
/// statement `stmt` (in the block). Standard data flow (live-out = ∪ live-in of
/// the successors; live-in = use ∪ (live-out ∖ def)).
fn liveness(f: &Function) -> Vec<Vec<BTreeSet<Local>>> {
    let nb = f.blocks.len();
    let succs: Vec<Vec<usize>> = f
        .blocks
        .iter()
        .map(|bb| match &bb.terminator {
            Terminator::Goto(b) => vec![b.0 as usize],
            Terminator::Branch { then_blk, else_blk, .. } => vec![then_blk.0 as usize, else_blk.0 as usize],
            Terminator::Switch { default, cases, .. } => {
                let mut v = vec![default.0 as usize];
                v.extend(cases.iter().map(|(_, b)| b.0 as usize));
                v
            }
            Terminator::Return(_) => vec![],
        })
        .collect();
    let term_uses: Vec<BTreeSet<Local>> = f
        .blocks
        .iter()
        .map(|bb| {
            let mut u = BTreeSet::new();
            match &bb.terminator {
                Terminator::Branch { cond, .. } => add_use(&mut u, cond),
                Terminator::Switch { value, .. } => add_use(&mut u, value),
                Terminator::Return(Some(op)) => add_use(&mut u, op),
                _ => {}
            }
            u
        })
        .collect();
    let mut live_out_block = vec![BTreeSet::<Local>::new(); nb];
    // Fixpoint over live-out per block.
    loop {
        let mut changed = false;
        for bi in (0..nb).rev() {
            let mut out = BTreeSet::new();
            for &s in &succs[bi] {
                out.extend(block_live_in(f, s, &term_uses[s], &live_out_block[s]));
            }
            if out != live_out_block[bi] {
                live_out_block[bi] = out;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Resolve per statement backward.
    let mut result: Vec<Vec<BTreeSet<Local>>> = Vec::with_capacity(nb);
    for (bi, bb) in f.blocks.iter().enumerate() {
        let mut cur = live_out_block[bi].clone();
        cur.extend(term_uses[bi].iter().copied());
        let mut per_stmt = vec![BTreeSet::new(); bb.statements.len()];
        for si in (0..bb.statements.len()).rev() {
            let (def, uses) = stmt_def_use(&bb.statements[si]);
            if let Some(d) = def {
                cur.remove(&d);
            }
            cur.extend(uses);
            per_stmt[si] = cur.clone();
        }
        result.push(per_stmt);
    }
    result
}

/// Live-in of an entire block (for the block fixpoint iteration).
fn block_live_in(
    f: &Function,
    bi: usize,
    term_uses: &BTreeSet<Local>,
    live_out: &BTreeSet<Local>,
) -> BTreeSet<Local> {
    let mut cur = live_out.clone();
    cur.extend(term_uses.iter().copied());
    for st in f.blocks[bi].statements.iter().rev() {
        let (def, uses) = stmt_def_use(st);
        if let Some(d) = def {
            cur.remove(&d);
        }
        cur.extend(uses);
    }
    cur
}

fn add_use(set: &mut BTreeSet<Local>, op: &Operand) {
    if let Operand::Copy(l) = op {
        set.insert(*l);
    }
}

/// (defined local, used locals) of a statement.
fn stmt_def_use(st: &Statement) -> (Option<Local>, Vec<Local>) {
    let mut uses = Vec::new();
    let mut u = |op: &Operand| {
        if let Operand::Copy(l) = op {
            uses.push(*l);
        }
    };
    let def = match st {
        Statement::Assign(d, rv) => {
            match rv {
                Rvalue::Use(op) | Rvalue::Neg(op) | Rvalue::Convert(op) => u(op),
                Rvalue::Binary(_, a, b) => {
                    u(a);
                    u(b);
                }
            }
            Some(*d)
        }
        Statement::Call { dest, args, .. }
        | Statement::CallGuarded { dest, args, .. }
        | Statement::CallVirtual { dest, args, .. }
        | Statement::CallPoly { dest, args, .. } => {
            args.iter().for_each(&mut u);
            *dest
        }
        Statement::New { dest, .. } | Statement::StackNew { dest, .. } => Some(*dest),
        Statement::GetField { dest, obj, .. } => {
            u(obj);
            Some(*dest)
        }
        Statement::PutField { obj, value, .. } => {
            u(obj);
            u(value);
            None
        }
        Statement::GetStatic { dest, .. } => Some(*dest),
        Statement::PutStatic { value, .. } => {
            u(value);
            None
        }
        Statement::NewArray { dest, len, .. } => {
            u(len);
            Some(*dest)
        }
        Statement::ArrayLen { dest, arr } => {
            u(arr);
            Some(*dest)
        }
        Statement::ArrayLoad { dest, arr, index, .. } => {
            u(arr);
            u(index);
            Some(*dest)
        }
        Statement::ArrayStore { arr, index, value, .. } => {
            u(arr);
            u(index);
            u(value);
            None
        }
        Statement::InstanceOf { dest, obj, .. } => {
            u(obj);
            Some(*dest)
        }
        Statement::InstanceOfPending { dest, .. } => Some(*dest),
        Statement::CheckCast { obj, .. } => {
            u(obj);
            None
        }
    };
    (def, uses)
}

/// Alias fixpoint: all locals that can hold the value of `root`.
fn alias_set(f: &Function, root: Local) -> BTreeSet<Local> {
    let mut aliases: BTreeSet<Local> = BTreeSet::new();
    aliases.insert(root);
    loop {
        let before = aliases.len();
        for bb in &f.blocks {
            for st in &bb.statements {
                if let Statement::Assign(d, Rvalue::Use(Operand::Copy(s))) = st {
                    if aliases.contains(s) {
                        aliases.insert(*d);
                    }
                }
            }
        }
        if aliases.len() == before {
            break;
        }
    }
    aliases
}

/// Blocks that lie on a cycle (can reach themselves).
fn cyclic_blocks(f: &Function) -> Vec<bool> {
    let succs: Vec<Vec<usize>> = f
        .blocks
        .iter()
        .map(|bb| match &bb.terminator {
            Terminator::Goto(b) => vec![b.0 as usize],
            Terminator::Branch { then_blk, else_blk, .. } => {
                vec![then_blk.0 as usize, else_blk.0 as usize]
            }
            Terminator::Switch { default, cases, .. } => {
                let mut v = vec![default.0 as usize];
                v.extend(cases.iter().map(|(_, b)| b.0 as usize));
                v
            }
            Terminator::Return(_) => vec![],
        })
        .collect();
    (0..f.blocks.len())
        .map(|start| {
            // DFS from the successors; if they reach `start`, it lies in a cycle.
            let mut seen = vec![false; f.blocks.len()];
            let mut stack: Vec<usize> = succs[start].clone();
            while let Some(b) = stack.pop() {
                if b == start {
                    return true;
                }
                if !std::mem::replace(&mut seen[b], true) {
                    stack.extend(&succs[b]);
                }
            }
            false
        })
        .collect()
}
