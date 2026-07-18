//! Elimination of dead pending-exception checks.
//!
//! The exception model inserts a `jrt_pending_set` + branch after every
//! potentially throwing call (handler/propagation vs. continuation). If
//! the preceding call provably does *not* throw, the check is dead — a
//! `call jrt_pending_set` per iteration that Rust does not have. This analysis
//! removes it.
//!
//! Throw-freedom (fixpoint): a function never sets a pending exception
//! if no statement can — field accesses only on provably non-null
//! objects (New results), calls only to throw-free functions or known
//! harmless runtime helpers. Conservative: when in doubt "can throw" (then the
//! check stays — correct, just an unused optimization).

use std::collections::{BTreeMap, BTreeSet};

use fastllvm_ir::*;

/// Removes dead pending checks. Returns the number of removed checks.
pub fn elide_pending_checks(program: &mut Program) -> usize {
    // Instance methods: there local 0 = `this` and never null (the caller
    // checks the receiver, or `this` comes from `new`).
    let instance: BTreeSet<String> = program
        .classes
        .iter()
        .flat_map(|c| c.methods.iter().filter(|m| !m.is_static).map(|m| m.mangled.clone()))
        .collect();
    let throw_free = compute_throw_free(&program.functions, &instance);
    let mut removed = 0;
    for f in &mut program.functions {
        let nn = non_null_locals(f, instance.contains(&f.name));
        removed += elide_in_function(f, &throw_free, &nn);
    }
    removed
}

/// Known non-throwing runtime helpers (never set `pending`).
fn runtime_safe(func: &str) -> bool {
    // explicitly exclude throwing helpers; everything else jrt_* counts as safe
    // if it is not listed here as throwing.
    const THROWS: &[&str] = &[
        "jrt_throw",
        "jrt_null_check",
        "jrt_idiv",
        "jrt_irem",
        "jrt_ldiv",
        "jrt_lrem",
        "jrt_iaload",
        "jrt_iastore",
        "jrt_aaload",
        "jrt_aastore",
        "jrt_arraylen",
        "jrt_bounds_check",
        "jrt_str_length",
        "jrt_str_char_at",
        "jrt_str_indexof",
        "jrt_str_startswith",
        "jrt_str_endswith",
        "jrt_str_compareto",
        "jrt_str_substring1",
        "jrt_str_substring2",
        "jrt_str_trim",
        "jrt_arraycopy",
        "jrt_array_clone",
        "jrt_enum_valueof",
        "jrt_parse_int",
        "jrt_parse_long",
        "jrt_throwable_message",
        "jrt_get_class",
        "jrt_class_getname",
        "jrt_class_getsimplename",
        "jrt_checkcast",
        "jrt_thread_start",
        "jrt_thread_join",
        "jrt_invoke_runnable",
    ];
    func.starts_with("jrt_") && !THROWS.contains(&func)
}

/// Provably non-null locals: `this` (in instance methods), New/StackNew
/// results, and copies thereof.
fn non_null_locals(f: &Function, is_instance: bool) -> BTreeSet<u32> {
    let mut nn = BTreeSet::new();
    if is_instance {
        nn.insert(0); // this
    }
    for bb in &f.blocks {
        for st in &bb.statements {
            if let Statement::New { dest, .. } | Statement::StackNew { dest, .. } = st {
                nn.insert(dest.0);
            }
        }
    }
    // Copies: fixpoint over Assign(d, Copy(s)).
    loop {
        let before = nn.len();
        for bb in &f.blocks {
            for st in &bb.statements {
                if let Statement::Assign(d, Rvalue::Use(Operand::Copy(s))) = st {
                    if nn.contains(&s.0) {
                        nn.insert(d.0);
                    }
                }
            }
        }
        // A reassignment of a non-nn value would invalidate d; since we
        // work flow-insensitively, we remove d again if it also has a
        // non-nn def.
        if nn.len() == before {
            break;
        }
    }
    // Conservative: drop again locals with any possibly-null def.
    let mut maybe_null = BTreeSet::new();
    for bb in &f.blocks {
        for st in &bb.statements {
            let (def, ok) = match st {
                Statement::New { dest, .. } | Statement::StackNew { dest, .. } => (Some(dest.0), true),
                Statement::Assign(d, Rvalue::Use(Operand::Copy(s))) => (Some(d.0), nn.contains(&s.0)),
                Statement::Assign(d, _) => (Some(d.0), false),
                Statement::GetField { dest, .. }
                | Statement::GetStatic { dest, .. }
                | Statement::ArrayLoad { dest, .. }
                | Statement::NewArray { dest, .. } => (Some(dest.0), false),
                Statement::Call { dest, .. }
                | Statement::CallGuarded { dest, .. }
                | Statement::CallVirtual { dest, .. }
                | Statement::CallPoly { dest, .. } => (dest.map(|d| d.0), false),
                _ => (None, false),
            };
            if let Some(d) = def {
                if !ok {
                    maybe_null.insert(d);
                }
            }
        }
    }
    nn.retain(|l| !maybe_null.contains(l));
    nn
}

