//! Whole-Program Solver, Stufe 2 (DESIGN.md §7):
//! Closed-World-Reachability (RTA), CHA-Devirtualisierung, Dead-Code-Pruning.
//!
//! Rapid Type Analysis (Bacon/Sweeney 1996): erreichbare Methoden und
//! instanziierte Klassen werden gemeinsam im Fixpunkt bestimmt — ein
//! virtueller Call-Site kann nur Methoden von Klassen treffen, die
//! irgendwo im erreichbaren Code mit `new` erzeugt werden. Sites mit
//! genau einem Ziel werden zu direkten Calls umgeschrieben
//! (Devirtualisierung nach Dean/Grove/Chambers 1995, sound unter
//! Closed World); der Receiver behält seinen Null-Check.

mod bounds;
mod escape;
mod inline;
mod longcmp;
mod pending;
mod narrow;
mod refcopy;
pub use bounds::elide_bounds;
pub use escape::stack_allocate;
pub use inline::inline_program;
pub use longcmp::fuse_long_compares;
pub use pending::elide_pending_checks;
pub use narrow::narrow_fields;
pub use refcopy::elide_redundant_ref_copies;

use std::collections::{BTreeMap, BTreeSet};

use fastllvm_ir::*;

/// Höchstzahl konkreter Zielklassen, bis zu der ein polymorpher Site zur
/// Typ-Wächter-Kaskade wird (darüber bleibt Vtable-Dispatch günstiger).
const MAX_POLY_CLASSES: usize = 3;

#[derive(Debug, Default)]
pub struct Stats {
    pub instantiated_classes: usize,
    pub reachable_functions: usize,
    pub pruned_functions: usize,
    pub virtual_sites: usize,
    pub devirtualized: usize,
    pub poly_devirtualized: usize,
    pub inlined_calls: usize,
    pub stack_allocated: usize,
    /// Kein instanziierter Typ kann in einem Referenzzyklus liegen → der
    /// Zyklen-Collector ist überflüssig (Phase 1 der Runtime-Elimination).
    pub acyclic: bool,
}

/// Schlüssel eines virtuellen Call-Sites: statische Klasse + Name + Deskriptor.
type SiteKey = (String, String, String);

