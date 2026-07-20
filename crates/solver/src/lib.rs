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
mod constprop;
mod escape;
mod inline;
mod longcmp;
mod pending;
mod narrow;
mod refcopy;
pub use bounds::elide_bounds;
pub use constprop::propagate_const_scalars;
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

    // Acyclic ⇔ the type-reference graph has no cycle, OR the shape/freshness analysis
    // proves the type-cyclic classes can never form a runtime cycle (all cyclic-slot
    // stores are null or fresh+linear). Either way pure RC suffices → drop the collector.
    let cyclic = cyclic_classes(&program.classes, &instantiated);
    stats.acyclic = cyclic.is_empty() || shape_proves_acyclic(program, &cyclic);
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
/// (Superseded by `cyclic_classes` + `shape_proves_acyclic`; kept as the reference
/// spelling of the type-graph construction.)
#[allow(dead_code)]
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

/// The set of instantiated classes that lie on a **type** cycle (a class whose
/// ref-fields can transitively reach itself). Same graph as `is_acyclic`, but it
/// returns *which* classes are cyclic (empty ⇔ acyclic) so the shape analysis can try
/// to prove those specific classes tree-shaped at runtime.
fn cyclic_classes(classes: &[ClassInfo], instantiated: &BTreeSet<String>) -> BTreeSet<String> {
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
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (ci, &c) in insts.iter().enumerate() {
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
    // A node is cyclic iff it can reach itself along ≥1 edge (DFS from each node).
    let mut result = BTreeSet::new();
    for start in 0..n {
        let mut stack = adj[start].clone();
        let mut seen = vec![false; n];
        while let Some(u) = stack.pop() {
            if u == start {
                result.insert(insts[start].to_string());
                break;
            }
            if seen[u] {
                continue;
            }
            seen[u] = true;
            stack.extend(adj[u].iter().copied());
        }
    }
    result
}

/// **Shape/freshness analysis** — prove that instances of the `cyclic` (type-cyclic)
/// classes can never form a runtime cycle, so the cycle collector is unnecessary and
/// pure RC suffices. Sound sufficient condition:
///
/// *Every* store that could place a reference into a cyclic-type slot (a `PutField` to a
/// ref field whose target could be cyclic, and — conservatively — any `PutStatic`/ref
/// `ArrayStore`) stores either `null` or a value that is **fresh AND linear**: defined by
/// `new`/`NewArray` or by a call to an *allocator-like* function (one whose every return
/// is itself fresh), and whose **only use in its function is this store** (a move).
///
/// Why this is sound: a cycle needs an edge x→y where y already reaches x. A fresh value
/// reaches nothing pre-existing; linearity (sole use) means no *other* reference to it
/// exists, so it cannot be an ancestor either. If every cyclic-slot write stores such a
/// value, no back-edge can ever be created. A single unsafe store (e.g. `b.next = a` with
/// `a` a live, multiply-used local — the adversarial `a↔b`) fails the check and the
/// collector is conservatively kept. Precision is sacrificed for soundness (a shared/DAG
/// child is fresh-but-not-linear ⇒ kept), which never drops a collector that is needed.
fn shape_proves_acyclic(program: &Program, cyclic: &BTreeSet<String>) -> bool {
    use fastllvm_ir::{Operand, Rvalue, Statement, Terminator};

    // Per-function facts: locals defined by New/NewArray, and locals defined by a direct
    // call (→ that callee's name), for the allocator-like fixpoint and freshness.
    struct FnFacts {
        new_defs: std::collections::HashSet<u32>,
        call_defs: std::collections::HashMap<u32, String>,
    }
    let visit_ops = |st: &Statement, f: &mut dyn FnMut(&Operand)| match st {
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
    };
    let facts: std::collections::HashMap<&str, FnFacts> = program
        .functions
        .iter()
        .map(|fun| {
            let mut new_defs = std::collections::HashSet::new();
            let mut call_defs = std::collections::HashMap::new();
            for bb in &fun.blocks {
                for st in &bb.statements {
                    match st {
                        Statement::New { dest, .. } | Statement::NewArray { dest, .. } => {
                            new_defs.insert(dest.0);
                        }
                        Statement::Call { dest: Some(d), func, .. }
                        | Statement::CallGuarded { dest: Some(d), func, .. } => {
                            call_defs.insert(d.0, func.clone());
                        }
                        _ => {}
                    }
                }
            }
            (fun.name.as_str(), FnFacts { new_defs, call_defs })
        })
        .collect();

    // Allocator-like fixpoint (greatest): start every fn "allocator-like", remove any
    // whose some value-return is not fresh (New, or a call to a still-allocator-like fn).
    let mut alloc_like: std::collections::HashSet<&str> =
        program.functions.iter().map(|f| f.name.as_str()).collect();
    let is_fresh = |name: &str, l: u32, set: &std::collections::HashSet<&str>| -> bool {
        let Some(ff) = facts.get(name) else { return false };
        ff.new_defs.contains(&l) || ff.call_defs.get(&l).map_or(false, |c| set.contains(c.as_str()))
    };
    loop {
        let mut changed = false;
        for fun in &program.functions {
            if !alloc_like.contains(fun.name.as_str()) {
                continue;
            }
            let ok = fun.blocks.iter().all(|bb| match &bb.terminator {
                Terminator::Return(Some(Operand::Copy(l))) => is_fresh(&fun.name, l.0, &alloc_like),
                Terminator::Return(Some(Operand::ConstNull)) | Terminator::Return(None) => true,
                Terminator::Return(Some(_)) => false, // a non-ref/const return ⇒ not an allocator
                _ => true,
            });
            if !ok {
                alloc_like.remove(fun.name.as_str());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // A ref-field target that could hold a cyclic instance (subtype relation).
    let class_of = |n: &str| program.classes.iter().find(|c| c.name == n);
    let subtype_of_target = |target: &str| -> bool {
        // Is any cyclic class a subtype of `target` (so the field could hold it)?
        cyclic.iter().any(|cc| {
            let mut stack = vec![cc.clone()];
            let mut seen = BTreeSet::new();
            while let Some(c) = stack.pop() {
                if c == target || target == "java/lang/Object" {
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
        })
    };
    let field_target = |class: &str, field: &str| -> Option<String> {
        let mut cur = Some(class.to_string());
        while let Some(cn) = cur {
            let info = class_of(&cn)?;
            if let Some(fi) = info.fields.iter().find(|f| f.name == field) {
                return fi.ref_target.clone();
            }
            cur = info.super_name.clone();
        }
        None
    };

    let dbg = std::env::var("FASTLLVM_DEBUG_SHAPE").is_ok();
    if dbg {
        eprintln!("[shape] cyclic={:?}", cyclic);
        eprintln!("[shape] allocator-like={:?}", alloc_like);
    }
    // A cyclic-slot write must store null or a value that is **fresh AND linear** at that
    // point. Freshness = "freshly allocated and not yet used". Because the IR is not SSA
    // (a stack slot is reused across the two `make()` calls) AND an allocating call splits
    // the block (its pending-exception check), we compute freshness by a **forward
    // dataflow**: a local is fresh at a program point iff it is fresh on *every* incoming
    // path (meet = intersection) and has not been used since its allocating def. Within a
    // block, an allocating def (New / allocator-like call) makes a local fresh; ANY use —
    // a stored value, a call argument, or the object of a field write — consumes it. So
    // `L=make(); … ; n.l=L` across the exception split still passes, while the adversarial
    // `a.v=…; a.next=b; b.next=a` fails (a used as the object of `a.next=b` before it is
    // stored into `b.next`).
    use std::collections::{HashMap, HashSet};
    let succs = |t: &Terminator| -> Vec<usize> {
        match t {
            Terminator::Goto(b) => vec![b.0 as usize],
            Terminator::Branch { then_blk, else_blk, .. } => vec![then_blk.0 as usize, else_blk.0 as usize],
            Terminator::Switch { default, cases, .. } => {
                let mut v = vec![default.0 as usize];
                v.extend(cases.iter().map(|(_, b)| b.0 as usize));
                v
            }
            Terminator::Return(_) => vec![],
        }
    };
    // Transfer a freshness set through one block's statements (no validation).
    let transfer = |ff: &FnFacts, alloc: &HashSet<&str>, entry: &HashSet<u32>, bb: &BasicBlock| -> HashSet<u32> {
        let _ = ff;
        let mut fresh = entry.clone();
        for st in &bb.statements {
            visit_ops(st, &mut |op| {
                if let Operand::Copy(l) = op {
                    fresh.remove(&l.0);
                }
            });
            match st {
                Statement::New { dest, .. } | Statement::NewArray { dest, .. } => {
                    fresh.insert(dest.0);
                }
                Statement::Call { dest: Some(d), func, .. }
                | Statement::CallGuarded { dest: Some(d), func, .. } => {
                    if alloc.contains(func.as_str()) {
                        fresh.insert(d.0);
                    } else {
                        fresh.remove(&d.0);
                    }
                }
                Statement::Assign(d, _)
                | Statement::GetField { dest: d, .. }
                | Statement::GetStatic { dest: d, .. }
                | Statement::ArrayLoad { dest: d, .. } => {
                    fresh.remove(&d.0);
                }
                _ => {}
            }
        }
        fresh
    };
    for fun in &program.functions {
        let ff = &facts[fun.name.as_str()];
        let n = fun.blocks.len();
        // Predecessors.
        let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (bi, bb) in fun.blocks.iter().enumerate() {
            for s in succs(&bb.terminator) {
                if s < n {
                    preds[s].push(bi);
                }
            }
        }
        // Forward fixpoint: fresh_in[b] = ∩ fresh_out[pred] (entry block: ∅).
        let mut fresh_in: Vec<HashSet<u32>> = vec![HashSet::new(); n];
        let mut fresh_out: Vec<HashSet<u32>> = (0..n).map(|b| transfer(ff, &alloc_like, &fresh_in[b], &fun.blocks[b])).collect();
        loop {
            let mut changed = false;
            for b in 0..n {
                let new_in: HashSet<u32> = if preds[b].is_empty() {
                    HashSet::new()
                } else {
                    let mut it = preds[b].iter();
                    let first = &fresh_out[*it.next().unwrap()];
                    it.fold(first.clone(), |acc, &p| acc.intersection(&fresh_out[p]).copied().collect())
                };
                if new_in != fresh_in[b] {
                    fresh_in[b] = new_in;
                    fresh_out[b] = transfer(ff, &alloc_like, &fresh_in[b], &fun.blocks[b]);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // Validate: re-walk each block from its fresh_in state.
        for (b, bb) in fun.blocks.iter().enumerate() {
            let mut fresh = fresh_in[b].clone();
            let value_safe = |op: &Operand, fresh: &HashSet<u32>| -> bool {
                match op {
                    Operand::ConstNull => true,
                    Operand::Copy(l) => fresh.contains(&l.0),
                    _ => false,
                }
            };
            for st in &bb.statements {
                let bad = match st {
                    Statement::PutField { class, field, value, .. } => {
                        let dangerous = field_target(class, field).is_some_and(|t| subtype_of_target(&t));
                        dangerous && !value_safe(value, &fresh)
                    }
                    Statement::PutStatic { value, .. } => {
                        !matches!(value, Operand::ConstNull) && !value_safe(value, &fresh)
                    }
                    Statement::ArrayStore { kind: fastllvm_ir::ArrKind::Ref, value, .. } => {
                        !matches!(value, Operand::ConstNull) && !value_safe(value, &fresh)
                    }
                    _ => false,
                };
                if bad {
                    if dbg {
                        eprintln!("[shape] FAIL in {}: {:?}", fun.name, st);
                    }
                    return false;
                }
                visit_ops(st, &mut |op| {
                    if let Operand::Copy(l) = op {
                        fresh.remove(&l.0);
                    }
                });
                match st {
                    Statement::New { dest, .. } | Statement::NewArray { dest, .. } => {
                        fresh.insert(dest.0);
                    }
                    Statement::Call { dest: Some(d), func, .. }
                    | Statement::CallGuarded { dest: Some(d), func, .. } => {
                        if alloc_like.contains(func.as_str()) {
                            fresh.insert(d.0);
                        } else {
                            fresh.remove(&d.0);
                        }
                    }
                    Statement::Assign(d, _)
                    | Statement::GetField { dest: d, .. }
                    | Statement::GetStatic { dest: d, .. }
                    | Statement::ArrayLoad { dest: d, .. } => {
                        fresh.remove(&d.0);
                    }
                    _ => {}
                }
            }
        }
    }
    true
}

/// Directed cycle search (white/gray/black DFS, iterative).
#[allow(dead_code)]
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
