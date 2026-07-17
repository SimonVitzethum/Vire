//! Fusion von Long-/Double-Vergleichen mit ihrem nachfolgenden 0-Test.
//!
//! javac senkt `j < N` (long) als `lcmp; if<cond>` — im Bytecode ein
//! eigenständiges `lcmp`, das `sign(x−y) ∈ {-1,0,1}` liefert, gefolgt von einem
//! Verzweigungstest gegen 0. Das Frontend bildet das auf einen `jrt_lcmp`-Call
//! plus `CmpX(result, 0)` ab — ein **Funktionsaufruf pro Iteration** in heißen
//! Schleifen, den Rust als eine native `icmp slt i64` erledigt.
//!
//! Da `sign(x−y) <op> 0 ⟺ x <op> y` für alle sechs Vergleiche gilt, ersetzt
//! diese Peephole-Optimierung das Paar durch einen direkten `Binary(op, x, y)`
//! auf den i64-Operanden (das Backend emittiert `icmp <cc> i64`) und entfernt
//! den nun toten `jrt_lcmp`-Aufruf. Konservativ: nur wenn der lcmp-Ergebnis-Slot
//! ausschließlich vom unmittelbar folgenden 0-Test gelesen wird.

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
        // Muster: Call(d, jrt_lcmp|dcmp, [x, y]) ; Assign(c, CmpX(Copy(d), 0)).
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
        // d darf nach dem 0-Test nicht mehr gelesen werden (bis zur nächsten
        // Definition) und nicht im Terminator vorkommen.
        if reads_local_after(bb, i + 2, d) {
            i += 1;
            continue;
        }
        // Fusion: Vergleich direkt auf die i64-Operanden, lcmp-Call entfernen.
        if let Statement::Assign(_, rv) = &mut bb.statements[i + 1] {
            *rv = Rvalue::Binary(op, x, y);
        }
        bb.statements.remove(i);
        fused += 1;
        // i zeigt jetzt auf den (ehemaligen) Assign; weiter dahinter suchen.
        i += 1;
    }
    fused
}

/// Wird `local` ab `from` als Operand gelesen, bevor es neu definiert wird?
/// (Terminator zählt als "danach".)
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
            return false; // neu definiert → altes d tot
        }
    }
    // Terminator prüfen.
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