pub fn run(program: &mut Program) -> Stats {
    let mut stats = Stats::default();

    // --- RTA-Fixpunkt ---
    let func_index: BTreeMap<String, usize> =
        program.functions.iter().enumerate().map(|(i, f)| (f.name.clone(), i)).collect();

    let mut roots: Vec<String> = if func_index.contains_key("java_main") {
        vec!["java_main".to_string()]
    } else {
        // Library-Modus: kein Einstiegspunkt bekannt → alles ist Wurzel.
        func_index.keys().cloned().collect()
    };
    // Statische Initialisierer laufen beim Programmstart → immer erreichbar.
    for c in &program.classes {
        if c.has_clinit {
            let clinit = fastllvm_ir::clinit_symbol(&c.name);
            if func_index.contains_key(&clinit) {
                roots.push(clinit);
            }
        }
    }
    // Runnable.run()-Implementierungen werden über die native Thread-Trampoline
    // aufgerufen (für RTA unsichtbar) → als Wurzeln behandeln.
    if program.class("java/lang/Runnable").is_some() {
        for c in &program.classes {
            let implements_runnable = c.interfaces.iter().any(|i| i == "java/lang/Runnable");
            if implements_runnable {
                let run = fastllvm_ir::mangle(&c.name, "run", "()V");
                if func_index.contains_key(&run) {
                    roots.push(run);
                }
            }
        }
    }

    let mut reachable: BTreeSet<String> = BTreeSet::new();
    let mut instantiated: BTreeSet<String> = BTreeSet::new();
    // Strings entstehen aus Literalen/Konkatenation, nicht via `new`; sie
    // sind aber als Object-Subtyp instanziiert und dürfen Object-Methoden-
    // Calls nicht fälschlich devirtualisieren lassen.
    if program.class("java/lang/String").is_some() {
        instantiated.insert("java/lang/String".to_string());
    }
    // Autoboxing-Wrapper gelten als instanziiert, sobald ihre valueOf-Box
    // aufgerufen wird (sie entstehen nicht via `new`).
    let calls_fn = |sym: &str| {
        program
            .functions
            .iter()
            .flat_map(|f| &f.blocks)
            .flat_map(|b| &b.statements)
            .any(|st| matches!(st, Statement::Call { func, .. } if func == sym))
    };
    for (vf, cls) in [
        ("jrt_integer_valueof", "java/lang/Integer"),
        ("jrt_long_valueof", "java/lang/Long"),
        ("jrt_boolean_valueof", "java/lang/Boolean"),
        ("jrt_double_valueof", "java/lang/Double"),
        ("jrt_character_valueof", "java/lang/Character"),
        ("jrt_float_valueof", "java/lang/Float"),
    ] {
        if calls_fn(vf) {
            instantiated.insert(cls.to_string());
        }
    }
    let mut sites: BTreeSet<SiteKey> = BTreeSet::new();
    let mut worklist: Vec<String> = roots;

    loop {
        let mut changed = false;
        while let Some(name) = worklist.pop() {
            if !reachable.insert(name.clone()) {
                continue;
            }
            changed = true;
            let Some(&fi) = func_index.get(&name) else { continue };
            for bb in &program.functions[fi].blocks {
                for st in &bb.statements {
                    match st {
                        Statement::Call { func, .. } => {
                            if func_index.contains_key(func) && !reachable.contains(func) {
                                worklist.push(func.clone());
                            }
                        }
                        Statement::New { class, .. } => {
                            instantiated.insert(class.clone());
                        }
                        Statement::CallVirtual { class, name, desc, .. } => {
                            sites.insert((class.clone(), name.clone(), desc.clone()));
                        }
                        _ => {}
                    }
                }
            }
        }
        // Virtuelle Sites gegen die aktuell instanziierten Klassen auflösen;
        // neue Ziele stoßen die nächste Fixpunkt-Runde an.
        for (class, name, desc) in &sites {
            for target in resolve_targets(program, &instantiated, class, name, desc) {
                if !reachable.contains(&target) {
                    worklist.push(target);
                }
            }
        }
        if worklist.is_empty() && !changed {
            break;
        }
    }

    stats.acyclic = is_acyclic(&program.classes, &instantiated);
    stats.instantiated_classes = instantiated.len();
    stats.virtual_sites = sites.len();

    // --- CHA/RTA-Devirtualisierung: Sites mit genau einem Ziel ---
    for f in &mut program.functions {
        for bb in &mut f.blocks {
            let mut i = 0;
            while i < bb.statements.len() {
                if let Statement::CallVirtual { class, name, desc, .. } = &bb.statements[i] {
                    let targets = resolve_targets_ref(&program.classes, &instantiated, class, name, desc);
                    if targets.len() == 1 {
                        let Statement::CallVirtual { dest, args, .. } = bb.statements.remove(i) else {
                            unreachable!()
                        };
                        // Java-Semantik: auch der devirtualisierte Aufruf
                        // wirft NPE bei null-Receiver (abfangbar via CallGuarded).
                        bb.statements.insert(
                            i,
                            Statement::CallGuarded { dest, func: targets.into_iter().next().unwrap(), args },
                        );
                        stats.devirtualized += 1;
                        i += 1;
                        continue;
                    }
                    // Bikonditionale Devirtualisierung: wenige (≤3) konkrete
                    // Zielklassen → Typ-Wächter-Kaskade aus Direkt-Aufrufen
                    // statt Vtable-Dispatch (LLVM inlinet die Direkt-Calls).
                    let pairs = resolve_target_pairs(&program.classes, &instantiated, class, name, desc);
                    let distinct: BTreeSet<&String> = pairs.iter().map(|(_, s)| s).collect();
                    if pairs.len() >= 2 && pairs.len() <= MAX_POLY_CLASSES && distinct.len() >= 2 {
                        let Statement::CallVirtual { dest, ret, args, .. } = bb.statements.remove(i) else {
                            unreachable!()
                        };
                        bb.statements.insert(i, Statement::CallPoly { dest, ret, args, targets: pairs });
                        stats.poly_devirtualized += 1;
                        i += 1;
                        continue;
                    }
                }
                i += 1;
            }
        }
    }

    // --- Pruning: nur erreichbare Funktionen behalten ---
    let before = program.functions.len();
    program.functions.retain(|f| reachable.contains(&f.name));
    stats.reachable_functions = program.functions.len();
    stats.pruned_functions = before - program.functions.len();

    stats
}