/// Can this statement set a pending exception?
fn can_throw(st: &Statement, throw_free: &BTreeMap<String, bool>, non_null: &BTreeSet<u32>) -> bool {
    let call_throws = |func: &str, recv: Option<&Operand>| -> bool {
        // User function: per summary. Runtime: per list. Otherwise conservative.
        let target_ok = throw_free.get(func).copied().unwrap_or(false) || runtime_safe(func);
        if !target_ok {
            return true;
        }
        // CallGuarded checks the receiver for null → can NPE, unless non-null.
        if let Some(Operand::Copy(l)) = recv {
            !non_null.contains(&l.0)
        } else {
            recv.is_some() && !matches!(recv, Some(Operand::Copy(_)))
        }
    };
    let obj_may_null = |op: &Operand| !matches!(op, Operand::Copy(l) if non_null.contains(&l.0));
    match st {
        Statement::Call { func, .. } => !(throw_free.get(func).copied().unwrap_or(false) || runtime_safe(func)),
        Statement::CallGuarded { func, args, .. } => call_throws(func, args.first()),
        Statement::CallVirtual { .. } | Statement::CallPoly { .. } => true,
        Statement::GetField { obj, .. } => obj_may_null(obj),
        Statement::PutField { obj, .. } => obj_may_null(obj),
        // Bounds-proven (unchecked) accesses are throw-free.
        Statement::ArrayLoad { checked, .. } | Statement::ArrayStore { checked, .. } => *checked,
        Statement::ArrayLen { .. } => true,
        Statement::InstanceOfPending { .. } | Statement::CheckCast { .. } => true,
        _ => false,
    }
}

/// Throw-freedom fixpoint.
fn compute_throw_free(functions: &[Function], instance: &BTreeSet<String>) -> BTreeMap<String, bool> {
    let mut tf: BTreeMap<String, bool> = functions.iter().map(|f| (f.name.clone(), true)).collect();
    let non_null: BTreeMap<String, BTreeSet<u32>> = functions
        .iter()
        .map(|f| (f.name.clone(), non_null_locals(f, instance.contains(&f.name))))
        .collect();
    loop {
        let mut changed = false;
        for f in functions {
            if !tf[&f.name] {
                continue;
            }
            let nn = &non_null[&f.name];
            let throws = f
                .blocks
                .iter()
                .flat_map(|b| &b.statements)
                .any(|st| can_throw(st, &tf, nn));
            if throws {
                tf.insert(f.name.clone(), false);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    tf
}

/// Removes, in a function, the pending checks whose block cannot throw.
/// Structure per check block: … [throwing op] ; `Call jrt_pending_set → c` ;
/// `Branch{c, exc, cont}`. If no op in the block throws → `Goto(cont)`.
fn elide_in_function(
    f: &mut Function,
    throw_free: &BTreeMap<String, bool>,
    non_null: &BTreeSet<u32>,
) -> usize {
    let mut removed = 0;
    for bb in &mut f.blocks {
        // Recognize the pattern: last statement is jrt_pending_set, terminator is
        // a branch on its dest.
        let Some(Statement::Call { dest: Some(c), func, .. }) = bb.statements.last() else {
            continue;
        };
        if func != "jrt_pending_set" {
            continue;
        }
        let Terminator::Branch { cond: Operand::Copy(cc), else_blk, .. } = &bb.terminator else {
            continue;
        };
        if cc.0 != c.0 {
            continue;
        }
        let cont = *else_blk;
        // Can any statement (other than the pending check itself) throw?
        let n = bb.statements.len();
        let throws = bb.statements[..n - 1]
            .iter()
            .any(|st| can_throw(st, throw_free, &non_null));
        if throws {
            continue;
        }
        bb.statements.pop(); // remove jrt_pending_set
        bb.terminator = Terminator::Goto(cont);
        removed += 1;
    }
    removed
}
