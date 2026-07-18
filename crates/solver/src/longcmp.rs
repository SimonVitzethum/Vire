//! Fusion of long/double comparisons with their following 0-test.
//!
//! javac lowers `j < N` (long) as `lcmp; if<cond>` — in the bytecode a
//! standalone `lcmp` yielding `sign(x−y) ∈ {-1,0,1}`, followed by a
//! branch test against 0. The frontend maps this onto a `jrt_lcmp` call
//! plus `CmpX(result, 0)` — a **function call per iteration** in hot
//! loops that Rust handles as a native `icmp slt i64`.
//!
//! Since `sign(x−y) <op> 0 ⟺ x <op> y` holds for all six comparisons,
//! this peephole optimization replaces the pair with a direct `Binary(op, x, y)`
//! on the i64 operands (the backend emits `icmp <cc> i64`) and removes
//! the now-dead `jrt_lcmp` call. Conservative: only when the lcmp result slot
//! is read exclusively by the immediately following 0-test.

use fastllvm_ir::*;

pub fn fuse_long_compares(program: &mut Program) -> usize {
    let mut n = 0;
    for f in &mut program.functions {
        for bi in 0..f.blocks.len() {
            n += fuse_block(&mut f.blocks[bi]);
        }
    }
    n
}

fn is_cmp0(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::CmpEq | BinOp::CmpNe | BinOp::CmpLt | BinOp::CmpGe | BinOp::CmpGt | BinOp::CmpLe
    )
}

fn fuse_block(bb: &mut BasicBlock) -> usize {
    let mut fused = 0;
    let mut i = 0;
    while i + 1 < bb.statements.len() {
        // Pattern: Call(d, jrt_lcmp|dcmp, [x, y]) ; Assign(c, CmpX(Copy(d), 0)).
        let (d, x, y) = match &bb.statements[i] {
            Statement::Call { dest: Some(d), func, args }
                if (func == "jrt_lcmp") && args.len() == 2 =>
            {
                (d.0, args[0].clone(), args[1].clone())
            }
            _ => {
                i += 1;
                continue;
            }
        };
        let fuse = match &bb.statements[i + 1] {
            Statement::Assign(_, Rvalue::Binary(op, Operand::Copy(l), Operand::ConstI32(0)))
                if is_cmp0(*op) && l.0 == d =>
            {
                Some(*op)
            }
            _ => None,
        };
        let Some(op) = fuse else {
            i += 1;
            continue;
        };
        // d must not be read after the 0-test (up to the next
        // definition) and must not occur in the terminator.
        if reads_local_after(bb, i + 2, d) {
            i += 1;
            continue;
        }
        // Fusion: comparison directly on the i64 operands, remove the lcmp call.
        if let Statement::Assign(_, rv) = &mut bb.statements[i + 1] {
            *rv = Rvalue::Binary(op, x, y);
        }
        bb.statements.remove(i);
        fused += 1;
        // i now points at the (former) Assign; keep searching after it.
        i += 1;
    }
    fused
}

/// Is `local` read as an operand from `from` on, before it is redefined?
/// (The terminator counts as "after".)
fn reads_local_after(bb: &BasicBlock, from: usize, local: u32) -> bool {
    for st in &bb.statements[from..] {
        let mut reads = false;
        let mut defines = false;
        visit_operands(st, |op| {
            if let Operand::Copy(l) = op {
                if l.0 == local {
                    reads = true;
                }
            }
        });
        if let Some(d) = def_local(st) {
            if d == local {
                defines = true;
            }
        }
        if reads {
            return true;
        }
        if defines {
            return false; // redefined → old d dead
        }
    }
    // Check the terminator.
    let mut reads = false;
    match &bb.terminator {
        Terminator::Branch { cond, .. } | Terminator::Switch { value: cond, .. } => {
            if let Operand::Copy(l) = cond {
                if l.0 == local {
                    reads = true;
                }
            }
        }
        Terminator::Return(Some(Operand::Copy(l))) => {
            if l.0 == local {
                reads = true;
            }
        }
        _ => {}
    }
    reads
}

fn def_local(st: &Statement) -> Option<u32> {
    match st {
        Statement::Assign(d, _)
        | Statement::New { dest: d, .. }
        | Statement::StackNew { dest: d, .. }
        | Statement::GetField { dest: d, .. }
        | Statement::GetStatic { dest: d, .. }
        | Statement::NewArray { dest: d, .. }
        | Statement::ArrayLen { dest: d, .. }
        | Statement::ArrayLoad { dest: d, .. }
        | Statement::InstanceOf { dest: d, .. }
        | Statement::InstanceOfPending { dest: d, .. } => Some(d.0),
        Statement::Call { dest, .. }
        | Statement::CallGuarded { dest, .. }
        | Statement::CallVirtual { dest, .. }
        | Statement::CallPoly { dest, .. } => dest.map(|d| d.0),
        _ => None,
    }
}

fn visit_operands(st: &Statement, mut f: impl FnMut(&Operand)) {
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
        | Statement::CallPoly { args, .. } => args.iter().for_each(f),
        Statement::GetField { obj, .. } => f(obj),
        Statement::PutField { obj, value, .. } => {
            f(obj);
            f(value);
        }
        Statement::PutStatic { value, .. } => f(value),
        Statement::InstanceOf { obj, .. } | Statement::CheckCast { obj, .. } => f(obj),
        Statement::NewArray { len, .. } => f(len),
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
        _ => {}
    }
}