/// Mögliche Implementierungen eines virtuellen Sites unter den
/// instanziierten Klassen (RTA-Zielmenge).
fn resolve_targets(
    program: &Program,
    instantiated: &BTreeSet<String>,
    class: &str,
    name: &str,
    desc: &str,
) -> BTreeSet<String> {
    resolve_targets_ref(&program.classes, instantiated, class, name, desc)
}

fn resolve_targets_ref(
    classes: &[ClassInfo],
    instantiated: &BTreeSet<String>,
    class: &str,
    name: &str,
    desc: &str,
) -> BTreeSet<String> {
    let class_of = |n: &str| classes.iter().find(|c| c.name == n);
    // Erbt/implementiert `sub` den Typ `sup` (Klasse oder Interface)?
    let is_subtype = |sub: &str, sup: &str| -> bool {
        if sup == "java/lang/Object" {
            return true; // implizite Wurzel aller Klassen
        }
        let mut stack = vec![sub.to_string()];
        let mut seen = BTreeSet::new();
        while let Some(c) = stack.pop() {
            if c == sup {
                return true;
            }
            if !seen.insert(c.clone()) {
                continue;
            }
            if let Some(ci) = class_of(&c) {
                if let Some(s) = &ci.super_name {
                    stack.push(s.clone());
                }
                for i in &ci.interfaces {
                    stack.push(i.clone());
                }
            }
        }
        false
    };
    let mut targets = BTreeSet::new();
    for inst in instantiated {
        if !is_subtype(inst, class) {
            continue;
        }
        // Methodenauflösung ab der konkreten Klasse aufwärts (JVMS 5.4.6).
        let mut cur = inst.as_str();
        loop {
            let Some(ci) = class_of(cur) else { break };
            if let Some(m) = ci.methods.iter().find(|m| m.name == name && m.desc == desc && m.has_body) {
                targets.insert(m.mangled.clone());
                break;
            }
            match ci.super_name.as_deref() {
                Some(s) => cur = s,
                None => break,
            }
        }
    }
    targets
}

