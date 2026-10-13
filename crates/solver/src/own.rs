//! Solver-side ownership classification — the borrowck offload (AGENTS.md §2,
//! OWNERSHIP-STATIC.md). ANALYSIS-ONLY: proves, per ref local, one of
//! Unique / Borrow / Region, and reports the rest as Shared (the E300
//! candidates). No transformation yet; the report behind `FASTLLVM_DUMP_OWN=1`
//! measures the provable fraction (EVALUATION.md §7.1: the one unmeasured number).
//!
//! Soundness direction: Unique/Borrow/Region must be *proven*; doubt yields
//! Shared. A wrong Unique would drop a real retain → use-after-free, so every
//! rule below is one-directional (admit only the provable shape).
//!
//! Rules (intraprocedural + capture summaries for calls):
//! - Region: `StackNew`/`StackNewArray`/`RegionNewArray` dests (frame/pool
//!   lifetime — escape analysis already proved them local).
//! - Unique: a `New`/`NewArray`/fresh-call dest whose value flows only through
//!   moves into Unique/Region parents (PutField/ArrayStore), `Return`, or a
//!   single live copy chain — never into a capturing call, a static, or a
//!   Shared parent; never merged with a Shared value; params start Shared.
//! - Borrow: never-reassigned ref params the function never captures (per
//!   summaries), plus GetField chains off Unique/Region/Borrow bases where the
//!   field is never written in this function.
//! - Shared: everything else (genuinely dynamic lifetimes → kernel `Rc` or a
//!   ceiling, never silent RC — see E300).

use std::collections::{BTreeMap, BTreeSet};

use fastllvm_ir::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnState {
    Unique,
    Borrow,
    Region,
    Shared,
}

/// Per-local use info for codegen: how many consuming uses (stores into owned
/// parents + returns, saturated at 2) and whether a return consumes it.
/// A Unique local with 0 consumes is freed at function exit; with 1 consume
/// ownership transfers there (store/return) and the exit skips it.
#[derive(Debug, Clone, Copy, Default)]
pub struct OwnUse {
    pub consumes: u8,
    pub returned: bool,
}

#[derive(Debug, Default)]
pub struct OwnReport {
    /// Per (function, local) ownership state (ref locals only).
    pub state: BTreeMap<(String, u32), OwnState>,
    /// Per (function, local) use info (ref locals only).
    pub uses: BTreeMap<(String, u32), OwnUse>,
    pub n_ref: usize,
    pub n_unique: usize,
    pub n_borrow: usize,
    pub n_region: usize,
    pub n_shared: usize,
    /// `New` sites classified Shared: (function, statement-index) — E300 candidates.
    pub shared_news: Vec<(String, usize)>,
}

/// Does ref argument `j` of a call to `func` possibly get captured (stored into
/// heap / returned inside heap / forwarded to a capturing call)? Unknown and
/// runtime functions capture unless allow-listed below. Keep in sync with
/// `escape.rs` (same question, same conservative default).
fn call_captures(func: &str, summaries: &BTreeMap<String, Vec<bool>>) -> Vec<bool> {
    if let Some(s) = summaries.get(func) {
        return s.clone();
    }
    // Known-pure runtime helpers: observe but never retain the ref.
    if matches!(
        func,
        "jrt_null_check"
            | "syscall"
            | "jrt_println_str"
            | "jrt_println_long"
            | "jrt_println_double"
            | "jrt_println_char"
            | "jrt_print_str"
            | "jrt_print_long"
            | "jrt_str_length"
            | "jrt_str_is_empty"
            | "jrt_str_char_at"
            | "jrt_str_equals"
            | "jrt_str_hashcode"
            | "jrt_str_indexof"
            | "jrt_str_startswith"
            | "jrt_str_endswith"
            | "jrt_str_compareto"
            | "jrt_array_data"
            | "jrt_array_len"
            | "vire_list_len"
            | "vire_map_len"
            | "vire_set_len"
            | "vire_set_contains"
            | "jrt_atomic_get"
            | "jrt_mutex_get"
    ) {
        return vec![false; 8];
    }
    // Fresh constructors: capture nothing, return fresh ownership.
    if matches!(
        func,
        "jrt_alloc"
            | "jrt_alloc_array"
            | "vire_list_new"
            | "vire_map_new"
            | "vire_set_new"
            | "jrt_str_concat"
            | "jrt_long_to_str"
            | "jrt_str_upper"
            | "jrt_str_lower"
            | "jrt_str_trim"
            | "jrt_str_substring1"
            | "jrt_chan_new"
            | "jrt_atomic_new"
            | "jrt_mutex_new"
    ) {
        return vec![false; 8];
    }
    // Everything else (user functions without a summary entry can't happen —
    // summaries cover all functions — but runtime mutators do): capture.
    vec![true; 8]
}

