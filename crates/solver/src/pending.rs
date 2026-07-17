//! Elimination toter pending-Exception-Prüfungen.
//!
//! Das Exception-Modell fügt nach jedem potenziell werfenden Aufruf ein
//! `jrt_pending_set` + Branch ein (Handler/Propagation vs. Fortsetzung). Kann
//! der vorangehende Aufruf beweisbar *nicht* werfen, ist die Prüfung tot — ein
//! `call jrt_pending_set` je Iteration, das Rust nicht hat. Diese Analyse
//! entfernt sie.
//!
//! Throw-Freiheit (Fixpunkt): eine Funktion setzt nie eine pending-Exception,
//! wenn kein Statement das kann — Feldzugriffe nur auf beweisbar nicht-null
//! Objekte (New-Ergebnisse), Aufrufe nur an throw-freie Funktionen bzw. bekannt
//! harmlose Runtime-Helfer. Konservativ: im Zweifel „kann werfen" (dann bleibt
//! die Prüfung stehen — korrekt, nur ungenutzte Optimierung).

use std::collections::{BTreeMap, BTreeSet};

use fastllvm_ir::*;

/// Entfernt tote pending-Prüfungen. Gibt die Anzahl entfernter Prüfungen zurück.
pub fn elide_pending_checks(program: &mut Program) -> usize {
    // Instanzmethoden: dort ist Local 0 = `this` und nie null (der Aufrufer
    // prüft den Receiver bzw. `this` kommt aus `new`).
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

/// Bekannt nicht-werfende Runtime-Helfer (setzen nie `pending`).
fn runtime_safe(func: &str) -> bool {
    // werfende Helfer explizit ausschließen; alles andere jrt_* gilt als sicher,
    // wenn es hier nicht als werfend gelistet ist.
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

/// Nicht-null beweisbare Locals: `this` (bei Instanzmethoden), New/StackNew-
/// Ergebnisse und Kopien davon.
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
    // Kopien: Fixpunkt über Assign(d, Copy(s)).
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
        // Eine erneute Zuweisung eines nicht-nn Werts würde d entwerten; da wir
        // flussunsensitiv arbeiten, entfernen wir d wieder, falls es auch einen
        // Nicht-nn-Def hat.
        if nn.len() == before {
            break;
        }
    }
    // Konservativ: Locals mit irgendeinem möglicherweise-null Def wieder streichen.
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

/// Kann dieses Statement eine pending-Exception setzen?
fn can_throw(st: &Statement, throw_free: &BTreeMap<String, bool>, non_null: &BTreeSet<u32>) -> bool {
    let call_throws = |func: &str, recv: Option<&Operand>| -> bool {
        // Nutzer-Funktion: laut Summary. Runtime: laut Liste. Sonst konservativ.
        let target_ok = throw_free.get(func).copied().unwrap_or(false) || runtime_safe(func);
        if !target_ok {
            return true;
        }
        // CallGuarded prüft den Receiver auf null → kann NPE, außer non-null.
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
        Statement::ArrayLoad { .. } | Statement::ArrayStore { .. } | Statement::ArrayLen { .. } => true,
        Statement::InstanceOfPending { .. } | Statement::CheckCast { .. } => true,
        _ => false,
    }
}

/// Throw-Freiheits-Fixpunkt.
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

/// Entfernt in einer Funktion die pending-Prüfungen, deren Block nicht werfen
/// kann. Struktur je Prüfblock: … [werfende Op] ; `Call jrt_pending_set → c` ;
/// `Branch{c, exc, cont}`. Ist keine Op im Block werfend → `Goto(cont)`.
fn elide_in_function(
    f: &mut Function,
    throw_free: &BTreeMap<String, bool>,
    non_null: &BTreeSet<u32>,
) -> usize {
    let mut removed = 0;
    for bb in &mut f.blocks {
        // Muster erkennen: letztes Statement ist jrt_pending_set, Terminator ist
        // Branch auf dessen dest.
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
        // Kann irgendein Statement (außer der pending-Prüfung selbst) werfen?
        let n = bb.statements.len();
        let throws = bb.statements[..n - 1]
            .iter()
            .any(|st| can_throw(st, throw_free, &non_null));
        if throws {
            continue;
        }
        bb.statements.pop(); // jrt_pending_set entfernen
        bb.terminator = Terminator::Goto(cont);
        removed += 1;
    }
    removed
}