/// Phase 1 der Runtime-Elimination: Kann *irgendein* instanziierter Typ in
/// einem Referenzzyklus liegen? Baut den Typ-Referenzgraphen (Kante C→S, wenn
/// C ein Ref-Feld vom Typ T hat und S ein instanziierter Subtyp von T ist;
/// Arrays leiten über ihr Element durch) und sucht einen gerichteten Zyklus.
/// Ist er azyklisch, genügt reine RC — der Zyklen-Collector entfällt.
/// Konservativ: unbekannte/breite Feldtypen (`Object`) erzeugen Kanten zu
/// allen Subtypen (lieber einen Zyklus zu viel annehmen als einen zu wenig).
fn is_acyclic(classes: &[ClassInfo], instantiated: &BTreeSet<String>) -> bool {
    let class_of = |n: &str| classes.iter().find(|c| c.name == n);
    let is_subtype = |sub: &str, sup: &str| -> bool {
        if sup == "java/lang/Object" {
            return true;
        }
        let mut stack = vec![sub.to_string()];
        let mut seen = BTreeSet::new();
        while let Some(c) = stack.pop() {
            if c == sup {
                return true;
            }
            if !seen.insert(c.clone()) {
                continue;
            }
            if let Some(ci) = class_of(&c) {
                if let Some(s) = &ci.super_name {
                    stack.push(s.clone());
                }
                stack.extend(ci.interfaces.iter().cloned());
            }
        }
        false
    };
    let insts: Vec<&str> = instantiated.iter().map(|s| s.as_str()).collect();
    let n = insts.len();
    // Adjazenz über instanziierte Klassen.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (ci, &c) in insts.iter().enumerate() {
        // Ref-Feld-Ziele inkl. geerbter Felder einsammeln.
        let mut targets: Vec<String> = Vec::new();
        let mut cur = Some(c.to_string());
        let mut guard = 0;
        while let Some(cn) = cur {
            guard += 1;
            if guard > 10_000 {
                break;
            }
            let Some(info) = class_of(&cn) else { break };
            for f in &info.fields {
                if let Some(t) = &f.ref_target {
                    targets.push(t.clone());
                }
            }
            cur = info.super_name.clone();
        }
        for t in &targets {
            for (si, &s) in insts.iter().enumerate() {
                if is_subtype(s, t) {
                    adj[ci].push(si);
                }
            }
        }
    }
    !has_cycle(&adj)
}

/// Gerichtete Zyklen-Suche (weiß/grau/schwarz-DFS, iterativ).
fn has_cycle(adj: &[Vec<usize>]) -> bool {
    let n = adj.len();
    let mut color = vec![0u8; n]; // 0=weiß, 1=grau, 2=schwarz
    for start in 0..n {
        if color[start] != 0 {
            continue;
        }
        // Stack aus (Knoten, nächster-Nachbar-Index).
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        color[start] = 1;
        while let Some((u, i)) = stack.last().copied() {
            if i < adj[u].len() {
                stack.last_mut().unwrap().1 += 1;
                let v = adj[u][i];
                match color[v] {
                    1 => return true, // grauer Nachbar → Rückkante → Zyklus
                    0 => {
                        color[v] = 1;
                        stack.push((v, 0));
                    }
                    _ => {}
                }
            } else {
                color[u] = 2;
                stack.pop();
            }
        }
    }
    false
}

/// Wie `resolve_targets_ref`, aber (konkrete Klasse → Symbol)-Paare: für die
/// bikonditionale Devirtualisierung, die pro konkreter Klasse einen
/// Vtable-Zeiger-Vergleich emittiert. Deterministisch sortiert.
fn resolve_target_pairs(
    classes: &[ClassInfo],
    instantiated: &BTreeSet<String>,
    class: &str,
    name: &str,
    desc: &str,
) -> Vec<(String, String)> {
    let class_of = |n: &str| classes.iter().find(|c| c.name == n);
    let is_subtype = |sub: &str, sup: &str| -> bool {
        if sup == "java/lang/Object" {
            return true;
        }
        let mut stack = vec![sub.to_string()];
        let mut seen = BTreeSet::new();
        while let Some(c) = stack.pop() {
            if c == sup {
                return true;
            }
            if !seen.insert(c.clone()) {
                continue;
            }
            if let Some(ci) = class_of(&c) {
                if let Some(s) = &ci.super_name {
                    stack.push(s.clone());
                }
                for i in &ci.interfaces {
                    stack.push(i.clone());
                }
            }
        }
        false
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    for inst in instantiated {
        if !is_subtype(inst, class) {
            continue;
        }
        let mut cur = inst.as_str();
        loop {
            let Some(ci) = class_of(cur) else { break };
            if let Some(m) = ci.methods.iter().find(|m| m.name == name && m.desc == desc && m.has_body) {
                pairs.push((inst.clone(), m.mangled.clone()));
                break;
            }
            match ci.super_name.as_deref() {
                Some(s) => cur = s,
                None => break,
            }
        }
    }
    pairs.sort();
    pairs
}