/// Fixpoint over the call graph: ref params that may be captured (stored to a
/// field/static/array, returned, or forwarded to a capturing call).
fn capture_summaries(functions: &[Function]) -> BTreeMap<String, Vec<bool>> {
    let mut summ: BTreeMap<String, Vec<bool>> = functions
        .iter()
        .map(|f| (f.name.clone(), vec![false; f.params.len()]))
        .collect();
    loop {
        let mut changed = false;
        for f in functions {
            for i in 0..f.params.len() {
                if f.params[i] != Ty::Ref || summ[&f.name][i] {
                    continue;
                }
                if param_captured(f, i as u32, &summ) {
                    summ.get_mut(&f.name).unwrap()[i] = true;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    summ
}

/// Is ref parameter `p` of `f` captured? Directly (field/static/array store,
/// return) or transitively (forwarded to a capturing call argument).
fn param_captured(f: &Function, p: u32, summ: &BTreeMap<String, Vec<bool>>) -> bool {
    // The copy-web of `p`: locals that may alias it (Assign-Use chains).
    let aliases = copy_aliases(f, p);
    let uses_param = |op: &Operand| matches!(op, Operand::Copy(l) if aliases.contains(&l.0));
    for b in &f.blocks {
        for st in &b.statements {
            match st {
                Statement::PutField { value, .. } if uses_param(value) => return true,
                Statement::PutStatic { value, .. } if uses_param(value) => return true,
                Statement::ArrayStore { value, .. } if uses_param(value) => return true,
                Statement::Call { func, args, .. } | Statement::CallGuarded { func, args, .. } => {
                    let cap = call_captures(func, summ);
                    for (j, a) in args.iter().enumerate() {
                        if uses_param(a) && cap.get(j).copied().unwrap_or(true) {
                            return true;
                        }
                    }
                }
                // Virtual/polymorphic dispatch: unknown target → conservative capture.
                Statement::CallVirtual { args, .. } | Statement::CallPoly { args, .. } => {
                    for a in args {
                        if uses_param(a) {
                            return true;
                        }
                    }
                }
                _ => {}
            }
        }
        match &b.terminator {
            // Returning the parameter hands ownership to the caller = captured.
            Terminator::Return(Some(op)) if uses_param(op) => return true,
            _ => {}
        }
    }
    false
}

/// Locals that may alias local `root` through `Assign(_, Use(Copy))` chains.
fn copy_aliases(f: &Function, root: u32) -> BTreeSet<u32> {
    let mut set = BTreeSet::from([root]);
    loop {
        let mut grew = false;
        for b in &f.blocks {
            for st in &b.statements {
                if let Statement::Assign(dest, Rvalue::Use(Operand::Copy(s))) = st {
                    if set.contains(&s.0) && set.insert(dest.0) {
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
    set
}

/// Fields written anywhere in `f` (by owner class + field name).
fn written_fields(f: &Function) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for b in &f.blocks {
        for st in &b.statements {
            if let Statement::PutField { class, field, .. } = st {
                out.insert((class.clone(), field.clone()));
            }
        }
    }
    out
}

pub fn classify(program: &Program) -> OwnReport {
    let mut rep = OwnReport::default();
    let summ = capture_summaries(&program.functions);
    // Fields and their classes, for parent-uniqueness of field stores.
    for f in &program.functions {
        classify_function(f, &summ, &mut rep);
    }
    rep.n_ref = rep.state.len();
    for s in rep.state.values() {
        match s {
            OwnState::Unique => rep.n_unique += 1,
            OwnState::Borrow => rep.n_borrow += 1,
            OwnState::Region => rep.n_region += 1,
            OwnState::Shared => rep.n_shared += 1,
        }
    }
    rep
}

fn classify_function(f: &Function, summ: &BTreeMap<String, Vec<bool>>, rep: &mut OwnReport) {
    let n = f.locals.len();
    // Def site of each ref local: New (unique candidate), call dest (fresh or
    // alias), GetField (borrow candidate), param, copy, other.
    #[derive(Clone, Copy, PartialEq)]
    enum Def {
        New,
        FreshCall,
        MaybeAlias,
        Param,
        Copy,
        Region,
        Other,
    }
    let mut def = vec![Def::Other; n];
    for (i, t) in f.params.iter().enumerate() {
        if *t == Ty::Ref && (i as u32) < n as u32 {
            def[i] = Def::Param;
        }
    }
    // Statement index counter for shared-New reporting.
    let mut stmt_idx = 0usize;
    for b in &f.blocks {
        for st in &b.statements {
            match st {
                Statement::New { dest, .. } => def[dest.0 as usize] = Def::New,
                Statement::StackNew { dest, .. } | Statement::StackNewArray { dest, .. } | Statement::RegionNewArray { dest, .. } => {
                    def[dest.0 as usize] = Def::Region
                }
                Statement::NewArray { dest, .. } => def[dest.0 as usize] = Def::New,
                Statement::Call { dest: Some(d), func, .. } | Statement::CallGuarded { dest: Some(d), func, .. } => {
                    // Fresh constructors yield Unique ownership; anything else may
                    // alias its arguments or global state → MaybeAlias.
                    let fresh = matches!(
                        func.as_str(),
                        "jrt_alloc"
                            | "jrt_alloc_array"
                            | "vire_list_new"
                            | "vire_map_new"
                            | "vire_set_new"
                            | "jrt_str_concat"
                            | "jrt_chan_new"
                            | "jrt_atomic_new"
                            | "jrt_mutex_new"
                    );
                    def[d.0 as usize] = if fresh { Def::FreshCall } else { Def::MaybeAlias };
                }
                Statement::CallVirtual { dest: Some(d), .. } | Statement::CallPoly { dest: Some(d), .. } => {
                    // Unknown dispatch target: the result may alias anything.
                    def[d.0 as usize] = Def::MaybeAlias;
                }
                Statement::GetField { dest, .. } => {
                    if def[dest.0 as usize] == Def::Other {
                        def[dest.0 as usize] = Def::MaybeAlias;
                    }
                }
                Statement::Assign(dest, Rvalue::Use(Operand::Copy(_))) => {
                    if def[dest.0 as usize] == Def::Other {
                        def[dest.0 as usize] = Def::Copy;
                    }
                }
                _ => {}
            }
            stmt_idx += 1;
        }
    }

    // Param reassignment: a reassigned param is no borrow root.
    let mut reassigned = BTreeSet::new();
    for b in &f.blocks {
        for st in &b.statements {
            match st {
                Statement::Assign(dest, _) => {
                    reassigned.insert(dest.0);
                }
                Statement::Call { dest: Some(d), .. }
                | Statement::CallGuarded { dest: Some(d), .. }
                | Statement::CallVirtual { dest: Some(d), .. }
                | Statement::CallPoly { dest: Some(d), .. }
                | Statement::New { dest: d, .. }
                | Statement::StackNew { dest: d, .. }
                | Statement::StackNewArray { dest: d, .. }
                | Statement::RegionNewArray { dest: d, .. }
                | Statement::NewArray { dest: d, .. }
                | Statement::GetField { dest: d, .. } => {
                    reassigned.insert(d.0);
                }
                _ => {}
            }
        }
    }

    let written = written_fields(f);
    let np = f.params.len() as u32;

    // Per-local state, starting optimistic for New/FreshCall, then knocking
    // down to Shared on any unprovable use. Params start Shared (promoted to
    // Borrow below if never reassigned and never captured).
    let mut state = vec![OwnState::Shared; n];
    for (i, t) in f.locals.iter().enumerate() {
        if *t != Ty::Ref {
            continue;
        }
        match def[i] {
            Def::New | Def::FreshCall => state[i] = OwnState::Unique,
            Def::Region => state[i] = OwnState::Region,
            _ => state[i] = OwnState::Shared,
        }
    }
    // Copy-class resolution: a copy of a Unique/Region/Borrow local starts in
    // the same state (knocked down below if misused). Fixpoint over chains.
    loop {
        let mut grew = false;
        for b in &f.blocks {
            for st in &b.statements {
                if let Statement::Assign(dest, Rvalue::Use(Operand::Copy(s))) = st {
                    let (d, s) = (dest.0 as usize, s.0 as usize);
                    if f.locals[d] == Ty::Ref && state[d] == OwnState::Shared && state[s] != OwnState::Shared {
                        state[d] = state[s];
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }

    // Knock-down: any use that breaks uniqueness/borrow makes the value Shared
    // (propagated forward through copies at the end).
    let mut knock = BTreeSet::new();
    let bad_use_of = |op: &Operand, knock: &mut BTreeSet<u32>| {
        if let Operand::Copy(l) = op {
            knock.insert(l.0);
        }
    };
    for b in &f.blocks {
        for st in &b.statements {
            match st {
                Statement::Call { func, args, .. } | Statement::CallGuarded { func, args, .. } => {
                    let cap = call_captures(func, summ);
                    for (j, a) in args.iter().enumerate() {
                        if cap.get(j).copied().unwrap_or(true) {
                            bad_use_of(a, &mut knock);
                        }
                    }
                }
                // Unknown dispatch target → all ref args conservatively captured.
                Statement::CallVirtual { args, .. } | Statement::CallPoly { args, .. } => {
                    for a in args {
                        bad_use_of(a, &mut knock);
                    }
                }
                Statement::PutStatic { value, .. } => bad_use_of(value, &mut knock),
                Statement::PutField { obj, value, .. } => {
                    // Storing into a Shared parent shares the value. (Parent
                    // states resolve in the propagation fixpoint below; the
                    // direct cases are handled here, the rest iteratively.)
                    if let Operand::Copy(o) = obj {
                        if state[o.0 as usize] == OwnState::Shared {
                            bad_use_of(value, &mut knock);
                        }
                    }
                }
                Statement::ArrayStore { arr, value, .. } => {
                    if let Operand::Copy(a) = arr {
                        if state[a.0 as usize] == OwnState::Shared {
                            bad_use_of(value, &mut knock);
                        }
                    }
                }
                _ => {}
            }
        }
        // Merged control flow is approximated by the reassignment rule below
        // (a Unique local assigned twice → Shared).
    }
    // Reassignment kills uniqueness (the old value's owner is unclear without
    // release tracking — conservative): a Unique local assigned twice → Shared,
    // except the single defining statement.
    {
        let mut def_count = vec![0u32; n];
        for b in &f.blocks {
            for st in &b.statements {
                let dest = match st {
                    Statement::Assign(d, _) => Some(d.0),
                    Statement::Call { dest: Some(d), .. }
                    | Statement::CallGuarded { dest: Some(d), .. }
                    | Statement::CallVirtual { dest: Some(d), .. }
                    | Statement::CallPoly { dest: Some(d), .. }
                    | Statement::New { dest: d, .. }
                    | Statement::StackNew { dest: d, .. }
                    | Statement::StackNewArray { dest: d, .. }
                    | Statement::RegionNewArray { dest: d, .. }
                    | Statement::NewArray { dest: d, .. }
                    | Statement::GetField { dest: d, .. } => Some(d.0),
                    _ => None,
                };
                if let Some(d) = dest {
                    def_count[d as usize] += 1;
                }
            }
        }
        for (i, t) in f.locals.iter().enumerate() {
            if *t == Ty::Ref && state[i] == OwnState::Unique && def_count[i] > 1 {
                knock.insert(i as u32);
            }
        }
    }
    let _ = (stmt_idx, np);
    // Root map: each local → its defining New/FreshCall root (u32::MAX = none).
    // Shared by the linearity and cycle rules below.
    let root_of = {
        let mut root_of = vec![u32::MAX; n];
        for b in &f.blocks {
            for st in &b.statements {
                match st {
                    Statement::New { dest, .. } | Statement::NewArray { dest, .. } => {
                        root_of[dest.0 as usize] = dest.0;
                    }
                    Statement::Call { dest: Some(d), .. }
                    | Statement::CallGuarded { dest: Some(d), .. }
                    | Statement::CallVirtual { dest: Some(d), .. }
                    | Statement::CallPoly { dest: Some(d), .. } => {
                        if state[d.0 as usize] == OwnState::Unique {
                            root_of[d.0 as usize] = d.0;
                        }
                    }
                    Statement::Assign(dest, Rvalue::Use(Operand::Copy(s))) => {
                        if root_of[s.0 as usize] != u32::MAX && root_of[dest.0 as usize] == u32::MAX {
                            root_of[dest.0 as usize] = root_of[s.0 as usize];
                        }
                    }
                    _ => {}
                }
            }
        }
        root_of
    };
    // LINEARITY (soundness-critical for direct-free codegen): a Unique object must
    // have AT MOST ONE consuming use across its whole copy web — a store into a
    // non-Shared parent, a return, or (implicitly) the function-exit cleanup.
    // Reads (GetField base, non-capturing call args, conditions) are free.
    // Two consumers (`store(x); return x`, or two live copies each consumed)
    // would free twice → Shared. Computed per New/FreshCall root over its web.
    // `consumers`/`returned` are hoisted: the backend needs per-local use info
    // (transfer vs exit-free) alongside the state map.
    let mut consumers = vec![0u32; n];
    let mut returned = vec![false; n];
    {
        // Consumer count per local: PutField/ArrayStore value into an owned
        // (Unique/Region) parent, or a returned operand.
        let mut bad_root = BTreeSet::new();
        for b in &f.blocks {
            for st in &b.statements {
                match st {
                    Statement::PutField { obj, value: Operand::Copy(v), .. } => {
                        // Only a Unique/Region parent keeps the value provably owned:
                        // a Shared OR Borrow parent has a lifetime we don't control
                        // (caller-owned) — storing there shares the value.
                        let parent_owned = matches!(obj, Operand::Copy(o) if matches!(state[o.0 as usize], OwnState::Unique | OwnState::Region));
                        if parent_owned {
                            consumers[v.0 as usize] += 1;
                        } else {
                            bad_root.insert(v.0);
                        }
                    }
                    Statement::PutField { value, .. } => {
                        // Non-copy value (null/const): no owner involved.
                        let _ = value;
                    }
                    Statement::ArrayStore { arr, value: Operand::Copy(v), .. } => {
                        let parent_owned = matches!(arr, Operand::Copy(a) if matches!(state[a.0 as usize], OwnState::Unique | OwnState::Region));
                        if parent_owned {
                            consumers[v.0 as usize] += 1;
                        } else {
                            bad_root.insert(v.0);
                        }
                    }
                    Statement::ArrayStore { .. } => {}
                    _ => {}
                }
            }
            if let Terminator::Return(Some(Operand::Copy(r))) = &b.terminator {
                consumers[r.0 as usize] += 1;
                returned[r.0 as usize] = true;
            }
        }
        // Attribute each local's consumers to its New/FreshCall root (follow the
        // copy chain backwards to the defining root; see `root_of` above).
        let mut root_consumers = vec![0u32; n];
        for (i, t) in f.locals.iter().enumerate() {
            if *t == Ty::Ref && consumers[i] > 0 {
                let r = root_of[i];
                if r != u32::MAX {
                    root_consumers[r as usize] += consumers[i];
                } else {
                    // Consumed value with no Unique root (param field-store):
                    // nothing to do (params are Shared/Borrow, handled by RC).
                }
            }
        }
        for (i, t) in f.locals.iter().enumerate() {
            if *t == Ty::Ref && state[i] == OwnState::Unique && root_of[i] as usize == i {
                if root_consumers[i] > 1 {
                    knock.insert(i as u32);
                }
            }
        }
        // Values stored into Shared parents: knock their whole web via the root.
        for v in &bad_root {
            let r = root_of[*v as usize];
            if r != u32::MAX {
                knock.insert(r);
            } else {
                knock.insert(*v);
            }
        }
    }
    // CYCLE DETECTION (soundness-critical): a Unique web whose stores form a
    // reference cycle can never be freed by direct frees (free(a) → free(b) →
    // free(a) = double free; or a leak). Knock every root that reaches itself
    // through parent→value store edges. Type-level cyclicity is too coarse
    // (trees are Node-typed but acyclic instances) — this is instance-level.
    {
        // Edge value-root → parent-root for stores between rooted values.
        let mut edges: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
        for b in &f.blocks {
            for st in &b.statements {
                let (parent, value) = match st {
                    Statement::PutField { obj: Operand::Copy(o), value: Operand::Copy(v), .. } => (*o, *v),
                    Statement::ArrayStore { arr: Operand::Copy(a), value: Operand::Copy(v), .. } => (*a, *v),
                    _ => continue,
                };
                let (pr, vr) = (root_of[parent.0 as usize], root_of[value.0 as usize]);
                if pr != u32::MAX && vr != u32::MAX && pr != vr {
                    edges.entry(vr).or_default().insert(pr);
                } else if pr != u32::MAX && pr == vr {
                    // Self-store (`a.next = a`): immediate cycle.
                    knock.insert(pr);
                }
            }
        }
        // Roots reaching themselves through edges (DFS per root; functions are
        // small, quadratic is fine).
        let mut cyclic = BTreeSet::new();
        for b in &f.blocks {
            for st in &b.statements {
                let r = match st {
                    Statement::New { dest, .. } | Statement::NewArray { dest, .. } => dest.0,
                    _ => continue,
                };
                // DFS from r following edges.
                let mut seen = BTreeSet::from([r]);
                let mut stack = vec![r];
                while let Some(x) = stack.pop() {
                    if let Some(ns) = edges.get(&x) {
                        for y in ns {
                            if *y == r {
                                cyclic.insert(r);
                            }
                            if seen.insert(*y) {
                                stack.push(*y);
                            }
                        }
                    }
                }
            }
        }
        for r in cyclic {
            knock.insert(r);
        }
    }
    // Propagate the cycle knock-down through copies to fixpoint.
    loop {
        let mut grew = false;
        for b in &f.blocks {
            for st in &b.statements {
                if let Statement::Assign(dest, Rvalue::Use(Operand::Copy(s))) = st {
                    if knock.contains(&s.0) && knock.insert(dest.0) {
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
    for k in &knock {
        if f.locals[*k as usize] == Ty::Ref {
            state[*k as usize] = OwnState::Shared;
        }
    }
    loop {
        let mut grew = false;
        for b in &f.blocks {
            for st in &b.statements {
                if let Statement::Assign(dest, Rvalue::Use(Operand::Copy(s))) = st {
                    if knock.contains(&s.0) && knock.insert(dest.0) {
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
    for k in &knock {
        state[*k as usize] = OwnState::Shared;
    }

    // Borrow promotion: never-reassigned, never-captured ref params that were
    // NOT knocked (a knocked use — e.g. passing it to a capturing call — breaks
    // the borrow), plus GetField chains off non-Shared bases with no local
    // write to the field. Iterated with copy-propagation to fixpoint.
    loop {
        let mut grew = false;
        for (i, t) in f.locals.iter().enumerate() {
            if *t != Ty::Ref {
                continue;
            }
            if (i as u32) < np
                && !reassigned.contains(&(i as u32))
                && !knock.contains(&(i as u32))
                && state[i] == OwnState::Shared
            {
                let captured = summ.get(&f.name).and_then(|s| s.get(i)).copied().unwrap_or(true);
                if !captured {
                    state[i] = OwnState::Borrow;
                    grew = true;
                }
            }
        }
        // GetField dest promotion: base must be proven non-Shared and the field
        // never written in this function (the field keeps the value alive).
        for b in &f.blocks {
            for st in &b.statements {
                if let Statement::GetField { dest, obj, class, field } = st {
                    let d = dest.0 as usize;
                    if f.locals[d] != Ty::Ref || state[d] != OwnState::Shared || knock.contains(&dest.0) {
                        continue;
                    }
                    let base_ok = matches!(obj, Operand::Copy(l) if state[l.0 as usize] != OwnState::Shared);
                    if base_ok && !written.contains(&(class.clone(), field.clone())) {
                        state[d] = OwnState::Borrow;
                        grew = true;
                    }
                }
            }
        }
        // Copies of promoted locals follow their source — unless knocked (a
        // knocked use breaks ownership; never promote a knocked local).
        for b in &f.blocks {
            for st in &b.statements {
                if let Statement::Assign(dest, Rvalue::Use(Operand::Copy(s))) = st {
                    let (d, s) = (dest.0 as usize, s.0 as usize);
                    if f.locals[d] == Ty::Ref && state[d] == OwnState::Shared && state[s] != OwnState::Shared && !knock.contains(&dest.0) {
                        state[d] = state[s];
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }

    for (i, t) in f.locals.iter().enumerate() {
        if *t != Ty::Ref {
            continue;
        }
        rep.state.insert((f.name.clone(), i as u32), state[i]);
        rep.uses.insert(
            (f.name.clone(), i as u32),
            OwnUse { consumes: consumers[i].min(2) as u8, returned: returned[i] },
        );
    }
    // Shared New sites (E300 candidates).
    let mut idx = 0usize;
    for b in &f.blocks {
        for st in &b.statements {
            if let Statement::New { dest, .. } = st {
                if state[dest.0 as usize] == OwnState::Shared {
                    rep.shared_news.push((f.name.clone(), idx));
                }
            }
            idx += 1;
        }
    }
}
