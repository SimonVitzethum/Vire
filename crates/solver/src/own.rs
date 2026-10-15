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
#[derive(Debug, Clone, Default)]
pub struct OwnUse {
    pub consumes: u8,
    pub returned: bool,
    /// Block index of the consuming return (if `returned`).
    pub return_block: Option<usize>,
    /// True for New/FreshCall dests (own at creation); copies borrow.
    pub is_root: bool,
    /// Total consumers across the copy web, saturated at 2 (roots only).
    pub root_total: u8,
    /// Free at function exits (roots with 0 consumers; returned roots at all
    /// exits except their own return block).
    pub exit_free: bool,
    /// Exits to skip at cleanup (transferred there). Sorted block indices.
    pub skip_exits: Vec<usize>,
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
    /// `New` sites classified ownership-Shared: (function, statement-index) —
    /// E300 candidates (transfer-dominance-only sites are singly owned and
    /// do NOT count, though M2 keeps them on the RC path).
    pub shared_news: Vec<(String, usize)>,
    /// Functions containing a strong `Rc`-only reference cycle (AGENTS.md §2.6):
    /// with no collector nothing could reclaim it — break with `weak` or move
    /// to a ceiling. Reported per function (a cycle has no single site).
    pub rc_cycle_fns: Vec<String>,
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
    // (Includes the capsule deep-copy-IN functions: they copy the reachable
    // data into the callee arena and never retain the original — the caller's
    // ownership is unaffected, so the input stays singly owned. The arena copy
    // dies with the pop; only the original's lifetime counts for E300.
    // Includes the deep-copy-OUT functions too: they read the (arena) original
    // and return a fresh heap copy without retaining the original.)
    if matches!(
        func,
        "jrt_null_check"
            | "syscall"
            | "jrt_println_str"
            | "jrt_println_long"
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
            | "jrt_array_clone"
            | "jrt_deep_copy_arena"
            | "jrt_deep_copy_heap"
            | "jrt_arena_export_array"
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
    // `weak f: T` fields (owner class + name): non-owning edges — excluded from
    // consumer counting, cycle edges, and borrow promotion, exactly like the
    // solver's own ownership graph (their `cyclic_classes` exclusion).
    let mut weak = BTreeSet::new();
    for c in &program.classes {
        for fl in &c.fields {
            if fl.weak {
                weak.insert((c.name.clone(), fl.name.clone()));
            }
        }
    }
    // Fields and their classes, for parent-uniqueness of field stores.
    // Pool depths first (per function), then pool-context (interprocedural),
    // then per-function classification with both.
    let mut depths: BTreeMap<String, Vec<Vec<u32>>> = BTreeMap::new();
    for f in &program.functions {
        depths.insert(f.name.clone(), pool_depths(f));
    }
    let pctx = pool_ctx(program, &depths);
    for f in &program.functions {
        classify_function(f, &summ, &weak, &pctx, &mut rep);
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

/// Pool depth per (block, statement) for one function (see classify). explicit `region` / inferred arenas /
    // capsules): a `New` at depth > 0 allocates into the pool (immortal, freed
    // en bloc), never onto the heap — classify Region, not Unique/Shared. Same
    // dynamic extent the backend uses (push/pop calls); min-over-predecessors
    // fixpoint for entries, exact walk within blocks (pushes usually sit mid-
    // block, e.g. per-iteration loop-arenas — block-level depth would miss them
    // and wrongly report heap).
fn pool_depths(f: &Function) -> Vec<Vec<u32>> {
    {
        let nb = f.blocks.len();
        let succ_of = |bi: usize| -> Vec<usize> {
            match &f.blocks[bi].terminator {
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
        let delta_stmt = |st: &Statement| -> i32 {
            match st {
                Statement::Call { func, .. }
                    if func == "jrt_arena_push" || func == "jrt_region_push" =>
                {
                    1
                }
                Statement::Call { func, .. }
                    if func == "jrt_arena_pop" || func == "jrt_region_pop" =>
                {
                    -1
                }
                _ => 0,
            }
        };
        let mut pred: Vec<Vec<usize>> = vec![Vec::new(); nb];
        for bi in 0..nb {
            for s in succ_of(bi) {
                if s < nb {
                    pred[s].push(bi);
                }
            }
        }
        // Block-entry depths: min over predecessor exits (0 if none).
        // Block-entry depths: min over predecessor exits, from TOP (u32::MAX)
        // downward — initializing 0 would stick (0 is the bottom of min).
        // Unreachable entry (no preds, or still MAX) means depth 0.
        let mut entry = vec![u32::MAX; nb];
        if nb > 0 {
            entry[0] = 0;
        }
        {
            let mut changed = true;
            while changed {
                changed = false;
                for bi in 0..nb {
                    if bi == 0 {
                        continue;
                    }
                    // Undecided predecessors (still MAX) contribute +inf (skip),
                    // not 0: wrapping MAX to 0 would anchor every downstream
                    // cycle at 0 and the fixpoint could never rise.
                    let mut e = u32::MAX;
                    for p in &pred[bi] {
                        if entry[*p] == u32::MAX {
                            continue;
                        }
                        let mut d = entry[*p] as i32;
                        for st in &f.blocks[*p].statements {
                            d += delta_stmt(st);
                        }
                        e = e.min(d.max(0) as u32);
                    }
                    let e = if pred[bi].is_empty() || e == u32::MAX { 0 } else { e };
                    if e != entry[bi] {
                        entry[bi] = e;
                        changed = true;
                    }
                }
            }
        }
        // Walk each block, recording depth before every statement.
        let mut out: Vec<Vec<u32>> = Vec::with_capacity(nb);
        for bi in 0..nb {
            let mut depths = Vec::with_capacity(f.blocks[bi].statements.len());
            let mut d = entry[bi] as i32;
            for st in &f.blocks[bi].statements {
                depths.push(d.max(0) as u32);
                d += delta_stmt(st);
            }
            out.push(depths);
        }
        out
    }
}

/// Functions whose result may be heap-allocated (capsule exports), mirroring
/// the backend's `heap_escaping_fns`: such functions are never pool-context
/// (their result is real heap, not pool memory).
fn heap_esc_fns(program: &Program) -> BTreeSet<String> {
    const HEAP_EXPORTS: &[&str] = &["jrt_deep_copy_heap", "jrt_arena_export_array"];
    let mut esc: BTreeSet<String> = BTreeSet::new();
    loop {
        let mut changed = false;
        for f in &program.functions {
            if esc.contains(&f.name) {
                continue;
            }
            let escapes = f.blocks.iter().flat_map(|b| &b.statements).any(|st| match st {
                Statement::Call { func, .. } | Statement::CallGuarded { func, .. } => {
                    HEAP_EXPORTS.contains(&func.as_str()) || esc.contains(func)
                }
                _ => false,
            });
            if escapes {
                esc.insert(f.name.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    esc
}

/// Pool-context functions: every non-self direct call site executes at pool
/// depth > 0, so all their `New`s land in a pool (freed en bloc, never heap).
/// Mirrors the backend's `arena_ctx_fns` minus the safe-args rule (containment
/// of THEIR OWN allocations holds regardless of argument kinds — unlike the
/// backend's immortal-RESULT rule, which needs it). Dispatch-reachable
/// (methods, CallPoly targets) and heap-escaping functions are excluded.
/// `depths`: per-function pool depths from `pool_depths`.
fn pool_ctx(program: &Program, depths: &BTreeMap<String, Vec<Vec<u32>>>) -> BTreeSet<String> {
    let heap_esc = heap_esc_fns(program);
    let mut dispatch: BTreeSet<String> = BTreeSet::new();
    for c in &program.classes {
        for m in &c.methods {
            dispatch.insert(m.mangled.clone());
        }
    }
    for name in &heap_esc {
        dispatch.insert(name.clone());
    }
    // Callee -> direct call sites (caller, in-pool?). Self-sites inherit the
    // entry's depth, so only non-self sites gate context.
    struct Site {
        caller: String,
        in_pool: bool,
        is_self: bool,
    }
    let mut sites: BTreeMap<String, Vec<Site>> = BTreeMap::new();
    for f in &program.functions {
        let dd = &depths[&f.name];
        for (bi, bb) in f.blocks.iter().enumerate() {
            for (si, st) in bb.statements.iter().enumerate() {
                match st {
                    Statement::CallPoly { targets, .. } => {
                        for (_, sym) in targets {
                            dispatch.insert(sym.clone());
                        }
                    }
                    Statement::Call { func, .. } | Statement::CallGuarded { func, .. }
                        if !func.starts_with("jrt_") && program.functions.iter().any(|g| &g.name == func) =>
                    {
                        let in_pool = dd.get(bi).and_then(|v| v.get(si)).copied().unwrap_or(0) > 0;
                        sites.entry(func.clone()).or_default().push(Site {
                            caller: f.name.clone(),
                            in_pool,
                            is_self: func == &f.name,
                        });
                    }
                    _ => {}
                }
            }
        }
    }
    let mut ctx: BTreeSet<String> = BTreeSet::new();
    loop {
        let mut changed = false;
        for f in &program.functions {
            let name = &f.name;
            if ctx.contains(name) || dispatch.contains(name) {
                continue;
            }
            let Some(ss) = sites.get(name) else { continue };
            // A seed needs at least one non-self site; every non-self site must
            // run pooled itself or inside an already-pooled caller (fixpoint).
            // Self-sites inherit the entry depth and gate nothing.
            let non_self: Vec<&Site> = ss.iter().filter(|s| !s.is_self).collect();
            if non_self.is_empty() {
                continue;
            }
            if non_self.iter().all(|s| s.in_pool || ctx.contains(&s.caller)) {
                ctx.insert(name.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    ctx
}

fn classify_function(
    f: &Function,
    summ: &BTreeMap<String, Vec<bool>>,
    weak: &BTreeSet<(String, String)>,
    pctx: &BTreeSet<String>,
    rep: &mut OwnReport,
) {
    let n = f.locals.len();
    // Def site of each ref local: New (unique candidate), call dest (fresh or
    // alias), GetField (borrow candidate), param, copy, other.
    #[derive(Clone, Copy, PartialEq, Debug)]
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
    // Kernel `Rc$T` New dests: forced-shared roots (AGENTS.md §2.5). Collected
    // at def time (class is on the statement); applied at state init.
    let mut rc_forced = BTreeSet::new();
    for (i, t) in f.params.iter().enumerate() {
        if *t == Ty::Ref && (i as u32) < n as u32 {
            def[i] = Def::Param;
        }
    }
    let pool_depth: Vec<Vec<u32>> = pool_depths(f);


    // Statement index counter for shared-New reporting.
    let mut stmt_idx = 0usize;
    for (bi, b) in f.blocks.iter().enumerate() {
        for (si, st) in b.statements.iter().enumerate() {
            let pooled = pool_depth[bi][si] > 0;
            match st {
                Statement::New { dest, .. } if pooled => def[dest.0 as usize] = Def::Region,
                // Pool-context callees (called only from pools): their News land
                // in the caller's pool at runtime — Region, like textual depth.
                Statement::New { dest, .. } if pctx.contains(&f.name) => def[dest.0 as usize] = Def::Region,
                // Kernel `Rc$T` instances (AGENTS.md §2.5): forced-shared — the
                // explicit opt-in to counting. Never Unique/Borrow/Region, never
                // elided, never arena'd (pool-contained Rc News above stay pool:
                // en-bloc free subsumes counting while contained).
                Statement::New { dest, class, .. } if class.starts_with("Rc$") => {
                    def[dest.0 as usize] = Def::New;
                    rc_forced.insert(dest.0);
                }
                Statement::New { dest, .. } => def[dest.0 as usize] = Def::New,
                Statement::StackNew { dest, .. } | Statement::StackNewArray { dest, .. } | Statement::RegionNewArray { dest, .. } => {
                    def[dest.0 as usize] = Def::Region
                }
                Statement::NewArray { dest, .. } if pooled => def[dest.0 as usize] = Def::Region,
                Statement::NewArray { dest, .. } if pctx.contains(&f.name) => def[dest.0 as usize] = Def::Region,
                Statement::NewArray { dest, .. } => def[dest.0 as usize] = Def::New,
                Statement::Call { dest: Some(d), func, args, .. } | Statement::CallGuarded { dest: Some(d), func, args, .. } => {
                    // Fresh constructors yield Unique ownership; anything else may
                    // alias its arguments or global state → MaybeAlias. Inside a
                    // pool, a USER call with all-scalar (non-Ref) args yields pool
                    // memory (jrt_alloc routes by dynamic extent; no heap argument
                    // to pass through) → Region. With Ref args the result may
                    // alias a heap argument → MaybeAlias (conservative).
                    let args_scalar = args.iter().all(|a| match a {
                        Operand::Copy(l) => f.locals[l.0 as usize] != Ty::Ref,
                        _ => true,
                    });
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
                    def[d.0 as usize] = if pooled && !func.starts_with("jrt_") && args_scalar {
                        Def::Region
                    } else if fresh {
                        Def::FreshCall
                    } else {
                        Def::MaybeAlias
                    };
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
        // Forced-shared `Rc$T` roots first (explicit opt-in — never Unique even
        // though freshly allocated; the count is the semantics).
        if rc_forced.contains(&(i as u32)) {
            state[i] = OwnState::Shared;
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
    //
    // THREE SETS (AGENTS.md §2): `knock` feeds M2 codegen (any doubt → RC path,
    // including M2-decidability doubts like the transfer-dominance rule below);
    // `oother` + `oloop` feed E300 (ownership reasons only). `oother` = capture,
    // escape, cycles, multi-owner use. `oloop` = loop-carried overwrite (needs a
    // per-iteration release under RC). A conditionally-transferred single owner
    // is ownable (accepted by E300) even where M2 keeps the RC path
    // (conservative codegen). Loop-filled-into-owned collections are forgiven
    // separately (see `forgiven` at the E300 count below).
    let mut knock = BTreeSet::new();
    let mut oother = BTreeSet::new();
    let mut oloop = BTreeSet::new();
    let bad_use_of = |op: &Operand, knock: &mut BTreeSet<u32>, oother: &mut BTreeSet<u32>| {
        if let Operand::Copy(l) = op {
            knock.insert(l.0);
            oother.insert(l.0);
        }
    };
    for b in &f.blocks {
        for st in &b.statements {
            match st {
                Statement::Call { func, args, .. } | Statement::CallGuarded { func, args, .. } => {
                    let cap = call_captures(func, summ);
                    for (j, a) in args.iter().enumerate() {
                        if cap.get(j).copied().unwrap_or(true) {
                            bad_use_of(a, &mut knock, &mut oother);
                        }
                    }
                }
                // Unknown dispatch target → all ref args conservatively captured.
                Statement::CallVirtual { args, .. } | Statement::CallPoly { args, .. } => {
                    for a in args {
                        bad_use_of(a, &mut knock, &mut oother);
                    }
                }
                Statement::PutStatic { value, .. } => bad_use_of(value, &mut knock, &mut oother),
                Statement::PutField { obj, value, class, .. } => {
                    // Storing into a Shared parent shares the value. (Parent
                    // states resolve in the propagation fixpoint below; the
                    // direct cases are handled here, the rest iteratively.)
                    // Exception (E300-precision, M2-conservative): a store into
                    // an `Rc$` parent is owned-via-counting — the value rides
                    // the parent's lifetime. It knocks for M2 (RC path, as
                    // before) but NOT for E300 (no false positive on owned
                    // subgraphs of shared graphs).
                    if let Operand::Copy(o) = obj {
                        if state[o.0 as usize] == OwnState::Shared {
                            if let Operand::Copy(l) = value {
                                knock.insert(l.0);
                                if !class.starts_with("Rc$") {
                                    oother.insert(l.0);
                                }
                            }
                        }
                    }
                }
                Statement::ArrayStore { arr, value, .. } => {
                    if let Operand::Copy(a) = arr {
                        if state[a.0 as usize] == OwnState::Shared {
                            bad_use_of(value, &mut knock, &mut oother);
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
    //
    // EXEMPTION — exclusive-path merges (sound): an if/match value-merge assigns
    // its result local once per branch (`res = a` / `res = b`) — dynamically only
    // ONE of them executes, so the merged value still has exactly one owner.
    // Exempt a Unique local whose every (reachable) definition is a pure move
    // `Assign(d, Use(Copy))` and no definition block reaches another (the
    // if/match diamond; a loop-carried or straight-line double-assign reaches
    // itself or the other and stays knocked). Sources still knock through the
    // bidirectional copy propagation below, so a merge with a Shared arm knocks.
    {
        // Block successors (local copy — the shared one is computed below).
        let succ_of = |bi: usize| -> Vec<usize> {
            if bi >= f.blocks.len() {
                return vec![];
            }
            match &f.blocks[bi].terminator {
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
            }
        };
        // Blocks reachable from entry (dead-code defs never execute).
        let mut entry_reach = BTreeSet::from([0usize]);
        {
            let mut stack = vec![0usize];
            while let Some(bi) = stack.pop() {
                for s in succ_of(bi) {
                    if s < f.blocks.len() && entry_reach.insert(s) {
                        stack.push(s);
                    }
                }
            }
        }
        // `reaches(a, b)`: block a can reach block b (reflexive).
        let reaches = |a: usize, b: usize| -> bool {
            if a == b {
                return true;
            }
            let mut seen = BTreeSet::from([a]);
            let mut stack = vec![a];
            while let Some(x) = stack.pop() {
                for s in succ_of(x) {
                    if s == b {
                        return true;
                    }
                    if s < f.blocks.len() && seen.insert(s) {
                        stack.push(s);
                    }
                }
            }
            false
        };
        // Per-local: definition blocks + whether every definition is a pure move.
        // A `res = null` definition carries no ownership (null needs no owner),
        // so it is merge-compatible like a move (the match fallthrough arm).
        let mut def_blocks: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut all_pure = vec![true; n];
        for (bi, b) in f.blocks.iter().enumerate() {
            for st in &b.statements {
                let (dest, pure) = match st {
                    Statement::Assign(d, Rvalue::Use(Operand::Copy(_))) => (Some(d.0), true),
                    Statement::Assign(d, Rvalue::Use(Operand::ConstNull)) => (Some(d.0), true),
                    Statement::Assign(d, _) => (Some(d.0), false),
                    Statement::Call { dest: Some(d), .. }
                    | Statement::CallGuarded { dest: Some(d), .. }
                    | Statement::CallVirtual { dest: Some(d), .. }
                    | Statement::CallPoly { dest: Some(d), .. }
                    | Statement::New { dest: d, .. }
                    | Statement::StackNew { dest: d, .. }
                    | Statement::StackNewArray { dest: d, .. }
                    | Statement::RegionNewArray { dest: d, .. }
                    | Statement::NewArray { dest: d, .. }
                    | Statement::GetField { dest: d, .. } => (Some(d.0), false),
                    _ => (None, true),
                };
                if let Some(d) = dest {
                    def_blocks[d as usize].push(bi);
                    if !pure {
                        all_pure[d as usize] = false;
                    }
                }
            }
        }
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
            if *t != Ty::Ref || state[i] != OwnState::Unique || def_count[i] <= 1 {
                continue;
            }
            // Merge exemption: all live defs pure moves on mutually-exclusive paths.
            // (One or zero live defs: no dynamic reassignment at all — exempt.)
            //
            // The exemption applies to E300 (`oother`) ONLY: the merged value is
            // singly owned, so a dynamic-lifetime error would be a false
            // positive. M2 codegen (`knock`) stays conservative — its per-root
            // exit-free needs path decisions the merge does not give it (a
            // 0-consumer branch root would be freed at exit while the merged
            // value was returned). Same value, two lenses; both sound.
            let live_defs: Vec<usize> =
                def_blocks[i].iter().copied().filter(|b| entry_reach.contains(b)).collect();
            let exclusive = live_defs.len() <= 1
                || (all_pure[i] && {
                    let mut ok = true;
                    for (x, a) in live_defs.iter().enumerate() {
                        for b in live_defs.iter().skip(x + 1) {
                            if reaches(*a, *b) || reaches(*b, *a) {
                                ok = false;
                                break;
                            }
                        }
                        if !ok {
                            break;
                        }
                    }
                    ok
                });
            // NOT exclusive: genuinely reassigned → both sets (M2 + E300).
            if !exclusive {
                knock.insert(i as u32);
                oother.insert(i as u32);
            } else if live_defs.len() > 1 {
                // Exclusive merge: M2-conservative (RC path, as before), E300-clean.
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
    // Reachability + dominance (for transfer soundness): a store-transfer is
    // only valid if it dominates every reachable exit — otherwise one path
    // would double-free (parent frees) and another would leak (local owns).
    // Unreachable blocks never execute: their consumers don't count (the owner
    // keeps the value → exit-free), but knock rules still see them (Shared is
    // always a safe fallback).
    let n_blocks = f.blocks.len();
    let succ = |bi: usize| -> Vec<usize> {
        match &f.blocks[bi].terminator {
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
    let mut reachable = BTreeSet::from([0usize]);
    {
        let mut stack = vec![0usize];
        while let Some(bi) = stack.pop() {
            for s in succ(bi) {
                if s < n_blocks && reachable.insert(s) {
                    stack.push(s);
                }
            }
        }
    }
    let exits: BTreeSet<usize> = (0..n_blocks)
        .filter(|bi| reachable.contains(bi) && matches!(f.blocks[*bi].terminator, Terminator::Return(_)))
        .collect();
    // Iterative dominators over reachable blocks.
    let mut dom: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n_blocks];
    dom[0].insert(0);
    {
        let all: BTreeSet<usize> = reachable.iter().copied().collect();
        for bi in reachable.iter().skip_while(|b| **b == 0) {
            dom[*bi] = all.clone();
        }
        // Predecessors.
        let mut pred: Vec<Vec<usize>> = vec![Vec::new(); n_blocks];
        for bi in reachable.iter() {
            for s in succ(*bi) {
                if reachable.contains(&s) {
                    pred[s].push(*bi);
                }
            }
        }
        let mut changed = true;
        while changed {
            changed = false;
            for bi in reachable.iter().filter(|b| **b != 0) {
                let mut new = all.clone();
                for p in &pred[*bi] {
                    new = new.intersection(&dom[*p]).copied().collect();
                }
                new.insert(*bi);
                if new != dom[*bi] {
                    dom[*bi] = new;
                    changed = true;
                }
            }
        }
    }
    let dominates = |t: usize, e: usize| -> bool { dom[e].contains(&t) };
    // Forward reachability per block (for the transfer rule below).
    let mut fwd: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n_blocks];
    for bi in reachable.iter() {
        let mut seen = BTreeSet::from([*bi]);
        let mut stack = vec![*bi];
        while let Some(x) = stack.pop() {
            for s in succ(x) {
                if s < n_blocks && reachable.contains(&s) && seen.insert(s) {
                    stack.push(s);
                }
            }
        }
        fwd[*bi] = seen;
    }
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
    // Block index of the single consumer (store or return) per local.
    let mut cons_block: Vec<Option<usize>> = vec![None; n];
    // Per-root consumer block (store) / return block, for the dominance rule.
    let mut root_cons_block: Vec<Option<usize>> = vec![None; n];
    let mut root_ret_block: Vec<Option<usize>> = vec![None; n];
    let mut root_has_ret = vec![false; n];
    {
        // Consumer count per local: PutField/ArrayStore value into an owned
        // (Unique/Region) parent, or a returned operand. Unreachable blocks
        // never execute: consumers there don't count (the owner keeps the
        // value → exit-free is correct for them).
        let mut bad_root = BTreeSet::new();
        // Values stored into Shared `Rc$` parents: owned-via-counting (same
        // E300-precision/M2-conservative split as the direct check above —
        // knock for M2, never `oother` for E300). Tracked separately so the
        // web-knock below can split them.
        let mut bad_root_rc = BTreeSet::new();
        for (bi, b) in f.blocks.iter().enumerate() {
            let live = reachable.contains(&bi);
            for st in &b.statements {
                match st {
                    Statement::PutField { obj, value: Operand::Copy(v), class, field, .. } => {
                        // `weak` fields take no ownership (no transfer, no count):
                        // the source keeps its +1 — never a consumer, never an edge.
                        if weak.contains(&(class.clone(), field.clone())) {
                            continue;
                        }
                        // Only a Unique/Region parent keeps the value provably owned:
                        // a Shared OR Borrow parent has a lifetime we don't control
                        // (caller-owned) — storing there shares the value.
                        // (`Rc$` parents below: owned-via-counting, see `bad_root_rc`.)
                        let parent_owned = matches!(obj, Operand::Copy(o) if matches!(state[o.0 as usize], OwnState::Unique | OwnState::Region));
                        if parent_owned {
                            if live {
                                consumers[v.0 as usize] += 1;
                                cons_block[v.0 as usize] = Some(bi);
                                let r = root_of[v.0 as usize];
                                if r != u32::MAX {
                                    root_cons_block[r as usize] = Some(bi);
                                }
                            }
                        } else if class.starts_with("Rc$") {
                            bad_root_rc.insert(v.0);
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
                            if live {
                                consumers[v.0 as usize] += 1;
                                cons_block[v.0 as usize] = Some(bi);
                                let r = root_of[v.0 as usize];
                                if r != u32::MAX {
                                    root_cons_block[r as usize] = Some(bi);
                                }
                            }
                        } else {
                            bad_root.insert(v.0);
                        }
                    }
                    Statement::ArrayStore { .. } => {}
                    _ => {}
                }
            }
            if let Terminator::Return(Some(Operand::Copy(r))) = &b.terminator {
                // Return blocks are always reachable when executed; a return in an
                // unreachable block never transfers.
                if live {
                    consumers[r.0 as usize] += 1;
                    returned[r.0 as usize] = true;
                    cons_block[r.0 as usize] = Some(bi);
                    let rt = root_of[r.0 as usize];
                    if rt != u32::MAX {
                        root_has_ret[rt as usize] = true;
                        root_ret_block[rt as usize] = Some(bi);
                    }
                }
            }
        }
        // LOOP REASSIGNMENT (soundness-critical): a single static definition is
        // not a single dynamic execution — a local defined inside a loop body
        // is reassigned per iteration, so the old value's +1 must be released
        // per iteration (normal RC). A block is on a cycle iff one of its
        // successors reaches it back (`fwd` sets above — simple and exact).
        // Knocking the defined local (bidirectional propagation knocks its web)
        // restores per-iteration releases.
        {
            let succ_of = |bi: usize| -> Vec<usize> {
                let mut succs = match &f.blocks[bi].terminator {
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
                };
                succs.retain(|s| *s < n_blocks && reachable.contains(s));
                succs
            };
            let mut in_cycle = vec![false; n_blocks];
            for bi in reachable.iter() {
                for s in succ_of(*bi) {
                    if fwd[s].contains(bi) {
                        in_cycle[*bi] = true;
                        break;
                    }
                }
            }
            // Pool membership: locals that can only hold pool memory — Region defs,
            // copies thereof, and field/array loads off pool holders (pool objects
            // hold only pool refs by admission). Calls excluded (conservative:
            // a call result in a pool stays Shared and may count for E300).
            // Loop-knock skips members (no per-iteration release needed — the
            // pool frees en bloc); every OTHER knock rule still applies, so a
            // pool value escaping to the heap is flagged and E300-counted.
            let mut pool_mem = vec![false; n];
            for (i, t) in f.locals.iter().enumerate() {
                if *t == Ty::Ref && def[i] == Def::Region {
                    pool_mem[i] = true;
                }
            }
            loop {
                let mut grew = false;
                for b in &f.blocks {
                    for st in &b.statements {
                        match st {
                            Statement::Assign(dest, Rvalue::Use(Operand::Copy(s))) => {
                                if pool_mem[s.0 as usize] && !pool_mem[dest.0 as usize] {
                                    pool_mem[dest.0 as usize] = true;
                                    grew = true;
                                }
                            }
                            Statement::GetField { dest, obj: Operand::Copy(o), .. } => {
                                if pool_mem[o.0 as usize] && !pool_mem[dest.0 as usize] {
                                    pool_mem[dest.0 as usize] = true;
                                    grew = true;
                                }
                            }
                            Statement::ArrayLoad { dest, arr: Operand::Copy(a), .. } => {
                                if pool_mem[a.0 as usize] && !pool_mem[dest.0 as usize] {
                                    pool_mem[dest.0 as usize] = true;
                                    grew = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if !grew {
                    break;
                }
            }
            // Knock ref locals defined in loop blocks — EXCEPT pool members (see
            // above): per-iteration overwrites of pool refs need no release, and
            // must not cascade onto the pool roots (that would E300-count sound
            // pooling and reintroduce per-iteration RC traffic on no-op calls).
            //
            // TRAVERSAL exemption (E300 only): `cur = cur.next` re-points within
            // the SAME ownership web — no ownership changes hands, so no
            // per-iteration release is owed and E300 must not fire. Detected as
            // an in-cycle `d = s` where s's web root equals d's root (s itself a
            // same-root copy, or a same-block GetField off a same-root base).
            // M2 (`knock`) still knocks — only the E300 loop set skips.
            for bi in reachable.iter().filter(|b| in_cycle[**b]) {
                for st in &f.blocks[*bi].statements {
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
                        if f.locals[d as usize] == Ty::Ref && !pool_mem[d as usize] {
                            knock.insert(d);
                            // Traversal re-point? (E300 loop set skips; M2 knocks.)
                            let mut traversal = false;
                            if let Statement::Assign(_, Rvalue::Use(Operand::Copy(s))) = st {
                                let droot = root_of[d as usize];
                                if droot != u32::MAX {
                                    // Web root of the source: a same-block GetField
                                    // result resolves to its base's root, else its own.
                                    let mut sroot = root_of[s.0 as usize];
                                    if sroot == u32::MAX {
                                        for st2 in &f.blocks[*bi].statements {
                                            if let Statement::GetField { dest, obj: Operand::Copy(base), .. } = st2 {
                                                if dest.0 == s.0 {
                                                    sroot = root_of[base.0 as usize];
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    traversal = sroot == droot;
                                }
                            }
                            if !traversal {
                                oloop.insert(d);
                            }
                        }
                    }
                }
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
                    oother.insert(i as u32);
                }
            }
        }
        // TRANSFER RULE (soundness-critical for M2 codegen, NOT an ownership
        // verdict): a single transfer T is statically decidable per exit E iff
        // either T dominates E (all paths ran the transfer → parent/caller owns
        // → skip) or T cannot reach E (no path ran it → local owns-or-null →
        // free). If some paths run T and others avoid it (T reaches E without
        // dominating it), M2 keeps the RC path — but the value is still singly
        // owned, so E300 (`oother`) does NOT count it. Returns are a special
        // case for free (a return block reaches only itself, so other exits
        // always free).
        for (i, t) in f.locals.iter().enumerate() {
            if *t == Ty::Ref && state[i] == OwnState::Unique && root_of[i] as usize == i {
                if root_consumers[i] == 1 && !root_has_ret[i] {
                    match root_cons_block[i] {
                        Some(cb) => {
                            let ok = exits.iter().all(|e| dominates(cb, *e) || !fwd[cb].contains(e));
                            if !ok {
                                knock.insert(i as u32);
                            }
                        }
                        None => {}
                    }
                }
            }
        }
        // Values stored into Shared parents: knock their whole web via the root.
        for v in &bad_root {
            let r = root_of[*v as usize];
            if r != u32::MAX {
                knock.insert(r);
                oother.insert(r);
            } else {
                knock.insert(*v);
                oother.insert(*v);
            }
        }
        // Values stored into Shared `Rc$` parents: owned-via-counting — knock
        // the web for M2 (RC path, retains balance) but keep E300 clean (the
        // value rides the explicitly-counted parent's lifetime).
        for v in &bad_root_rc {
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
    // Pool-contained cycles (every member pool-defined) are EXEMPT: pools free
    // blindly en bloc without traversal, so cycles are irrelevant to them —
    // and the backend never direct-frees pool memory anyway (immortal path).
    // A cycle touching heap stays knocked (and E300-counted).
    {
        // Edge value-root → parent-root for stores between rooted values.
        // `weak` stores are no edges (non-owning back-edge, like the solver's
        // own graph exclusion). Self-stores ride the same map (a DFS that
        // returns to its start through any path, length ≥ 1).
        //
        // TRANSFER-shape stores form no edges: `New(p)` + same-block
        // `Assign(h, Copy(p))` (h ≠ p, carrier takeover) + same-block
        // `p.f = h` (old value into the new node, before the takeover).
        // At static-root granularity this is a self-edge, but dynamically it
        // is the push-front chain (each instance points to its predecessor —
        // acyclic); only a separate backward store (new into old, which never
        // matches this shape) can close a real cycle, and that edge is kept.
        // Without this, every `head = Node(head, i)` would read as a cycle.
        let mut transfer_store: BTreeSet<(usize, usize)> = BTreeSet::new();
        for (bi, b) in f.blocks.iter().enumerate() {
            // New-dests of this block.
            let news: BTreeSet<u32> = b
                .statements
                .iter()
                .filter_map(|st| match st {
                    Statement::New { dest, .. } => Some(dest.0),
                    _ => None,
                })
                .collect();
            if news.is_empty() {
                continue;
            }
            // Carrier takeovers `h = p` of those News in this block.
            let mut takeovers: Vec<(u32, u32)> = Vec::new();
            for st in &b.statements {
                if let Statement::Assign(dest, Rvalue::Use(Operand::Copy(s))) = st {
                    if news.contains(&s.0) && dest.0 != s.0 {
                        takeovers.push((dest.0, s.0));
                    }
                }
            }
            if takeovers.is_empty() {
                continue;
            }
            for (si, st) in b.statements.iter().enumerate() {
                let (p, v) = match st {
                    Statement::PutField { obj: Operand::Copy(o), value: Operand::Copy(v), .. } => (o.0, v.0),
                    Statement::ArrayStore { arr: Operand::Copy(a), value: Operand::Copy(v), .. } => (a.0, v.0),
                    _ => continue,
                };
                if news.contains(&p) && takeovers.iter().any(|(h, q)| *h == v && *q == p) {
                    transfer_store.insert((bi, si));
                }
            }
        }
        let mut edges: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
        for (bi, b) in f.blocks.iter().enumerate() {
            for (si, st) in b.statements.iter().enumerate() {
                if transfer_store.contains(&(bi, si)) {
                    continue;
                }
                let (parent, value) = match st {
                    Statement::PutField { obj: Operand::Copy(o), value: Operand::Copy(v), class, field, .. }
                        if !weak.contains(&(class.clone(), field.clone())) =>
                    {
                        (*o, *v)
                    }
                    Statement::ArrayStore { arr: Operand::Copy(a), value: Operand::Copy(v), .. } => (*a, *v),
                    _ => continue,
                };
                let (pr, vr) = (root_of[parent.0 as usize], root_of[value.0 as usize]);
                if pr != u32::MAX && vr != u32::MAX {
                    edges.entry(vr).or_default().insert(pr);
                }
            }
        }
        // Roots reaching themselves through edges (DFS per root; functions are
        // small, quadratic is fine). Pool-contained SCCs don't knock.
        let mut cyclic = BTreeSet::new();
        for b in &f.blocks {
            for st in &b.statements {
                let r = match st {
                    Statement::New { dest, .. } | Statement::NewArray { dest, .. } => dest.0,
                    _ => continue,
                };
                // DFS from r following edges, collecting the reached set and
                // whether the walk returns to r (through others or a self edge).
                let mut seen = BTreeSet::from([r]);
                let mut stack = vec![r];
                let mut reaches_self = false;
                while let Some(x) = stack.pop() {
                    if let Some(ns) = edges.get(&x) {
                        for y in ns {
                            if *y == r {
                                reaches_self = true;
                            }
                            if seen.insert(*y) {
                                stack.push(*y);
                            }
                        }
                    }
                }
                if reaches_self {
                    // Knock unless pool-contained (all reached members pool-defined).
                    let all_pooled = seen.iter().all(|m| {
                        def.get(*m as usize).is_some_and(|d| *d == Def::Region)
                    });
                    if !all_pooled {
                        cyclic.insert(r);
                    }
                    // Strong `Rc`-only cycles (AGENTS.md §2.6): with no collector
                    // there is nothing that could reclaim them — a silent leak.
                    // Every member forced-shared via `Rc$` (weak back-edges form
                    // no edges, so they never appear here). Reported separately
                    // (see `rc_cycle_fns`): break with `weak` or a ceiling.
                    // Mixed cycles already E300-fire through their plain members.
                    if reaches_self && !seen.is_empty() && seen.iter().all(|m| rc_forced.contains(m)) {
                        if !rep.rc_cycle_fns.contains(&f.name) {
                            rep.rc_cycle_fns.push(f.name.clone());
                        }
                    }
                }
            }
        }
        for r in cyclic {
            knock.insert(r);
            oother.insert(r);
        }
    }
    // Propagate knock-down through copies to fixpoint — BIDIRECTIONAL (soundness-
    // critical): a knocked copy shares its object, so the whole web (including
    // the root and earlier copies) is Shared. Forward-only propagation misses
    // the case of a directly-knocked copy (e.g. passed to a capturing call)
    // whose root would otherwise be exit-freed while the copy still holds a
    // retained +1 → release-after-free. Either direction knocks the web.
    // (Mirrored for `oother`/`oloop`: a knocked copy knocks its web in the same
    // source class, so E300 sees the whole dynamically-shared web, not just
    // the use site — and loop-only knock stays loop-only through copies.)
    loop {
        let mut grew = false;
        for b in &f.blocks {
            for st in &b.statements {
                if let Statement::Assign(dest, Rvalue::Use(Operand::Copy(s))) = st {
                    if knock.contains(&s.0) && knock.insert(dest.0) {
                        grew = true;
                    }
                    if knock.contains(&dest.0) && knock.insert(s.0) {
                        grew = true;
                    }
                    if oother.contains(&s.0) && oother.insert(dest.0) {
                        grew = true;
                    }
                    if oother.contains(&dest.0) && oother.insert(s.0) {
                        grew = true;
                    }
                    if oloop.contains(&s.0) && oloop.insert(dest.0) {
                        grew = true;
                    }
                    if oloop.contains(&dest.0) && oloop.insert(s.0) {
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
        // never written in this function (the field keeps the value alive) —
        // except `weak` fields, whose referent lifetime is not base-owned
        // (parent-pointer discipline, not checkable here).
        for b in &f.blocks {
            for st in &b.statements {
                if let Statement::GetField { dest, obj, class, field } = st {
                    let d = dest.0 as usize;
                    if f.locals[d] != Ty::Ref || state[d] != OwnState::Shared || knock.contains(&dest.0) {
                        continue;
                    }
                    if weak.contains(&(class.clone(), field.clone())) {
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
        // Disposition for codegen (only meaningful when Unique; the backend
        // ignores the rest and runs normal RC):
        // - root + 0 consumers → exit_free everywhere (frees what it owns);
        // - root + 1 store (decidable everywhere, enforced above) → skip
        //   dominated exits, free the rest (unreachable from T);
        // - root + 1 return at T → exit_free with skip at T (other exits free:
        //   their paths never ran the transfer);
        // - copies → skip every exit (borrows; the root disposes).
        let is_root = root_of[i] as usize == i && state[i] == OwnState::Unique;
        let (exit_free, skip_exits, ret_block, total) = if state[i] != OwnState::Unique {
            (false, Vec::new(), None, 0)
        } else if !is_root {
            // Copies never own: skip every exit (borrows; the root disposes).
            let mut sk: Vec<usize> = exits.iter().copied().collect();
            sk.sort_unstable();
            (false, sk, None, 0)
        } else {
            // Root: web total + the consuming return block, if any.
            let mut tot = 0u32;
            let mut rb = None;
            for (j, t) in f.locals.iter().enumerate() {
                if *t == Ty::Ref && root_of[j] as usize == i && consumers[j] > 0 {
                    tot += consumers[j];
                    if returned[j] {
                        rb = cons_block[j];
                    }
                }
            }
            if tot == 0 {
                (true, Vec::new(), None, 0)
            } else if let Some(t) = rb {
                // Return-transfer at T: skip T, free all other exits (their
                // paths never ran the transfer — T reaches only itself).
                (true, vec![t], Some(t), 1)
            } else {
                // Store-transfer (decidable everywhere by the rule above):
                // skip dominated exits, free the rest (unreachable from T).
                let cb = root_cons_block[i];
                let mut sk: Vec<usize> = exits
                    .iter()
                    .filter(|e| cb.is_some_and(|c| dominates(c, **e)))
                    .copied()
                    .collect();
                sk.sort_unstable();
                (false, sk, None, 1)
            }
        };
        rep.uses.insert(
            (f.name.clone(), i as u32),
            OwnUse {
                consumes: consumers[i].min(2) as u8,
                returned: returned[i],
                return_block: ret_block,
                is_root,
                root_total: total.min(2) as u8,
                exit_free,
                skip_exits: skip_exits,
            },
        );
    }
    // Shared New sites (E300 candidates): heap allocations with genuinely
    // dynamic lifetime — `oother`-knocked (capture, escape, cycles, multi-owner
    // use), or loop-knocked (`oloop`) without a static home. Transfer-dominance
    // (M2-codegen doubt) does NOT count: the value is still singly owned.
    // Pool-contained sites (Def::Region, never knocked for any escape-ish
    // reason) already HAVE their ceiling (the pool) and don't count.
    //
    // FORGIVEN (loop-allocated into owned homes): a loop-knocked `New` whose
    // value reaches an owned home unconditionally is array-/chain-owned (freed
    // with the home at any size — the unbounded-but-ownable class, AGENTS.md
    // §2.6). Two shapes, both same-block straight-line (⇒ unconditional):
    // (a) direct anchor: stored into an owned (Unique/Region) parent;
    // (c) carrier takeover (push-front idiom `head = Node(head, i)`): taken over
    // by a carrier `h`, whose old value the same block stored into the new node
    // BEFORE the takeover (old owned by new, new owned by carrier — a chain).
    // Copies (other than the single takeover), calls, returns, or stores
    // elsewhere (or a non-loop knock source) disqualify — doubt stays counted.
    // A direct anchor AND a takeover together are two homes (shared) — also out.
    // M2 (`knock`) is unaffected: the site keeps the RC path; only E300 is cleaned.
    let mut forgiven = BTreeSet::new();
    for (bi, b) in f.blocks.iter().enumerate() {
        for (si, st) in b.statements.iter().enumerate() {
            let d = match st {
                Statement::New { dest, .. } => dest.0,
                _ => continue,
            };
            if !oloop.contains(&d) || oother.contains(&d) {
                continue;
            }
            let uses_d = |op: &Operand| matches!(op, Operand::Copy(l) if l.0 == d);
            // Anchor: a same-block store of d into an owned parent.
            let mut anchored = false;
            // Carrier takeovers `h = d` (at most one; recorded with position).
            let mut takeovers: Vec<(u32, usize, usize)> = Vec::new();
            // All Copy(d) uses must be anchors, the single takeover, or reads.
            let mut clean = true;
            for (bi2, b2) in f.blocks.iter().enumerate() {
                for (si2, st2) in b2.statements.iter().enumerate() {
                    // The New itself defines d — not a use.
                    if bi2 == bi && si2 == si {
                        continue;
                    }
                    match st2 {
                        Statement::PutField { obj, value, .. } if uses_d(value) => {
                            let owned_parent = match obj {
                                Operand::Copy(p) => matches!(
                                    state[p.0 as usize],
                                    OwnState::Unique | OwnState::Region
                                ),
                                _ => false,
                            };
                            if bi2 == bi && owned_parent {
                                anchored = true;
                            } else {
                                clean = false;
                            }
                        }
                        Statement::ArrayStore { arr, value, .. } if uses_d(value) => {
                            let owned_parent = match arr {
                                Operand::Copy(p) => matches!(
                                    state[p.0 as usize],
                                    OwnState::Unique | OwnState::Region
                                ),
                                _ => false,
                            };
                            if bi2 == bi && owned_parent {
                                anchored = true;
                            } else {
                                clean = false;
                            }
                        }
                        // A carrier takeover `h = d` is validated below (needs
                        // the old-value transfer store); record, don't decide.
                        Statement::Assign(dest, Rvalue::Use(op))
                            if uses_d(op) && dest.0 != d =>
                        {
                            takeovers.push((dest.0, bi2, si2));
                        }
                        // Mutating d through a field/array base: no ownership effect.
                        Statement::PutField { obj, .. } if uses_d(obj) => {}
                        Statement::ArrayStore { arr, .. } if uses_d(arr) => {}
                        // Reads: field/array loads off d borrow.
                        Statement::GetField { obj, .. } if uses_d(obj) => {}
                        Statement::ArrayLoad { arr, .. } if uses_d(arr) => {}
                        _ => {
                            // Any other Copy(d) occurrence (call arg, other
                            // Assign shape, return, PutStatic, Binary, cond…) —
                            // be strict: conservatively disqualify. (Carrier
                            // chains like `mut t = Node(..); a[i] = t` stay
                            // counted: open precision, documented.)
                            let mut found = false;
                            match st2 {
                                Statement::Assign(_, r) => match r {
                                    Rvalue::Use(op) => {
                                        if uses_d(op) {
                                            found = true;
                                        }
                                    }
                                    Rvalue::Binary(_, a, c) => {
                                        if uses_d(a) || uses_d(c) {
                                            found = true;
                                        }
                                    }
                                    Rvalue::Convert(op) | Rvalue::Neg(op) => {
                                        if uses_d(op) {
                                            found = true;
                                        }
                                    }
                                },
                                Statement::Call { args, .. }
                                | Statement::CallGuarded { args, .. }
                                | Statement::CallVirtual { args, .. }
                                | Statement::CallPoly { args, .. } => {
                                    if args.iter().any(|a| uses_d(a)) {
                                        found = true;
                                    }
                                }
                                Statement::PutStatic { value, .. } => {
                                    if uses_d(value) {
                                        found = true;
                                    }
                                }
                                _ => {}
                            }
                            // Terminator returns of d also disqualify (strict).
                            if found {
                                clean = false;
                            }
                        }
                    }
                    if !clean {
                        break;
                    }
                }
                if !clean {
                    break;
                }
            }
            // Terminator check: `return d` disqualifies (strict).
            if clean {
                for b2 in &f.blocks {
                    if let Terminator::Return(Some(op)) = &b2.terminator {
                        if uses_d(op) {
                            clean = false;
                            break;
                        }
                    }
                }
            }
            // Carrier-takeover validation (c): exactly one `h = d`, same block
            // as the New, plus a same-block store of the old carrier value `h`
            // into the new node `d` positioned BEFORE the takeover (old owned
            // by new, new owned by carrier — straight-line ⇒ unconditional).
            // A direct anchor alongside means two homes (shared) — out.
            let mut took_over = false;
            if clean && !anchored && takeovers.len() == 1 {
                let (h, tbi, tsi) = takeovers[0];
                if tbi == bi {
                    for (ssi, sst) in b.statements.iter().enumerate() {
                        let transfer = match sst {
                            Statement::PutField { obj, value, .. } => {
                                matches!(obj, Operand::Copy(p) if p.0 == d)
                                    && matches!(value, Operand::Copy(v) if v.0 == h)
                            }
                            Statement::ArrayStore { arr, value, .. } => {
                                matches!(arr, Operand::Copy(p) if p.0 == d)
                                    && matches!(value, Operand::Copy(v) if v.0 == h)
                            }
                            _ => false,
                        };
                        if transfer && ssi < tsi {
                            took_over = true;
                            break;
                        }
                    }
                }
            }
            if clean && ((anchored && takeovers.is_empty()) || took_over) {
                forgiven.insert(d);
            }
        }
    }
    let mut idx = 0usize;
    for b in &f.blocks {
        for st in &b.statements {
            if let Statement::New { dest, class, .. } = st {
                // Explicit kernel `Rc$T` opt-ins never count (AGENTS.md §2.5):
                // sharing is the declared semantics, served by counting.
                // (Strong `Rc`-only cycles are reported separately above.)
                let counted = !class.starts_with("Rc$")
                    && (oother.contains(&dest.0)
                        || (oloop.contains(&dest.0) && !forgiven.contains(&dest.0)));
                if counted {
                    let pooled_clean =
                        def[dest.0 as usize] == Def::Region && !knock.contains(&dest.0);
                    if !pooled_clean {
                        rep.shared_news.push((f.name.clone(), idx));
                    }
                }
            }
            idx += 1;
        }
    }
}
