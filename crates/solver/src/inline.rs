//! Inliner on the mid-level IR.
//!
//! Runs after devirtualization so that devirtualized sites become inlinable
//! — inlining then unlocks everything else on the LLVM side
//! (DESIGN.md §4). Conservative: only direct calls, only small, non-
//! self-recursive targets.

use std::collections::BTreeMap;

use fastllvm_ir::*;

/// Upper bound on statements up to which a function is inlined.
const SIZE_LIMIT: usize = 16;
/// Upper bound on inlining operations per caller (against code explosion
/// from chains of small functions).
const PER_CALLER_LIMIT: usize = 64;

pub fn inline_program(program: &mut Program) -> usize {
    let size = |f: &Function| f.blocks.iter().map(|b| b.statements.len()).sum::<usize>();
    let calls_self = |f: &Function| {
        f.blocks.iter().flat_map(|b| &b.statements).any(|st| {
            matches!(st, Statement::Call { func, .. } | Statement::CallGuarded { func, .. }
                if *func == f.name)
        })
    };
    // Exception flow in the callee is inlinable: every call site has a
    // pending check after it (throw_after), so an exception propagating out
    // of the inlined body is detected in the continuation block; an
    // internal try/catch handler is self-contained anyway.

    // Copy candidates so callers stay mutable.
    let candidates: BTreeMap<String, Function> = program
        .functions
        .iter()
        .filter(|f| size(f) <= SIZE_LIMIT && !calls_self(f))
        .map(|f| (f.name.clone(), f.clone()))
        .collect();

    let mut total = 0;
    for f in &mut program.functions {
        let mut budget = PER_CALLER_LIMIT;
        while budget > 0 {
            let Some((blk, idx)) = find_call_site(f, &candidates) else { break };
            splice(f, blk, idx, &candidates);
            budget -= 1;
            total += 1;
        }
    }
    total
}

fn find_call_site(f: &Function, candidates: &BTreeMap<String, Function>) -> Option<(usize, usize)> {
    for (bi, bb) in f.blocks.iter().enumerate() {
        for (si, st) in bb.statements.iter().enumerate() {
            let func = match st {
                Statement::Call { func, .. } | Statement::CallGuarded { func, .. } => func,
                _ => continue,
            };
            if candidates.contains_key(func) && *func != f.name {
                return Some((bi, si));
            }
        }
    }
    None
}

/// Replaces the (possibly guarded) call in block `blk` at index `idx` with the
/// callee's body: the block is split at the call, callee blocks (with
/// renumbered locals and blocks) are appended, returns are redirected to the
/// continuation block. For `CallGuarded`, a null check of the
/// receiver is prepended as a guard (the catchable NPE is preserved).
fn splice(f: &mut Function, blk: usize, idx: usize, candidates: &BTreeMap<String, Function>) {
    let (dest, func, args, guarded) = match f.blocks[blk].statements[idx].clone() {
        Statement::Call { dest, func, args } => (dest, func, args, false),
        Statement::CallGuarded { dest, func, args } => (dest, func, args, true),
        _ => unreachable!(),
    };
    let callee = &candidates[&func];

    // Inline context for debug info: the call-site line in this caller. Appended
    // to every DebugLine of the inlined body so a crash shows the caller chain
    // (DWARF inlinedAt). The last DebugLine before the call gives the line.
    let call_line = f.blocks[blk].statements[..idx]
        .iter()
        .rev()
        .find_map(|st| if let Statement::DebugLine(fr) = st { fr.first().map(|(_, l)| *l) } else { None })
        .unwrap_or(0);
    let ctx = (f.name.clone(), call_line);

    let local_off = f.locals.len() as u32;
    f.locals.extend(callee.locals.iter().copied());
    // For a guarded call: two additional synthetic blocks (npe, arg)
    // before the callee blocks; otherwise the callee blocks start directly.
    let extra = if guarded { 2u32 } else { 0 };
    let block_off = f.blocks.len() as u32;
    let callee_first = block_off + extra;
    let cont_block = Block(callee_first + callee.blocks.len() as u32);

    let tail: Vec<Statement> = f.blocks[blk].statements.split_off(idx + 1);
    f.blocks[blk].statements.pop(); // the (guarded) call itself

    if guarded {
        // Caller block: receiver == null? → npe block, otherwise arg block.
        let cmp = f.locals.len() as u32;
        f.locals.push(Ty::I32);
        f.blocks[blk].statements.push(Statement::Assign(
            Local(cmp),
            Rvalue::Binary(BinOp::CmpEq, args[0].clone(), Operand::ConstNull),
        ));
        let cont_term = std::mem::replace(
            &mut f.blocks[blk].terminator,
            Terminator::Branch {
                cond: Operand::Copy(Local(cmp)),
                then_blk: Block(block_off),     // npe
                else_blk: Block(block_off + 1), // arg
            },
        );
        // npe block.
        f.blocks.push(BasicBlock {
            statements: vec![Statement::Call { dest: None, func: "jrt_throw_npe".into(), args: vec![] }],
            terminator: Terminator::Goto(cont_block),
        });
        // arg block: argument assignments, then into the callee.
        let arg_assigns = args
            .into_iter()
            .enumerate()
            .map(|(k, arg)| Statement::Assign(Local(local_off + k as u32), Rvalue::Use(arg)))
            .collect();
        f.blocks.push(BasicBlock { statements: arg_assigns, terminator: Terminator::Goto(Block(callee_first)) });
        splice_callee(f, callee, dest, local_off, callee_first, cont_block, &ctx);
        f.blocks.push(BasicBlock { statements: tail, terminator: cont_term });
        return;
    }

    // Unguarded call: argument assignments directly in the caller block.
    for (k, arg) in args.into_iter().enumerate() {
        f.blocks[blk]
            .statements
            .push(Statement::Assign(Local(local_off + k as u32), Rvalue::Use(arg)));
    }
    let cont_term = std::mem::replace(&mut f.blocks[blk].terminator, Terminator::Goto(Block(callee_first)));
    splice_callee(f, callee, dest, local_off, callee_first, cont_block, &ctx);
    f.blocks.push(BasicBlock { statements: tail, terminator: cont_term });
}

