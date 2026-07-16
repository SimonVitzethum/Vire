//! Inliner auf der Mittel-IR.
//!
//! Läuft nach der Devirtualisierung, damit devirtualisierte Sites inlinebar
//! werden — Inlining schaltet dann LLVM-seitig alles Weitere frei
//! (DESIGN.md §4). Konservativ: nur direkte Calls, nur kleine, nicht
//! selbstrekursive Ziele.

use std::collections::BTreeMap;

use fastllvm_ir::*;

/// Obergrenze an Statements, bis zu der eine Funktion inlinet wird.
const SIZE_LIMIT: usize = 16;
/// Obergrenze an Inline-Vorgängen pro Aufrufer (gegen Code-Explosion
/// durch Ketten kleiner Funktionen).
const PER_CALLER_LIMIT: usize = 64;

pub fn inline_program(program: &mut Program) -> usize {
    let size = |f: &Function| f.blocks.iter().map(|b| b.statements.len()).sum::<usize>();
    let calls_self = |f: &Function| {
        f.blocks.iter().flat_map(|b| &b.statements).any(|st| {
            matches!(st, Statement::Call { func, .. } if *func == f.name)
        })
    };
    // Funktionen mit Exception-Kontrollfluss nicht inlinen: ihr
    // Propagate-Return würde beim Inlinen zum normalen Fortsetzungsblock
    // umgebogen und der pending-Check des Aufrufers ginge verloren.
    let has_exception_flow = |f: &Function| {
        f.blocks.iter().flat_map(|b| &b.statements).any(|st| {
            matches!(st, Statement::Call { func, .. }
                if func == "jrt_throw" || func == "jrt_pending_set" || func == "jrt_take_pending")
        })
    };

    // Kandidaten kopieren, damit Aufrufer mutierbar bleiben.
    let candidates: BTreeMap<String, Function> = program
        .functions
        .iter()
        .filter(|f| size(f) <= SIZE_LIMIT && !calls_self(f) && !has_exception_flow(f))
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
            if let Statement::Call { func, .. } = st {
                if candidates.contains_key(func) && *func != f.name {
                    return Some((bi, si));
                }
            }
        }
    }
    None
}

/// Ersetzt den Call in Block `blk` an Index `idx` durch den Rumpf des Callees:
/// Block wird am Call geteilt, Callee-Blöcke (mit umnummerierten Locals und
/// Blöcken) angehängt, Returns auf den Fortsetzungsblock umgebogen.
fn splice(f: &mut Function, blk: usize, idx: usize, candidates: &BTreeMap<String, Function>) {
    let Statement::Call { dest, func, args } = f.blocks[blk].statements[idx].clone() else {
        unreachable!()
    };
    let callee = &candidates[&func];

    let local_off = f.locals.len() as u32;
    let block_off = f.blocks.len() as u32;
    let cont_block = Block(block_off + callee.blocks.len() as u32);

    f.locals.extend(callee.locals.iter().copied());

    // Aufrufer-Block teilen: [0..idx) + Argument-Zuweisungen + Sprung in
    // den Callee; Rest wandert in den Fortsetzungsblock.
    let tail: Vec<Statement> = f.blocks[blk].statements.split_off(idx + 1);
    f.blocks[blk].statements.pop(); // der Call selbst
    for (k, arg) in args.into_iter().enumerate() {
        f.blocks[blk]
            .statements
            .push(Statement::Assign(Local(local_off + k as u32), Rvalue::Use(arg)));
    }
    let cont_term = std::mem::replace(&mut f.blocks[blk].terminator, Terminator::Goto(Block(block_off)));

    for cb in &callee.blocks {
        let mut statements: Vec<Statement> = cb.statements.clone();
        for st in &mut statements {
            remap_statement(st, local_off);
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

    f.blocks.push(BasicBlock { statements: tail, terminator: cont_term });
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
        Statement::Call { dest, args, .. } | Statement::CallVirtual { dest, args, .. } => {
            if let Some(d) = dest {
                remap_local(d, off);
            }
            for a in args {
                remap_operand(a, off);
            }
        }
        Statement::New { dest, .. } | Statement::StackNew { dest, .. } => remap_local(dest, off),
        Statement::GetField { dest, obj, .. } => {
            remap_local(dest, off);
            remap_operand(obj, off);
        }
        Statement::PutField { obj, value, .. } => {
            remap_operand(obj, off);
            remap_operand(value, off);
        }
        Statement::NewArray { dest, len, .. } => {
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
    }
}
