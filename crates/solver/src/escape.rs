//! Escape-Analyse (Choi et al. 1999, stark vereinfacht): Objekte, die ihre
//! Funktion beweisbar nie verlassen, werden stack-alloziert (`StackNew`).
//!
//! Das ist der erste Speichersicherheits-/Ownership-Baustein (DESIGN.md
//! §6a): ein nicht entkommendes Objekt hat exakt einen Besitzer — den
//! Stack-Frame — und eine statisch bewiesene Lebenszeit, wie ein Rust-Wert.
//! Läuft nach Devirtualisierung + Inlining: erst durch das Inlining der
//! Konstruktoren wird der Receiver-Store sichtbar statt als entkommendes
//! Call-Argument (Synergie aus DESIGN.md §4).
//!
//! Konservative Escape-Quellen:
//! - Rückgabe (`Return`) eines Alias
//! - Argument eines Calls (außer `jrt_null_check`) oder virtuellen Calls
//! - als *Wert* in `putfield` gespeichert (Stores *in* das Objekt sind ok)
//!
//! Stack-Allokation nur außerhalb von Schleifen: der Alloca-Slot würde
//! sonst über Iterationen wiederverwendet, während Aliase aus früheren
//! Iterationen noch leben könnten.

use std::collections::BTreeSet;

use fastllvm_ir::*;

pub fn stack_allocate(program: &mut Program) -> usize {
    let mut total = 0;
    for f in &mut program.functions {
        total += run_function(f);
    }
    total
}

fn run_function(f: &mut Function) -> usize {
    let cyclic = cyclic_blocks(f);
    let mut promote: Vec<(usize, usize)> = Vec::new();

    for (bi, bb) in f.blocks.iter().enumerate() {
        for (si, st) in bb.statements.iter().enumerate() {
            let Statement::New { dest, .. } = st else { continue };
            if cyclic[bi] {
                continue;
            }
            if !escapes(f, *dest) {
                promote.push((bi, si));
            }
        }
    }

    let n = promote.len();
    for (bi, si) in promote {
        let Statement::New { dest, class } = f.blocks[bi].statements[si].clone() else {
            unreachable!()
        };
        f.blocks[bi].statements[si] = Statement::StackNew { dest, class };
    }
    n
}

/// Alias-Fixpunkt: alle Locals, die den Wert von `root` halten können,
/// dann Prüfung aller Escape-Quellen.
fn escapes(f: &Function, root: Local) -> bool {
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

    let is_alias = |op: &Operand| matches!(op, Operand::Copy(l) if aliases.contains(l));

    for bb in &f.blocks {
        for st in &bb.statements {
            match st {
                Statement::Call { func, args, .. } => {
                    if func != "jrt_null_check" && args.iter().any(is_alias) {
                        return true;
                    }
                }
                Statement::CallVirtual { args, .. } | Statement::CallPoly { args, .. } => {
                    if args.iter().any(is_alias) {
                        return true;
                    }
                }
                Statement::PutField { value, .. } => {
                    if is_alias(value) {
                        return true;
                    }
                }
                Statement::ArrayStore { value, .. } => {
                    // In ein Array gespeichert → das Objekt überlebt uns
                    // potentiell; konservativ als entkommend werten.
                    if is_alias(value) {
                        return true;
                    }
                }
                Statement::PutStatic { value, .. } => {
                    // In ein statisches Feld gespeichert → entkommt.
                    if is_alias(value) {
                        return true;
                    }
                }
                // GetField/PutField/Array-Zugriff über `obj`/`arr` sowie
                // Vergleiche lassen das Objekt selbst nicht entkommen.
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

/// Blöcke, die auf einem Zyklus liegen (sich selbst erreichen können).
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
            // DFS von den Nachfolgern; erreicht sie `start`, liegt er im Zyklus.
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
