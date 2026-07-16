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

    // Objekte = Allokations-Sites. Position (bi, si) + Ziel-Local.
    let news: Vec<(usize, usize, Local)> = f
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(bi, bb)| {
            bb.statements.iter().enumerate().filter_map(move |(si, st)| match st {
                Statement::New { dest, .. } => Some((bi, si, *dest)),
                _ => None,
            })
        })
        .collect();
    if news.is_empty() {
        return 0;
    }

    // Alias-Menge pro Objekt (flussunsensitiver Kopie-Fixpunkt; wegen
    // Local-Slot-Wiederverwendung konservativ überschätzt → nur mehr Escapes).
    let aliases: Vec<BTreeSet<Local>> = news.iter().map(|(_, _, d)| alias_set(f, *d)).collect();
    // Objekte, die ein Operand referenzieren kann.
    let objs_of = |op: &Operand| -> Vec<usize> {
        match op {
            Operand::Copy(l) => (0..news.len()).filter(|&i| aliases[i].contains(l)).collect(),
            _ => Vec::new(),
        }
    };

    // direct[o] = o entkommt unmittelbar; edges = ungerichtete Kanten zwischen
    // Objekten, die per Feld verbunden sind (both-or-neither: Container und
    // Inhalt werden nur gemeinsam promoviert). So hält ein Stack-Container
    // ausschließlich immortale Inhalte → keine Feld-Freigabe/Leck möglich.
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
                // Aufrufargumente entkommen (Callee könnte sie festhalten);
                // der reine Null-Check tut das nicht.
                Statement::Call { func, args, .. } => {
                    if func != "jrt_null_check" {
                        for a in args {
                            mark(&mut direct, a);
                        }
                    }
                }
                Statement::CallGuarded { args, .. }
                | Statement::CallVirtual { args, .. }
                | Statement::CallPoly { args, .. } => {
                    for a in args {
                        mark(&mut direct, a);
                    }
                }
                Statement::PutStatic { value, .. } | Statement::ArrayStore { value, .. } => {
                    mark(&mut direct, value);
                }
                // Feld-Sensitivität, `obj.field = value`:
                //  - value verfolgt, obj verfolgt  → ungerichtete Kante value↔obj
                //  - value verfolgt, obj unbekannt → value entkommt (in fremden
                //    Container gespeichert)
                //  - value unbekannte Ref, obj verfolgt → obj entkommt (ein
                //    immortaler Stack-Container hielte sonst eine Heap-Referenz,
                //    deren Drop nie läuft → Leck)
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

    // Fixpunkt: Entkommen über die ungerichteten Kanten propagieren — eine
    // Zusammenhangskomponente entkommt, sobald ein Mitglied entkommt.
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

    // Nicht entkommende, nicht in Schleifen liegende Objekte stack-allozieren.
    let mut count = 0;
    for (idx, (bi, si, _)) in news.iter().enumerate() {
        if escape[idx] || cyclic[*bi] {
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

/// Alias-Fixpunkt: alle Locals, die den Wert von `root` halten können.
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
