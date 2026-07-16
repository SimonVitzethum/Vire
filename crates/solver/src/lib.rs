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

mod escape;
mod inline;
pub use escape::stack_allocate;
pub use inline::inline_program;

use std::collections::{BTreeMap, BTreeSet};

use fastllvm_ir::*;

#[derive(Debug, Default)]
pub struct Stats {
    pub instantiated_classes: usize,
    pub reachable_functions: usize,
    pub pruned_functions: usize,
    pub virtual_sites: usize,
    pub devirtualized: usize,
    pub inlined_calls: usize,
    pub stack_allocated: usize,
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

    let mut reachable: BTreeSet<String> = BTreeSet::new();
    let mut instantiated: BTreeSet<String> = BTreeSet::new();
    // Strings entstehen aus Literalen/Konkatenation, nicht via `new`; sie
    // sind aber als Object-Subtyp instanziiert und dürfen Object-Methoden-
    // Calls nicht fälschlich devirtualisieren lassen.
    if program.class("java/lang/String").is_some() {
        instantiated.insert("java/lang/String".to_string());
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
                        // wirft NPE bei null-Receiver.
                        bb.statements.insert(
                            i,
                            Statement::Call { dest: None, func: "jrt_null_check".into(), args: vec![args[0].clone()] },
                        );
                        bb.statements.insert(
                            i + 1,
                            Statement::Call { dest, func: targets.into_iter().next().unwrap(), args },
                        );
                        stats.devirtualized += 1;
                        i += 2;
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