/// Appends the (renumbered) callee blocks; returns become jumps
/// to the continuation block (with assignment of the return value to `dest`).
fn splice_callee(
    f: &mut Function,
    callee: &Function,
    dest: Option<Local>,
    local_off: u32,
    block_off: u32,
    cont_block: Block,
    ctx: &(String, u32),
) {
    for cb in &callee.blocks {
        let mut statements: Vec<Statement> = cb.statements.clone();
        for st in &mut statements {
            remap_statement(st, local_off);
            // DWARF inlinedAt: record that this line is inlined at the call site.
            if let Statement::DebugLine(frames) = st {
                frames.push(ctx.clone());
            }
        }
        let terminator = match &cb.terminator {
            Terminator::Goto(b) => Terminator::Goto(Block(b.0 + block_off)),
            Terminator::Branch { cond, then_blk, else_blk } => {
                let mut cond = cond.clone();
                remap_operand(&mut cond, local_off);
                Terminator::Branch {
                    cond,
                    then_blk: Block(then_blk.0 + block_off),
                    else_blk: Block(else_blk.0 + block_off),
                }
            }
            Terminator::Switch { value, default, cases } => {
                let mut value = value.clone();
                remap_operand(&mut value, local_off);
                Terminator::Switch {
                    value,
                    default: Block(default.0 + block_off),
                    cases: cases.iter().map(|(k, b)| (*k, Block(b.0 + block_off))).collect(),
                }
            }
            Terminator::Return(op) => {
                if let (Some(d), Some(op)) = (dest, op.as_ref()) {
                    let mut op = op.clone();
                    remap_operand(&mut op, local_off);
                    statements.push(Statement::Assign(d, Rvalue::Use(op)));
                }
                Terminator::Goto(cont_block)
            }
        };
        f.blocks.push(BasicBlock { statements, terminator });
    }
}

fn remap_local(l: &mut Local, off: u32) {
    l.0 += off;
}

fn remap_operand(op: &mut Operand, off: u32) {
    if let Operand::Copy(l) = op {
        remap_local(l, off);
    }
}

fn remap_rvalue(rv: &mut Rvalue, off: u32) {
    match rv {
        Rvalue::Use(op) | Rvalue::Neg(op) | Rvalue::Convert(op) => remap_operand(op, off),
        Rvalue::Binary(_, a, b) => {
            remap_operand(a, off);
            remap_operand(b, off);
        }
    }
}

fn remap_statement(st: &mut Statement, off: u32) {
    match st {
        Statement::Assign(l, rv) => {
            remap_local(l, off);
            remap_rvalue(rv, off);
        }
        Statement::Call { dest, args, .. }
        | Statement::CallGuarded { dest, args, .. }
        | Statement::CallVirtual { dest, args, .. }
        | Statement::CallPoly { dest, args, .. } => {
            if let Some(d) = dest {
                remap_local(d, off);
            }
            for a in args {
                remap_operand(a, off);
            }
        }
        Statement::New { dest, .. } | Statement::StackNew { dest, .. } | Statement::StackNewArray { dest, .. } => remap_local(dest, off),
        Statement::GetField { dest, obj, .. } => {
            remap_local(dest, off);
            remap_operand(obj, off);
        }
        Statement::PutField { obj, value, .. } => {
            remap_operand(obj, off);
            remap_operand(value, off);
        }
        Statement::NewArray { dest, len, .. } | Statement::RegionNewArray { dest, len, .. } => {
            remap_local(dest, off);
            remap_operand(len, off);
        }
        Statement::ArrayLen { dest, arr } => {
            remap_local(dest, off);
            remap_operand(arr, off);
        }
        Statement::ArrayLoad { dest, arr, index, .. } => {
            remap_local(dest, off);
            remap_operand(arr, off);
            remap_operand(index, off);
        }
        Statement::ArrayStore { arr, index, value, .. } => {
            remap_operand(arr, off);
            remap_operand(index, off);
            remap_operand(value, off);
        }
        Statement::GetStatic { dest, .. } => remap_local(dest, off),
        Statement::PutStatic { value, .. } => remap_operand(value, off),
        Statement::InstanceOfPending { dest, .. } => remap_local(dest, off),
        Statement::CheckCast { obj, .. } => remap_operand(obj, off),
        Statement::InstanceOf { dest, obj, .. } => {
            remap_local(dest, off);
            remap_operand(obj, off);
        }
        // No locals — the inlined callee's source line rides along unchanged.
        Statement::DebugLine(_) => {}
    }
}
