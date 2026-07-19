//! Whole-program solver, stage 2 (DESIGN.md §7):
//! closed-world reachability (RTA), CHA devirtualization, dead-code pruning.
//!
//! Rapid Type Analysis (Bacon/Sweeney 1996): reachable methods and
//! instantiated classes are determined jointly in a fixpoint — a
//! virtual call site can only reach methods of classes that are
//! created with `new` somewhere in the reachable code. Sites with
//! exactly one target are rewritten to direct calls
//! (devirtualization after Dean/Grove/Chambers 1995, sound under
//! closed world); the receiver keeps its null check.

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

/// Maximum number of concrete target classes up to which a polymorphic site
/// becomes a type-guard cascade (above this, vtable dispatch stays cheaper).
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
    /// No instantiated type can lie in a reference cycle → the
    /// cycle collector is superfluous (phase 1 of the runtime elimination).
    pub acyclic: bool,
}

/// Key of a virtual call site: static class + name + descriptor.
type SiteKey = (String, String, String);

pub fn run(program: &mut Program) -> Stats {
    let mut stats = Stats::default();

    // --- RTA-Fixpunkt ---
    let func_index: BTreeMap<String, usize> =
        program.functions.iter().enumerate().map(|(i, f)| (f.name.clone(), i)).collect();

    let mut roots: Vec<String> = if func_index.contains_key("java_main") {
        vec!["java_main".to_string()]
    } else {
        // Library mode: no entry point known → everything is a root.
        func_index.keys().cloned().collect()
    };
    // Static initializers run at program startup → always reachable.
    for c in &program.classes {
        if c.has_clinit {
            let clinit = fastllvm_ir::clinit_symbol(&c.name);
            if func_index.contains_key(&clinit) {
                roots.push(clinit);
            }
        }
    }
    // Functions invoked through generated/native glue invisible to RTA (Vire
    // `spawn` workers, called from their C shim via jrt_spawn) → keep as roots.
    for name in &program.exported {
        if func_index.contains_key(name) {
            roots.push(name.clone());
        }
    }
    // Runnable.run() implementations are invoked through the native thread
    // trampoline (invisible to RTA) → treat them as roots.
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
    // Strings arise from literals/concatenation, not via `new`; but they
    // are instantiated as an Object subtype and must not let Object-method
    // calls be wrongly devirtualized.
    if program.class("java/lang/String").is_some() {
        instantiated.insert("java/lang/String".to_string());
    }
    // Autoboxing wrappers count as instantiated as soon as their valueOf box
    // is called (they do not arise via `new`).
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
        // Resolve virtual sites against the currently instantiated classes;
        // new targets trigger the next fixpoint round.
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

    // --- CHA/RTA devirtualization: sites with exactly one target ---
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
                        // Java semantics: the devirtualized call, too,
                        // throws NPE on a null receiver (catchable via CallGuarded).
                        bb.statements.insert(
                            i,
                            Statement::CallGuarded { dest, func: targets.into_iter().next().unwrap(), args },
                        );
                        stats.devirtualized += 1;
                        i += 1;
                        continue;
                    }
                    // Biconditional devirtualization: few (≤3) concrete
                    // target classes → type-guard cascade of direct calls
                    // instead of vtable dispatch (LLVM inlines the direct calls).
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

    // --- Pruning: keep only reachable functions ---
    let before = program.functions.len();
    program.functions.retain(|f| reachable.contains(&f.name));
    stats.reachable_functions = program.functions.len();
    stats.pruned_functions = before - program.functions.len();

    stats
}

/// Possible implementations of a virtual site among the
/// instantiated classes (RTA target set).
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
    // Does `sub` inherit/implement the type `sup` (class or interface)?
    let is_subtype = |sub: &str, sup: &str| -> bool {
        if sup == "java/lang/Object" {
            return true; // implicit root of all classes
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
        // Method resolution from the concrete class upward (JVMS 5.4.6).
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

/// Phase 1 of the runtime elimination: can *any* instantiated type lie in
/// a reference cycle? Builds the type reference graph (edge C→S if
/// C has a ref field of type T and S is an instantiated subtype of T;
/// arrays pass through via their element) and searches for a directed cycle.
/// If it is acyclic, pure RC suffices — the cycle collector is dropped.
/// Conservative: unknown/broad field types (`Object`) create edges to
/// all subtypes (better to assume one cycle too many than one too few).
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
    // Adjacency over instantiated classes.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (ci, &c) in insts.iter().enumerate() {
        // Collect ref-field targets including inherited fields.
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

/// Directed cycle search (white/gray/black DFS, iterative).
fn has_cycle(adj: &[Vec<usize>]) -> bool {
    let n = adj.len();
    let mut color = vec![0u8; n]; // 0=white, 1=gray, 2=black
    for start in 0..n {
        if color[start] != 0 {
            continue;
        }
        // Stack of (node, next-neighbor-index).
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        color[start] = 1;
        while let Some((u, i)) = stack.last().copied() {
            if i < adj[u].len() {
                stack.last_mut().unwrap().1 += 1;
                let v = adj[u][i];
                match color[v] {
                    1 => return true, // gray neighbor → back edge → cycle
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

/// Like `resolve_targets_ref`, but (concrete class → symbol) pairs: for the
/// biconditional devirtualization, which emits one vtable-pointer comparison
/// per concrete class. Deterministically sorted.
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
