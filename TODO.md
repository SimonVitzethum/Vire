# Vire — Roadmap (open work)

Only **open** and **partial** items. Completed work lives in [DONE.md](DONE.md).
Legend: `[ ]` open · `[~]` partial. Design basis: [language/](language/).

## Current state (2026-07)

The whole pipeline is functional and green (lexer → parser → macro/comptime →
inline → inference → SSA IR → whole-program solver → LLVM `-O2 -flto -march=native`).
Performance sits at **geomean ~1.00× Rust** across 12 Vire benchmarks — compute-bound
at parity or faster, virtual dispatch 2.4× faster than clang. What shipped is in
[DONE.md](DONE.md); the remaining headroom is captured in the Performance Push below.

Soundness floor (never waived): Java heap-balance oracle **65/65** +
`tests/vire_heap.sh` 0-live + all `tests/vire_*.sh` green after every change.

---

# ⚡ PERFORMANCE PUSH — TOP PRIORITY (2-month plan)

**Goal: maximum performance without losing memory safety.** Every item is gated by
the 65/65 heap oracle + 0-live. **Execution order: Tier 4 → Tier 1 → Tier 2 →
Tier 3.** (Tier 4 first per decision 2026-07-22.)

Baseline is already Rust-parity, so the achievable delta is: (1) capture the one
structural ~2× lever (auto-arena for alloc-bound graphs), (2) mop up the last few
>1.1× residuals to parity, (3) plant vectorization enablers — plus (Tier 4, first)
the GPU-track wins.

**Do NOT re-attempt (measured dead — see [DONE.md](DONE.md)):** RC-inline-as-IR
(costs `-flto` inlining of other hot helpers), per-access `noalias` for
latency-bound benches (graph/sort), node-pool/SoA rewrite (slower), hand
regalloc/scheduling tuning for raytracer (low ROI, no single pass).

## Tier 4 — GPU track (DO FIRST) — separate track, CPU suite untouched

- [x] **Device-module middle-end (`opt -O3` before `llc`)** — DONE. The NVPTX
  emitter produces naive alloca-per-local IR; `llc` alone skips the
  target-independent mid-end, so loop scalars could hit slow `.local` memory. The
  build now runs `opt -O3` on the device module first (saxpy: 13 allocas → 0,
  register-promoted). Best-effort fallback if `opt` absent. See
  [benchmarks/gpu/VS-CUDA-OXIDE.md](benchmarks/gpu/VS-CUDA-OXIDE.md).
- [x] **Read-only array analysis** — DONE. `read_only_params` proves which array
  params a kernel never stores into and skips their D2H copyback (sound: an
  untraceable base forces in/out). Verified bit-exact (saxpy `x` skips, `y`
  downloads). *Still open below: write-only H2D elision.*
- [ ] **Write-only H2D elision + persistent context / async** — skip the *upload*
  for output-only buffers; reuse device buffers across launches; a non-synchronous
  launch path (v1 syncs every launch). *Removes per-launch malloc/free + sync
  overhead across repeated kernels.*
- [ ] **Explicit launch config** — let a kernel/call choose block size / 2-D & 3-D
  grids + shared memory, instead of fixed `block=256, grid=ceil(N/256)`.
- [ ] **Sub-word + Ref arrays on device**, `Array<F32>` scalars, device-side math
  intrinsics (sqrt/exp via `@llvm.nvvm.*`).
- [ ] **Fair Rust-GPU baseline** — build cuda-oxide (needs its rustc backend
  toolchain) to fill the Vire-GPU vs Rust-GPU column in benchmarks/gpu.

## Tier 1 — the structural ~2× lever (highest ceiling)

- [x] **RC elided on provably-arena refs → sharedgraph WON (2026-07-24).** After region
  inference the arena fired but the emitted LLVM still called `jrt_retain`/`jrt_release` at
  ~89 hot sites on arena-immortal objects (each a runtime no-op via the rc<0 early return,
  but the call traffic cost 282 ms). Bisection (arena fires in all): normal 352, `--no-cycles`
  154, `--no-rc` 70 ms. Built the compile-time elision in `crates/backend/src/lib.rs`:
  - `arena_immortal_dests` + `arena_block_depths`: a New/NewArray at arena-depth>0 (CFG
    dataflow, min-over-predecessors) is an immortal seed; a user call there with immortal
    args returns arena memory.
  - `arena_ctx_fns`: an interprocedural least fixpoint — a function whose every non-self
    direct call site is in-arena (with scalar/immortal args), that is not dispatch-reachable
    (method / CallPoly target) and not heap-escaping, ALWAYS runs in an arena, so its New
    and ref params are immortal. This captures the recursive builders (`chain`, `make`) that
    have no `arena_push` of their own.
  - `heap_escaping_fns`: functions transitively calling `jrt_deep_copy_heap` /
    `jrt_arena_export_array` (capsule exports) are excluded — their result is real heap.
  - GetField-from-immortal is immortal; PutField skips retain(v) when v immortal and
    release(old) when the object is immortal.
  - Result: **sharedgraph 352 → 75 ms = 0.51× Rust** (the class from the original 5.0× to
    0.51×); **binary-trees 205 → 46 ms** (make() is arena-context, RC 108 → 3). 0 heap, 0
    collector, correct, 0-live, GUARD_FREE-clean. Gate GREEN (Java 67/67), fuzzer, and
    `vire_heap`/`vire_interproc_arena` soundness cases (builder called in+out of arena must
    NOT be arena-context — pins the no-UAF boundary).

- [x] **Automatic region/arena inference for short-lived heap graphs — ref-mutation
  case DONE (2026-07).** The loop-arena already captured recursive build/use/drop; the
  gap was ref-storing field mutation `obj.f = ref` (topology mutation, cycles). Now
  admitted when `obj` AND `ref` are provably iteration-fresh (`loop_fresh_locals` /
  `expr_is_fresh`, greatest-fixpoint over the loop body). On sharedgraph the arena fires:
  729 → 352 ms (2.08×; 5.0× → 2.4× Rust), 0 heap allocs, 0 collector — 0-live +
  GUARD_FREE-clean, pinned both directions in `vire_interproc_arena.sh` (+3 cases).
  **Still open (smaller):** (a) the same relaxation inside CALLEES (currently only the
  loop's own function; needs the freshness domain extended interprocedurally), (b)
  ref-*array* element stores `a[i] = ref` on a fresh array, (c) the residual ~2.4× to
  Rust's `Rc` is a separate bare-allocation codegen gap the arena does not touch.
  Original framing (kept for context): capture the build→consume→drop pattern soundly
  without the user writing `capsule`, freeing the subgraph en bloc.
  - Extends a proven mechanism: thread-local `arena_top`, `while_arena_safe`
    interprocedural escape check, `tests/vire_interproc_arena.sh`, 0-live oracle all
    already exist — this generalizes the trigger from explicit `capsule` to inferred.
  - **Diagnosis CORRECTED by measurement (2026-07, [benchmarks/complex/sharedgraph.vr](benchmarks/complex/sharedgraph.vr)):**
    the earlier revert note blamed recursion — that is **wrong**. `region_bad`
    already admits recursive builders (its `seen` set makes a self-recursive call
    return `false`), and the loop-arena *already fires* on a recursive build/use/drop:
    a non-mutating variant of sharedgraph emits `jrt_arena_push`/`pop` around a
    recursive `chain(20)` and allocates **0 heap objects** (all 8M nodes immortal in
    the arena). The one thing that suppresses the arena on the cyclic benchmark is the
    **ref-storing field mutation** `last.next = h` — `region_bad_stmt` bails on any
    `obj.f = <ref>` because it cannot prove the base `obj` is arena-local. So the real
    work is narrow and precise: **admit `obj.f = ref` in the arena when `obj` AND the
    stored `ref` are provably iteration-fresh** (a forward freshness dataflow over the
    loop body, seeded by constructor/arena-returning-call results, closed under
    field-read + copy). Both fresh ⇒ both in the arena ⇒ freed together ⇒ no dangle, no
    leak. Measured payoff: 729 ms → ~273 ms ceiling (5.0× → ~1.9× Rust). Start in the
    loop's own function (`in_callee = false`) only; keep callee mutations conservative.
    **Soundness-critical** (a wrong freshness verdict = use-after-free after
    `arena_pop`): gate on the 0-live oracle + `GUARD_FREE` (temporal) + the fuzzer, and
    pin the `vire_interproc_arena` push-count invariant.
  - **Arena fixed costs — chunk recycling DONE** (`jrt_arena_pop`/`arena_alloc`):
    standard 64 KiB chunks are recycled through a capped per-thread free-list instead
    of `free()`d at each pop — removes the O(chunks) free burst (a latency spike) and
    the per-capsule chunk malloc. (Larger-chunk tuning still open.)
  - **Soundness-critical** (a wrong escape verdict = use-after-free): pin promote
    *and* decline in both directions with new cases in
    `tests/vire_interproc_arena.sh` before enabling by default.
  - *Effort ~4–6 wk. This is the one place with real structural headroom.*

## Tier 2 — targeted, safe, medium ROI

- [x] **NBody — already at parity (2026-07, re-measured).** The "1.16× / seven
  double[] reloads" entry was stale (predates the `Math.sqrt`→`sqrtsd` intrinsic + fma
  scheduling). Measured now: fastjavac 1211 ms vs a faithful Rust port 1165 ms = **1.04×**,
  identical output. Disassembly of `advance()`: the 7 array base pointers are already kept
  in registers and the body uses `vfmadd` with no visible redundant reloads. The
  scoped-noalias work is not warranted — there is no reload pathology left to remove.
  (Note kept: inlining `advance` makes it *worse*, 7.5× — do not.)
- [ ] **(M0.3-iv) Field-array bounds elision** — exact gap located (2026-07-24), sound
  fix scoped, NOT built (unexercised + disproportionate). `bounds.rs` GVN gives every
  `GetField` a fresh per-site `Opaque` sym, so `x.arr.len()` in a loop guard and
  `x.arr[i]` in the body get DIFFERENT syms → the length relation is lost → the field
  access stays `checked: true`. (Param/local arrays with a HOISTED length already elide:
  `n = d.len(); while i < n { d[i] }` → `checked: false`; only the field case is open.)
  The sound fix: value-number `GetField(base, field)` as a shared `Field(base, field)`
  sym, but ONLY for stable fields (never `PutField`'d, no calls) off an unreassigned
  parameter — AND the base must be the canonical (post-collapse) sym, which forces a
  shared field-name interner + `field_vn`/`unreassigned` to be threaded through all four
  transfer points (`transfer_block`, `step_env`, `find_cmp`, the elision applier), since
  the loop-header Phi collapse happens after the GVN fixpoint. **Attempted and reverted:**
  the threading through a soundness-critical GVN (a wrong sym-merge = elided check = OOB)
  is not warranted while NO benchmark indexes a field array in a hot loop (Vire code
  passes arrays + length as separate params; the object-graph work uses scalar/ref
  fields). Revisit when a field-array-heavy workload appears.
- [x] **graph WON — was 1.61× Rust, now 1.12× (compute at parity) (2026-07).** The
  cause was neither RC, nor object layout, nor bounds checks (all ruled out causally:
  `FASTLLVM_NO_BOUNDS` closed <7%; Vire emits 2 throw sites vs Rust's 32). Isolating
  steady-state compute (8 warm reps over pre-allocated arrays) showed Vire's compute is
  **0.93× Rust — faster**; the whole 1.61× was one-time paging. `jrt_region_array`
  default-zeroed each array with a full `memset`, faulting the entire 56 MB working set
  (incl. the untouched tail of the `array(m+16)` worst-case scratch arrays), while Rust's
  `vec![0;n]` gets lazy zero pages. **Fix (runtime, codegen-identical):** memset only the
  reused prefix below a dirty high-water mark (`r_dirty`); the fresh `mmap(MAP_ANONYMOUS)`
  tail stays zero + unfaulted. RSS 56→30 MB (= Rust), cold 55.8→44.4 ms (Rust 39.7). The
  residual 1.12× is the bounds checks. Gated: Java 67/67, fuzzer, +2 `vire_heap` zero-init
  tests. General win — helps every region-array-heavy program, not just graph.
- [x] **graph — at parity, no residual (2026-07, best-of-10 re-measure).** The "1.12×
  bounds-check residual" was a small-sample artefact. Bounds checks ON: 38.8 ms vs Rust
  39.9 (and vs Vire no-bounds 39.8) — parity, marginally ahead. The lazy-region fix closed
  graph fully; nothing left to do. (PGO infra remains available but is not needed here.)

## Tier 3 — enablers with broad latent effect

- [x] **`+%` / integer vectorization — ALREADY DONE (stale premise, verified 2026-07-24).**
  The item assumed checked-overflow blocks vectorization. Vire has **no** checked
  overflow: integer `add`/`sub`/`mul` already emit plain LLVM (no nsw/nuw) and wrap
  (see the runtime.c note "addition etc. wrap"). The `+%`/`-%`/`*%` operators already
  exist end-to-end (lexer `PlusPct`/`MinusPct`/`StarPct` → parser `AddWrap`/… → lower
  `IB::Add`/… — identical to `+`); `x +% 1` on i64::MAX wraps to MIN. And integer
  reductions **already auto-vectorize** (a contiguous i64 sum loop emits 35 `vpaddq`).
  Nothing to build.
- [x] **SIMD argmin — BUILT, MEASURED, REVERTED (2026-07-24): it does not help at any
  size.** The item assumed vectorizing the branchy argmin would close kmeans' 1.28× C++
  gap. Built an `argmin(arr)` builtin → `jrt_argmin` with a hand-written 4-wide AVX2
  compare/blend path (i64, correct first-min tie-break, `target("avx2")` + runtime
  `__builtin_cpu_supports` gate, scalar fallback). Isolated microbenchmark (1.25M calls,
  stable): AVX2 vs scalar is **0.67× at n=8, 0.92× n=32, 0.99× n=128, 1.00–1.01× at
  n≥512** — never a win. Small n: the horizontal-reduce/setup overhead exceeds the tiny
  scalar loop; large n: argmin is memory-bandwidth-bound, so scalar already saturates.
  This is exactly why no compiler auto-vectorizes it. And the kmeans gap is not argmin
  anyway: `dist[c]` in the argmin is already bounds-elided, and `FASTLLVM_NO_BOUNDS`
  closes only 0.8 ms of the 33→26 ms gap — it is deep codegen (same bucket as regex/
  pquicksort), not vectorization. Reverted rather than ship a slower-or-equal SIMD path.

## Perf — residual / parked (low ROI, keep for context)

- [ ] **Codegen scheduling / register allocation** on the branchy/irregular residuals.
  Re-measured best-of-N vs Rust 2026-07-24: regex **1.27×**, pquicksort **1.22×**,
  pipeline **1.12×**, compression **1.12×**. (STALE values corrected: raytracer is
  **0.99× = parity**, not 1.9×; sort is **0.99×**, not 1.05×; fft 0.99×.) Verified *not*
  IR quality (same program through `opt -O2` matches clang) — it's the LLVM **backend**
  reacting to subtle IR structure. Deep-codegen tuning, not a single fixable pass.
  **Parked — low ROI vs the wins already banked.**
- [ ] **Expand the differential fuzzer** (`tests/fuzz_gen.py`) — floats
  (fp-contract-matched), nested control-flow, break/continue, strings. (Correctness
  insurance, not perf, but belongs with the perf work.)
- [x] **Compile-time super-linearity FIXED — now O(n) (2026-07-24).** The ~O(n^1.4) was
  not inherent: profiled it (added a `FASTLLVM_TIME_PASSES` per-pass profiler) to
  `narrow_fields`, which was ~50% of the solver+backend cost and super-linear (67→408 ms
  for 4× functions). Cause: one whole-program range fixpoint over a GLOBAL local-range
  map that outgrew L2 → cache-miss-bound → super-linear time on linear work. Fixed by
  running the LOCAL analysis per function (self-contained: GetField/Call yield ⊤) on a
  small cache-resident map, collecting field ranges once per function (behaviour-
  identical). Result: whole solver+backend is now **linear** — 129/260/530 ms at
  1000/2000/4000 functions (2.0× per doubling; was 155/349/787 ≈ 2.25×). Extrapolated to
  100k LOC: ~1.9 s (was ~5–7 s). All remaining passes (elide_bounds, backend::emit) now
  scale linearly too. Analysis caching / true incrementality is a further lever but no
  longer needed to keep compile linear.
- [x] **Runtime GC latency — incremental cycle collector DONE** (`jrt_collect_step`):
  bounded incremental stepping (continuous, buffer-bounded RAM, no big-pass spike).
  Two soundness bugs found + fixed (MarkRoots must free only BLACK rc==0, not GRAY
  trial-deleted; a whole-buffer pass frees dead head-of-buffer nodes the compaction
  would otherwise drop unfreed), **verified against the `listdrop` leak-catcher** +
  a cross-batch garbage-cycle stress + flat RSS — see [DONE.md](DONE.md). The
  giant-connected-component **free phase** is now spread too (deferred garbage queue;
  a 2M-node ring drops 0-live, RSS flat to 16M). *Residual (research-level):* the
  mark/scan/collect *traversals* of a giant not-yet-proven-garbage component are
  still one atomic pass (~ms for millions of nodes) — fully bounding them needs a
  resumable traversal + a concurrent **write barrier** (Bacon–Rajan concurrent
  variant); high-risk, rare in practice. Also open: chunk-recycle bound tuning,
  larger arena chunks.
- [x] **Free-cascade — budgeted/deferred, DONE** (`drain_drops`): the release drop
  loop now frees at most `FREE_BUDGET` per top-level release (the rest deferred in the
  LIFO drop queue, drained `FREE_PUMP` per allocation + fully at shutdown), so
  dropping a large dead subgraph spreads across operations instead of one burst.
  Sound (queued objects are rc==0, unreachable); verified 0-live incl. a 1M-node list
  drop (deferral engaged) + the `listdrop` leak-catcher — see [DONE.md](DONE.md).

---

## Compile-time programming layer (macros + comptime + reflection, one typed AST)

**Framing:** a **compile-time programming layer**, not text substitution. Macros,
`comptime`, and reflection all operate on the same typed AST / type graph, run
*after* parse+inference, re-checked after expansion.

- [~] **Phase 2 — move passes after inference.** comptime folding now lives in a
  post-inference pass ([comptime.rs](crates/vire/src/comptime.rs) `eval_comptime`):
  collects module `const`s, inlines refs to literals (respecting shadowing), folds
  `comptime`/`comptime if`. `const` now works (value/comptime/array size).
  `tests/vire_comptime.sh`. **Still open:** move **macro expansion** after inference
  too (still runs before — the untyped anti-pattern), and have the pass consult the
  type graph (type-aware `comptime if`).
- [ ] **Phase 3+ — features on the foundation** (sequence below).
- [~] **(b) typed reflection over the type graph** — `@derive(Eq, Show, Ord, Hash, Json)`
  works for product AND sum types ([derive.rs](crates/vire/src/derive.rs)).
  `tests/vire_derive.sh`. **Open:** generic types (needs generic-method
  monomorphization in lower.rs), nested-user-type fields (recursive derive), JSON
  string escaping, and `@typeinfo(T)` as a comptime-iterable typed value (needs
  aggregate comptime values — the interpreter is scalar-only today).
- [~] **(c) hygienic item macros** — `macro name(P: type, n: ident, e: expr){ <items> }`
  → declarations ([itemmacro.rs](crates/vire/src/itemmacro.rs)); AST-level,
  kind-checked, hygienic, type-checked after expansion; nested invocations expand to
  a fixpoint; generic type args work. `tests/vire_itemmacro.sh`. **Open:** token
  **pasting** (identifier interpolation), multi-argument generics (`Map[K, V]`),
  `block`/`pat` parameter kinds.
- [ ] **`comptime for`** (loop unrolling to runtime statements) / **`emit`** surface
  syntax. Also open: comptime over reference/aggregate values (scalars only today),
  `return`/`break` in a comptime body.

## Front-end completeness

- [x] **`vire fmt`** (roundtrip AST→source) as parser-fuzz insurance — **DONE (2026-07-24).**
  `vire fmt FILE.vr` prints canonical source (`-i` rewrites in place). Two invariants, both
  tested: idempotency (`fmt(fmt(x))==fmt(x)`) and round-trip run equality (running the formatted
  file prints the same). Strings are re-escaped incl. `{`→`{{` (interpolation is parse-time).
  Verified idempotent over all 36 compiling `examples/vire/*.vr`. Module `fmt.rs`; test
  `tests/vire_fmt.sh`.
- [~] **Error messages** — panic-mode recovery collects multiple diagnostics; still
  open: fix suggestions and pointing near the true cause.
- [~] **Trait resolution + coherence.** Duplicate/overlapping method defs per type
  rejected; bounded generics `[T: Trait]` resolve + enforced. Open: overlapping
  **generic** impls, coherence across impls.
- [~] **Monomorphization** — works via the inliner/`instantiate`; full value-generic
  monomorphization (distinct instances per N) partly open (inference of a type arg
  that appears only in return position defaults to `Int`).
- [~] **Iterator-mutation check** ([REFERENCE.md](language/REFERENCE.md) §9a) — local
  non-mutation analysis; not provable → compile error.
- [x] **Merge points keep the object class — SOUNDLY.** `mut n = if c { a } else { b }`
  and `mut n = match … { … }` now propagate the branches'/arms' object class (and array
  element kind) onto the result local **only when every branch agrees**; a disagreeing
  or unknown branch leaves it unknown, so `n.field` on a heterogeneous merge is a loud
  compile error, never a wrong-offset load ([lower.rs](crates/vire/src/lower.rs)
  `lower_if`/`lower_match`). This unblocked self-recursive object builders: recursion-
  inlining ([inline.rs](crates/vire/src/inline.rs)) rewrites the tail self-call into an
  `if`/`else`, so a Vire binary tree that bound the recursive result and read a field of
  it failed "type of the object unknown" before. The `while`-loop back-edge idiom
  (`mut cur = head; while … { cur = cur.next }`) already worked. Regressions:
  `tests/vire_heap.sh` `if_expr_object_class` / `match_object_arms` /
  `heterogeneous_if_no_field` (must-reject, soundness) / `local_annotation_escape`;
  example: `examples/vire/object_graph.vr`.
  *Known remaining instance:* the `?` operator extracts the Ok/Some payload as a scalar,
  so `mut n = find()?; n.field` still errors (loud, sound) — sits on the partial
  Option/Result surface (Stdlib section) and is deferred with it.
- [x] **Local type annotation `mut x: T = …`** (also `x: T = …`) — the inference escape
  hatch. Supplies the object class the RHS may not carry (e.g. an `if` with a `null`
  branch), so the monomorphic unifier having a blind spot no longer forces a program
  rewrite. Parsed in [parser.rs](crates/vire/src/parser.rs); the class seeds the local in
  `lower.rs`. Test: `tests/vire_heap.sh local_annotation_escape`.

## Stdlib + FFI

- [~] **Collections breadth.** `list()`/`map()`/`set()`, `Str` methods, and iterator
  adapters (`fold`/`sum`/`count`/`map`/`filter`/`each`, statement-bodied lambdas)
  work. `tests/vire_iter.sh`. **Open:** `Str.split` (needs a typed `list[Str]`), and
  the full `Option`/`Result` surface (`.wrap(msg)` context/chain — core `?`/`match`
  works).

---

## Features 1–8 (open parts only)

### [1] Multithreading, safe by construction
- [x] **`parallel_map(n, shared, worker)` — DONE (2026-07-24).** Returns `Array[Int]` of
  `out[i] = worker(i, shared)`; workers write disjoint indices (no data race), `shared` must be
  a Sync type (same Send check as parallel_for). Runtime `jrt_parallel_map` (threads + sequential),
  glue in spawn.rs desugars to `{ let r = array(n); shim(n, shared, r); r }`. Thread-safe against
  the slab allocator (workers allocate too). Test: vire_threads.sh `parallel_map` (x30).
- [ ] `Mutex.lock(closure)` (scoped-guard form); typed `Channel[T]`
  for ref payloads (currently Int values).
- [ ] (M0.1c) measure real multithread atomic contention.

### [2] Template programming
- [ ] Fixed arrays `[T; N]` as a distinct inline-storage value type (value-generic
  `array(N)` already gives constant-size stack arrays).
- [ ] Overlapping/coherence checking for generic impls; inference of a type arg that
  appears only in return position (defaults to `Int` today).

### [3] Compile-time reflection
- [ ] `@typeinfo(T)` (fields/variants/methods/attributes, comptime-iterable).
- [ ] `@derive` via reflection (generic + nested-user-type — see (b) above).
- [ ] `comptime for`, `emit`. **No** runtime reflection (AOT).

### [4] Own optional preprocessor *(= comptime/@if/macros)* — DONE
- [x] **Hygienic macros: typed parameters `block`/`pat`, token pasting, diagnostic
  spans into the expansion — DONE (2026-07-24).** Item-macro kind system extended
  with `block` (a `{ … }` argument, kind-checked, spliced as e.g. a generated fn
  body) and `pat` (a pattern argument — `_`/literal/binding/`Ctor(...)` — spliced
  into `match`/`for` positions via `expr_to_pattern` + a `patmap` threaded through
  substitution). **Token pasting** `Base ## _suffix` (`##` = `Tok::HashHash`) builds
  distinct generated names per invocation (solves the name-collision limitation) and
  works as a call target; resolved AST-level (fragments = ident params, else literal),
  deferred via a `\u{1}` sentinel only inside macro bodies so nothing escapes to
  lowering. **Diagnostic spans**: `expand_item_macros` now returns `Vec<Diag>` —
  kind errors point at the offending argument, arity/unknown at the invocation site,
  duplicates at a representative def; substituted idents carry the call-site span.
  Also fixed a real **OOM**: a malformed macro body spun the parse loop (unbounded
  diagnostics growth); the body-item loop now always advances. Tests in
  `tests/vire_itemmacro.sh` (16, +5); typed `expr`/`ident`/`type` + hygiene were
  already done.

### [6] Logger — remaining
- [x] **`with log.span(field, value, …) { … }` — DONE (2026-07-24).** Scoped context:
  every `log.*` inside the block prepends `[field=value …]`. Nests and unwinds. Compile-time
  only — desugars to `__log_span_push`/`__log_span_pop` markers the lowering consumes into a
  field stack (zero runtime cost, like disabled levels). Test: vire_log.sh `with log.span`.
- [x] **Colored console sink — DONE (2026-07-24).** `--log-color` (`FASTLLVM_LOG_COLOR`) wraps
  the `[LEVEL]` tag in an ANSI colour by severity (error=red/warn=yellow/info=green/debug=dim);
  opt-in, plain otherwise. Test: vire_log.sh `--log-color`.
- [ ] JSON + file sinks (need message restructuring / a runtime file handle) — follow-up.

### [7] Go-style error handling — remaining
- [x] **`.wrap(msg)` + `.error()` — DONE (2026-07-24).** `result.wrap(msg)` on an `Err` prepends
  "msg: " to the (string) error, keeping the chain (`.wrap("outer").wrap("outermost")` →
  "outermost: outer: file not found"); on `Ok` it passes through, so `.wrap(m)?` works. `.error()`
  returns the message as a `Str` (Go's `err.Error()`; `""` when Ok) — the way a wrapped chain is
  observed. `?`/`match` already worked. Runtime `jrt_err_wrap` (immortal join, no RC on the erased
  slot) + `jrt_as_str`; lowering in lower.rs (branch/merge on the tag). Also fixed a real backend
  crash: storing a pointer payload (a string literal) into the i64-ERASED Result/Option slot now
  emits `ptrtoint`, not an invalid `store i64 @jstr.0`. Test `tests/vire_errors.sh`; gate GREEN.
- [ ] **Two-parameter typed Result payloads** (the honest remaining gap). Single-param generics
  are monomorphized type-correctly (`Option[Str]` round-trips a string via `Option$Str`), but
  `Result[T,E]` (two params) stays i64-erased, so `match Err(e) -> print(e)` yields the pointer as
  a number — use `.error()` for the message meanwhile. Extend the generic sum-type instantiation
  (`generic_stypes`, `class_of_ann`) to two type args (`Result$T$E`) + infer the unconstrained arg.
  Soundness-sensitive (payload layout) — do NOT rush; gate on the heap oracle. Then `Err(e)` binds
  a typed `e`, typed sum-type errors + attached debug path follow.
  - **Attempted 2026-07-24, reverted (finding).** Registering Result in `generic_stypes` [T,E]
    monomorphizes it and `match Err(e)` binds typed — BUT the erased `Result` (i64 payloads, used
    by `?`/`.wrap`/`.error`) and `Result$T$E` then coexist and are mixed at construct-vs-match
    sites. They agree ONLY when every payload is i64 (`Result[Int,Int]` works); with a Ref payload
    (`Result[Str,_]`) the layouts diverge → a Ref field read as i64 (or vice versa) → memory-unsafe
    under RC, and it regressed `vire_errors.sh`. The real fix must unify the erased/mono
    representation and teach `?`/`.wrap`/`.error` the mono class before gating on the heap oracle.

### [8] Debug symbols + crash paths — remaining
- [ ] freestanding: compact symbol table instead of libc `backtrace`; map the entry
  symbol `java_main` back to `main` in the DISubprogram name (cosmetic).

---

## GPU `@gpu` — reach and beat cuda-oxide

*(Near-term GPU perf items live in the Performance Push, Tier 4 above. This is the
full roadmap to match cuda-oxide's feature scope + performance and then exceed it.)*

**Framing (see [benchmarks/gpu/VS-CUDA-OXIDE.md](benchmarks/gpu/VS-CUDA-OXIDE.md)):**
both lower to PTX through the *same* LLVM NVPTX backend, so raw codegen is already
at parity for simple kernels (after the `opt -O3` mid-end that just landed). The
gap is four things: **(1)** device-programming *primitives* Vire can't express,
**(2)** the *high-perf kernel classes* (tensor cores, TMA) that need those
primitives, **(3)** perf *infrastructure* (async/streams/persistent buffers/
autotuning), and **(4)** Vire-only *beat levers* (memory safety, whole-program
specialization, single-source oracle). Honest scope: full tensor-core/TMA parity
is NVIDIA-research-grade (multi-quarter), so sequence primitives first.

### Stage G1 — device-programming primitives (reach parity on the common 80%)
- [x] **Block barrier** (`gpu_sync()` → `@llvm.nvvm.barrier0`) — DONE.
- [x] **Warp intrinsics** — DONE: `gpu_shfl_down` (`shfl.sync.down.i32`) and
  `gpu_warp_reduce_add` (5× shuffle+add full-warp sum). Enables the fast-reduction
  idiom (warp-reduce → atomic) with no shared memory. *Vote/ballot/scan still open.*
- [x] **Device atomics** — DONE: `gpu_atomic_add(arr, idx, v)` → `atomicrmw add`
  (global, Int/Long), returns the old value. Read-only analysis made sound (an array
  passed to any device call counts as written). *CAS/other ops still open.*
- [x] **IEEE device math** — DONE: `gpu_sqrt/fabs/floor/ceil/fmin/fmax` via
  `@llvm.*.f64` (round-to-nearest → bit-exact vs CPU). *Transcendentals below.*
- [ ] **Transcendental math** (sin/cos/exp/log/tan/pow) — needs libdevice
  (`__nv_*`) bitcode linked into the device module (not plain LLVM intrinsics).
- [ ] **Shared memory** (`@shared` arrays, `Workgroup`/`addrspace(3)`) — new syntax
  + IR; unlocks block-level (not just warp-level) reductions and tiling.
- [ ] **Vote/ballot + warp scan**; atomic **CAS**/min/max/exchange.
- [ ] **Tunable launch config**: explicit block size, 2-D/3-D grids, dynamic
  shared-memory size (replaces the fixed `block=256, grid=ceil(N/256)`).
- [ ] **Device `printf`** (debugging).
- [ ] **Device-side helper fns** with inlining (ensure non-kernel device fns emit;
  `opt` inlines them).

### Stage G2 — perf infrastructure (close the transfer/launch overhead)
- [ ] **Write-only H2D elision** — skip the *upload* for output-only buffers
  (complements the read-only D2H skip just shipped).
- [ ] **Persistent device buffers** across launches (no malloc/free per call).
- [ ] **Async launches + CUDA streams**; overlap H2D / compute / D2H.
- [ ] **Pinned (page-locked) host memory** for faster transfers.
- [ ] **Per-arch codegen** (`-mcpu=sm_90`/`sm_100`) + **cubin caching**, not only
  forward-JIT PTX (saves the ~0.2 s JIT on every run).
- [ ] **Occupancy-aware launch autotuning** (`cuOccupancyMaxPotentialBlockSize`).

### Stage G3 — high-performance kernel classes (where cuda-oxide gets 10×+)
- [ ] **`cp.async` / TMA** async global→shared copies (Hopper/Blackwell).
- [ ] **Tensor-core MMA**: `mma.sync` / `wgmma` / `tcgen05` intrinsics.
- [ ] **Cooperative groups / thread-block clusters**.
- [ ] **Tiled-GEMM building block** in-language (comptime-generated) as the
  reference win. *Scope: NVIDIA-research-grade; do G1/G2 first.*

### Stage G4 — the BEAT levers (Vire-only — exceed, don't just match)
- [ ] **Memory-safe device mode.** cuda-oxide device access is unchecked
  (CUDA-like). Vire's solver can prove many device indices in-range (reuse
  `bounds.rs` relational elision) and bounds-check the rest → an *optional safe GPU
  mode* (off by default for parity, on for safety). No CUDA/C++/cuda-oxide analogue.
- [ ] **Whole-program kernel specialization.** const-prop launch bounds +
  monomorphize kernels per call-site (value generics exist) → constant loop trips,
  `__launch_bounds__`, device dead-arg elimination. A single-source whole-program
  compiler can specialize kernels a library-based flow cannot.
- [ ] **Single-source CPU+GPU + bit-exact oracle (already unique).** Extend:
  automatic CPU fallback when no GPU present; **differential CPU-vs-GPU fuzzing** of
  kernels (reuse `fuzz_gen.py`); float kernels with an fp-contract-matched oracle.
- [ ] **comptime kernel generation.** Generate specialized kernels (tile sizes,
  unroll factors) at compile time from the comptime layer — autotuning with no
  runtime JIT.

### Fair measurement (fill the Rust-GPU column)
- [ ] Build the cuda-oxide toolchain (pinned nightly) once; run **identical**
  kernels; compare **kernel-compute time only** (warm context, exclude H2D/D2H).
  Start with saxpy + a shared-mem reduction + a tiled GEMM. Per
  [benchmarks/gpu/VS-CUDA-OXIDE.md](benchmarks/gpu/VS-CUDA-OXIDE.md).

---

## `@vulkan` — safe, easy, full-performance Vulkan (graphics + compute)

### Open backend work — autonomous implementation pass (tracking)

Everything the shipped `@vulkan` does not yet cover, ordered roughly by tractability.
Checked off as implemented + tested (`tests/vire_vulkan.sh`); items that turn out
unsound or genuinely multi-week are marked *skipped* with the reason.

- [x] **Multi-component swizzles** — `.xy`/`.xyz`/`.rgb` reads (today single-component only).
- [x] **`if` as a statement** — effect-only branches (today `if` is value-only).
- [x] **`@gpuvk` — vendor-neutral Vulkan compute.** A general data-parallel compute stage
  over a Vire array (SPIR-V compute + `vkCmdDispatch`), distinct from CUDA/ROCm `@gpu`.
  Runs on Intel/NVIDIA/AMD. `@gpu` stays CUDA (NVPTX); `@gpuvk` is the Vulkan option.
- [x] **Uniform / params** — a wider push constant (several floats) readable in
  `@fragment`/`@vertex` (today only the task cull plane).
- [~] **`Mat` in shaders + transform** — a small matrix type + `mat*vec`, so a `@vertex`
  can apply a transform from a uniform.
- [x] **Depth buffer** — a depth attachment so overlapping meshlets occlude correctly.
- [x] **Wider scene records** — a colour/normal field beyond `offset+cone`; normals for
  lit meshlets.
- [ ] **`vkCmdDrawMeshTasksIndirectCountEXT`** — a GPU count buffer (today a fixed indirect count).
- [x] **Textures / samplers** — image + sampler + descriptor + fragment sampling. *Large;
  attempt, else skip with reason.*
- [~] **Typed resource handles + lifetime safety (RC-bound texture+buffer+session handles DONE; persistent ctx)** — `Buffer`/`Texture`/`Pipeline` as
  RC/region-tracked Vire values (no GPU use-after-free). *Architectural; attempt a
  minimal handle, else document.*
- [~] **Render graph → auto barriers (layout transitions + N-pass chain + multi-input DAG DONE) / declarative `frame { bg }` first step DONE** — *architectural,
  multi-week; document honestly, do not fake.*
- [~] **Windowed arbitrary geometry + per-frame draw (animated window + Vire-driven session + windowed Vire geometry DONE)** — *needs a persistent
  context; attempt, else document.*

**Investigated — high value, de-risked, multi-quarter.** Full design, safety
model, and target ergonomics in [language/GPU-VULKAN.md](language/GPU-VULKAN.md).
The vision: Vulkan **as easy as OpenGL** but with full performance, memory safety,
and Vire's whole-program optimizations — a *compiler-integrated safe Vulkan
framework* (not an FFI binding). What makes it Vire-only: **compile-time
pipeline/descriptor baking** (constants in the binary, no runtime reflection or
first-use hitches), a **static render graph → minimal correct barriers** (the
hardest hand-Vulkan part, done by the compiler), **language-level handle safety**
(RC/region lifetimes → no GPU-resource use-after-free), **zero-cost validation**
(layers under `--debug`, compiled out in release), and **single-source shaders**
(`@vertex`/`@fragment`/`@compute` → SPIR-V via the `@gpu` emitter). Escape hatch:
raw `Vk*` via verified `native "c"`. All deps present here (LLVM `spirv64`,
libvulkan, GLFW/SDL2, Wayland+X11, WSI on both Intel iGPU + RTX).

Staged (each stage runnable):
- [ ] **V1 — safe compute foundation.** `@compute` → SPIR-V → dispatch over a
  minimal safe Vulkan runtime; reuse the `jrt_gpu_*` ABI + read-only analysis. No
  windowing. Delivers vendor-neutral compute (runs on Intel + NVIDIA here). *Smallest
  real step; stands up the SPIR-V emitter + runtime.* (This subsumes the old
  "Vulkan compute backend" idea — it is the foundation, not a separate track.)
- [~] **V2 — hello triangle.** *Mostly DONE — visible in a window.* `vk_window(0)`
  opens a GLFW window + Vulkan swapchain and presents the triangle until closed
  (per-frame acquire/submit/present, FIFO); `vk_triangle()` keeps the headless
  pixel-verified CI path. One runtime shares `build_pipeline`/`build_rp`/`rec_draw`
  across both. Wayland extent-clamp handled. `examples/vire/vulkan_triangle.vr`,
  `crates/driver/src/vk_runtime.c`, `tests/vire_vulkan.sh`. Linked only when used
  (`want_vulkan` → libvulkan+glfw). *Remaining:* the declarative `frame { clear;
  draw }` surface + arbitrary geometry (today the triangle is fixed), and the
  single-source `@vertex`/`@fragment` → SPIR-V shaders (the SPIR-V-emitter item
  below; shaders are bootstrap glslc SPIR-V for now).
- [~] **V3 — resources.** Buffers/meshes, uniforms, textures/samplers exist as RC
  handles. *Descriptor-set-layout derivation DONE:* the shader compiler reflects each
  stage's resource usage (`uses_ssbo`/`uses_texture`/`uses_texture2`/`uses_push_constant`
  → `fastllvm_ir::VkIface`), unions it across stages, and main.rs emits it as C data
  (`VK_IFACE_*`) in `vk_shaders.c`; the runtime builds the `VkDescriptorSetLayout` from
  it via one generic `mk_dsl_reflected()` instead of a hardcoded per-demo layout. Wired
  for the textured (1 sampler), 2-sampler blend, and mesh/meshlet SSBO paths — the
  binding, descriptor type AND stage mask now come from the shader, verified
  pixel-identical (`tests/vire_vulkan.sh`, 36). *Push-constant range + pipeline layout
  DONE for the mesh/meshlet path:* one `mk_pipeline_layout_reflected()` builds the
  `VkPipelineLayout` from the reflected dsl + the reflected push range
  (`VK_IFACE_PUSH_SIZE/STAGES`), and the `@task cull_plane()` push command pushes to the
  reflected stage — so the range's size and stage mask come from *which stage reads the
  push*, not `MESH | maybe TASK`. The whole mesh pipeline layout (descriptors + push) is
  now shader-derived. *Remaining:* the graphics vertex/fragment pipeline keeps its fixed
  16-byte per-frame `uniform()` channel (a runtime protocol, always pushed — not a
  shader-varying quantity), the standalone compute-dispatch dsl (size-4 host count),
  texture arrays, and the `draw(pipe, mesh, uniforms)` host surface. *Generic draw
  surface DONE (graphics):* `vk_draw(verts, ux,uy,uz,uw)` — the program supplies the
  geometry AND a vec4 uniform, rendered through its own compiled @vertex/@fragment (the
  uniform reaches `uniform()`), instead of a fixed per-demo entry point
  (`examples/vire/vulkan_draw.vr`, `tests/vire_vulkan.sh vire_draw_generic`, 37).
  *Generic draw WITH a reflected resource DONE:* `vk_draw_tex(verts, handle, ux,uy,uz,uw)`
  binds an RC texture handle to the sampler binding the @fragment's `tex()` reflects into,
  with program geometry + uniform — so one generic draw covers the textured case, pipeline
  + descriptor layout from the shader, resource + geometry + parameters from the program
  (`tests/vire_vulkan.sh vire_draw_tex`, 38; the fixed `vk_draw_handle` now shares the same
  `draw_res_geo` path). *Multiple reflected bindings DONE:* `draw_res_geo` takes a handle
  array and binds each to `VK_IFACE_BINDING[i]`, so `vk_draw_tex2(verts, h0, h1, ux..uw)`
  binds two textures to the two reflected sampler bindings of a `tex()`+`tex2()` blend
  @fragment (`tests/vire_vulkan.sh vire_draw_tex2`, 39). *Storage buffer DONE:* a fragment
  `buf(i)` builtin reads a read-only float storage buffer (a new reflected binding), and
  `draw_res_geo` has a per-binding KIND switch — a `GpuTex` writes a sampler descriptor, a
  `GpuBuf` writes a storage-buffer descriptor — so `vk_draw_buf(verts, handle, ux..uw)`
  feeds a data-driven fragment from a Vire GPU buffer (`tests/vire_vulkan.sh vire_draw_buf`,
  40). *Mixed heterogeneous bindings DONE:* `buf()` moved to binding 2 (clear of the
  samplers at 0/1), so `vk_draw_tex_buf(verts, tex, buf, ux..uw)` binds a texture (sampler,
  binding 0) AND a storage buffer (binding 2) in ONE draw — the per-binding kind switch
  writes each descriptor type, verified centroid (229,102) = (tex.r, buf[0]), 0-live
  (`tests/vire_vulkan.sh vire_draw_tex_buf`, 41). The generic graphics surface now binds
  textures AND buffers, one/many/mixed, all from the shader interface (vk_draw / _tex /
  _tex2 / _buf / _tex_buf). *Remaining:* the mesh/meshlet pipeline through a generic
  surface — needs device unification (persistent context + mesh-shader ext) to accept a
  GpuBuf scene handle; the mesh layout is already reflected. And `draw(...)` naming sugar.
- [ ] **V4 — render graph.** Automatic image-layout transitions + minimal barriers;
  depth, multi-pass, MSAA, swapchain-resize.
- [~] **VS — Vire shaders (SPIR-V emitter).** *DECIDED: Vire is the shader language.*
  *Steps 1+2 SHIPPED:* Vire **owns SPIR-V generation** (`crates/backend/src/spirv.rs`
  emits assembly → `spirv-as` → generated `vk_shaders.c`, no glslc), and a real
  **shader compiler** (`crates/vire/src/shader.rs`) compiles an `@fragment fn`
  **body** to SPIR-V ops — float/vector arithmetic (`OpFAdd/Sub/Mul/Div`), `mut`
  bindings, `vecN(...)` constructors, and vector·scalar (`OpVectorTimesScalar`) —
  not just a constant. `@vertex`/`@fragment` parse as item attributes and are pulled
  out of host lowering + inference. Verified (`tests/vire_vulkan.sh`): a computed
  green fragment (`vec4(0.1,0.4,0.15,0.5) * 2.0`) renders green, headless + windowed.
  *Fragment inputs — `gl_FragCoord` DONE:* `frag_x()`/`frag_y()`/`frag_coord()`
  read the pixel position (`OpLoad` + `OpCompositeExtract`, BuiltIn FragCoord added
  to the interface only when used), so a fragment computes **per-pixel** — a
  gradient `vec4(frag_x()/256.0, …)` gives centroid r≈128 from position, verified
  (`tests/vire_vulkan.sh vire_fragment_fragcoord`; `examples/vire/vulkan_triangle.vr`
  shows a visible gradient). *`@vertex` DONE:* a Vire `@vertex fn vs(pos: Vec2) ->
  Vec4` receives the built-in triangle corner (indexed from a fixed array by
  `gl_VertexIndex`) and returns `gl_Position` — so it **transforms** the geometry
  (swizzles `.x/.y` + mixed `vecN` construction added). Verified: a shift `vec4(pos.x
  + 3.0, …)` moves the triangle off-screen (`tests/vire_vulkan.sh vire_vertex_shader`;
  both stages Vire-authored). *Varyings DONE:* the `@vertex` stage writes a per-vertex
  value with `out_color(vec3)` and the `@fragment` reads the **interpolated** result
  with `in_color()` — the vertex→fragment Location-0 link is derived by the compiler
  (Output/Input decorated + added to each entry-point interface only when used). A
  Gouraud triangle (corner colors from position) gives centroid r≈128, g≈152 with
  g≠r, proving interpolation a flat fragment cannot produce (`tests/vire_vulkan.sh
  vire_varying_color`; `examples/vire/vulkan_varying.vr`). *Vertex buffers DONE:* the
  pipeline reads positions from a vertex buffer (attribute Location 0), and
  `vk_mesh(verts)` renders Vire-authored geometry — a flat `[Float]` of interleaved
  (x,y) uploaded as f32, drawn as a triangle list. The `@vertex` reads each position
  from the buffer (both the default and Vire `@vertex` shaders unified on the
  attribute; the old `gl_VertexIndex` built-in array is gone). Verified: the default
  corners as Vire data render identically to `vk_triangle`, and an off-screen mesh
  makes the centroid the clear color (`tests/vire_vulkan.sh vire_mesh_buffer`;
  `examples/vire/vulkan_mesh.vr` draws a quad with a per-vertex varying). *Per-vertex
  attributes DONE:* `vk_mesh_c(verts)` interleaves (x,y, r,g,b) per vertex; the
  `@vertex` reads its own color from the buffer via `attr_color()` (attribute Location
  1, added to the vertex-input state + shader interface only when used) and forwards
  it as a varying — the classic RGB-corner triangle, where the centroid samples all
  three channels blended (`tests/vire_vulkan.sh vire_mesh_attr_color`;
  `examples/vire/vulkan_rgb.vr`). Geometry AND per-vertex data now both flow from
  Vire — this is the typed stage I/O VM builds on. *Structured control flow DONE:*
  shader locals are now `Function`-storage variables (OpVariable + load/store, so
  mutation carries across control-flow edges), and the emitter supports `if`/`else`
  as a value (`OpSelectionMerge`), `while` loops (`OpLoopMerge`), comparisons
  (`OpFOrdLessThan`…→ bool), `&&`/`||`, and `+=`/assignment. Verified: a per-pixel
  `if frag_x() < 100` picks the color (centroid → blue), and a `while` accumulates
  0.1×5 into red (→128) (`tests/vire_vulkan.sh vire_shader_branch`/`vire_shader_loop`;
  `examples/vire/vulkan_control.vr`). *Remaining:* (a) real `Vec2/3/4`/`Mat4` in the
  host type system (today vectors are shader-local); (b) `GLSL.std.450` builtins
  (normalize/dot/mix/sqrt…) + `if`-as-statement with effect-only branches; (c) index
  buffers + more attribute types (normal/uv).
- [~] **VM — GPU-driven meshlets (first-class).** *Foundation SHIPPED — the mesh
  pipeline runs end-to-end.* `vk_mesh_shader()` renders through a **mesh** pipeline
  (`VK_EXT_mesh_shader`): no vertex buffer and no vertex stage — a `MeshEXT` shader
  emits the triangle's vertices + primitive itself (`OpSetMeshOutputsEXT`,
  `gl_MeshVerticesEXT`, `gl_PrimitiveTriangleIndicesEXT`), dispatched with
  `vkCmdDrawMeshTasksEXT` (one task workgroup = one meshlet). The runtime selects a
  mesh-capable device, enables the `meshShader` feature, and loads the draw entry via
  `vkGetDeviceProcAddr`; returns -2 where unsupported so callers skip. SPIR-V is a
  bootstrap `@mesh` (crates/backend/src/spirv.rs, assembled at spv1.4) + the Vire
  `@fragment` for color. Verified: centroid = the Vire fragment color
  (`tests/vire_vulkan.sh vire_mesh_shader`; `examples/vire/vulkan_meshlet.vr`).
  *Vire-authored `@mesh` + `@task` DONE:* all three GPU-driven stages now compile from
  Vire. `@mesh` (`set_mesh_outputs(nv,np)` / `mesh_pos(i, vec4)` / `mesh_tri(i,a,b,c)`)
  emits a meshlet's vertices + primitives with the positions computed in full Vire
  (arithmetic/`vecN`/GLSL builtins); `@task` (`emit_mesh_tasks(n)`) is the
  amplification stage that dispatches meshlet workgroups. Verified: a Vire-authored
  triangle takes the fragment color, and `emit_mesh_tasks(0)` **culls** it (centroid →
  clear) — GPU-gated geometry (`tests/vire_vulkan.sh vire_mesh_authored`/
  `vire_task_cull`; `examples/vire/vulkan_meshlet_authored.vr`). *GPU frustum culling
  DONE:* the host passes a frustum plane to `vk_mesh_shader(nx,ny,nz,d)`, delivered to
  the `@task` shader as a **push constant** (`cull_plane()`); the task shader tests the
  meshlet's bounding-sphere center on the GPU (`dot` + compare → `emit_mesh_tasks(bool)`
  lowers to `OpSelect` 1/0). The same meshlet renders or is culled purely from the
  camera data (`tests/vire_vulkan.sh vire_task_gpu_cull`; `examples/vire/vulkan_cull.vr`).
  *Many meshlets from a Vire scene buffer DONE:* `vk_mesh_scene(offsets)` uploads a
  `[Float]` of per-meshlet (x,y) offsets to an **SSBO** and issues one
  `vkCmdDrawMeshTasksIndirectEXT` dispatching N mesh workgroups; each `@mesh` workgroup
  reads its own offset with `meshlet_offset()` (`scene[gl_WorkGroupID.x]`, a
  descriptor-set-bound storage buffer) and emits its triangle there. Verified: two
  meshlets (left+right) both render, and the scene array decides which exist
  (`tests/vire_vulkan.sh vire_mesh_scene`; `examples/vire/vulkan_scene.vr`). *Fused
  GPU-driven cull renderer DONE:* `vk_mesh_scene_cull(offsets, nx,ny,nz,d)` runs one
  `@task` workgroup per meshlet — each reads its center (`meshlet_offset()`), tests it
  against the pushed frustum plane (`cull_plane()`, `dot` + compare), and emits ONLY
  the survivors via `emit_visible(bool)`, which writes the meshlet index into a
  task→mesh **payload** (`TaskPayloadWorkgroupEXT`). The `@mesh` reads
  `scene[payload.idx]` (`culled_offset()`) and draws it. So invisible meshlets never
  reach the rasterizer — the decision is entirely on the GPU from the camera plane.
  Verified: a left+right scene shows both with a permissive plane, and the left
  meshlet is GPU-culled with a +x plane (`tests/vire_vulkan.sh vire_scene_cull`;
  `examples/vire/vulkan_scene_cull.vr`). *GPU-built scene DONE:* a Vire `@compute`
  builder (`set_meshlet(vec2)`, indexed by `meshlet_index()`) fills the scene SSBO on
  the GPU; `vk_mesh_built(count, nx,ny,nz,d)` dispatches it, barriers, then runs the
  `@task` cull + `@mesh` draw over the GPU-built buffer — the meshlet set never exists
  on the host. So build → cull → draw → shade are all Vire, in one program (four
  stages compiled to SPIR-V + a compute pipeline with a shader-write→read barrier).
  Verified: 2 GPU-built meshlets show both under a permissive plane and cull the left
  under a +x plane (`tests/vire_vulkan.sh vire_mesh_built`; `examples/vire/vulkan_built.vr`).
  *Typed scene records + cone/backface culling DONE:* the scene record is a Vire struct
  `Meshlet { offset: vec2, cone: vec2 }` (std430, one layout shared by every stage via
  `resource_decls`). The `@compute` builder writes both fields (`set_meshlet(offset,
  cone)`); the `@task` reads the facing direction (`meshlet_cone()`) and backface-culls
  (`emit_visible(cone.x > 0)`) — verified: of two GPU-built meshlets the one facing
  toward is drawn and the one facing away is culled (`tests/vire_vulkan.sh
  vire_cone_cull`; `examples/vire/vulkan_cone.vr`). *Per-vertex mesh attributes → fragment
  DONE:* the `@mesh` writes a per-vertex colour with `mesh_color(i, vec3)` (a Location-0
  output array sized to the vertex cap) and the `@fragment` reads it interpolated via
  `in_color()` — the RGB-corner triangle produced by the mesh shader itself, verified
  (`tests/vire_vulkan.sh vire_mesh_color`; `examples/vire/vulkan_mesh_color.vr`).
  *Remaining:* wider scene records (a colour/normal field beyond offset+cone); normal
  attributes for lit meshlets; real geometry input to the builder (today it places
  meshlets by formula). The GPU-driven renderer skeleton — build, cull, draw, shade,
  with per-vertex attributes — is now entirely Vire, which normally spans GLSL/HLSL +
  C++ + a mesh toolchain.
- [ ] **`@gpu`-on-Vulkan compute path** (separate from graphics): the SPIR-V dialect
  of the device emitter via `llc -march=spirv64` (StorageBuffer/`Workgroup`, subgroup
  ops, `GLSL.std.450`); G1 intrinsics map directly (barrier→`OpControlBarrier`,
  warp→subgroup, atomic→`OpAtomicIAdd`). Compute-flavor SPIR-V, so `llc` suffices
  here (unlike the graphics stages above).
- [ ] **V5 — Vire optimizations.** Compile-time pipeline/descriptor baking, shader
  monomorphization per material, whole-program resource-lifetime + dead-resource
  elimination, zero-cost validation gating.
- [~] **V6 — temporal upscaling (FSR2 / DLSS) — INPUTS + CONTEXT DONE, SDK + present loop
  OPEN (2026-07-24).** The compiler/runtime now auto-provide every temporal-upscaler input,
  each CPU-verified + validation-clean (`tests/vire_vulkan.sh`, 53/0):
  - *GPU query/select DONE:* `vk_gpu_count/list/select` — `ctx_init` honours a chosen device
    (scans for a graphics queue). `vk_gpu_select(1)` renders on the **RTX 5070** with no crash
    (the old GLSL.std.450 crash was invalid SPIR-V; our shaders are validation-clean now). This
    is the P0 prerequisite for DLSS/Streamline (NVIDIA-only).
  - *Auto motion vectors DONE (the crown jewel):* `compile_vertex_mv` evaluates the @vertex
    transform TWICE (current vs previous-frame uniform, `uniform_member` selects push-constant
    member 0/1) and writes `ndc_cur − ndc_prev` to an **R16G16F** motion target — the exact
    velocity FSR2/DLSS consume, derivable only because a single-source compiler sees the
    transform. `vk_motion` (verified: 0.1 x-translation → mx=1000 exact).
  - *Jitter DONE:* `vk_jitter(i)` = runtime Halton(2,3) service; the MV vertex adds it to
    gl_Position (colour only, push member 2). Motion is computed from UN-jittered clip →
    verified invariant under jitter.
  - *Sampleable depth / full bundle DONE:* one render (`render_bundle`) emits colour + R16G16F
    motion + **D32 depth** (SAMPLED+TRANSFER_SRC); `vk_depth` reads it (z=0.5→500000 exact).
  - *Upscaler context DONE:* `vk_render_res(n)`/`vk_display_res(m)` split + `vk_upscale`
    (render low-res, bilinear upscale to high-res, persistent temporal-history EMA). Reference
    bilinear+EMA on host (simple/verifiable); GPU render real.
  - **OPEN (the actual upscaler + real-time path):**
    - [ ] **Vendor the FFX FSR2 SDK** (`ffx_fsr2` + its shaders) and feed the existing bundle
      (colour+motion+depth+jitter, render→display res) into `ffxFsr2ContextDispatch` — replaces
      the reference bilinear+EMA with real motion-guided reconstruction. Vendor-agnostic, runs
      on either GPU; do this FIRST (no P0 needed).
    - [ ] **General present loop (P2)** — `vk_window` presents only the built-in triangle and
      has a swapchain-semaphore reuse hazard (found by the validation sweep). Generalise to
      present arbitrary Vire geometry at render-res → upscale → display-res, per-frame; fix the
      per-frame-semaphore ring (analogous to the fcache ring). Needed for a real-time budget
      (the only regime where upscaling pays).
    - [ ] **DLSS / NVIDIA Streamline backend** — same inputs, swap the backend; needs graphics
      on the 5070 (P0, done) + the NGX/Streamline Vulkan plumbing (proprietary blob; Linux
      native-Vulkan maturity to be VERIFIED, not assumed). After FSR2, since inputs are identical.

---

## Cross-cutting

- [~] **Compile time** whole-program+mono+comptime — measured super-linear; analysis
  caching / incremental is open (also in Perf Push residual/parked).
- [ ] **Overflow default**: checked also in release, wrapping only explicit
  ([REFERENCE.md](language/REFERENCE.md) §3.1). *(Enables Tier 3 `+%` vectorization.)*

## Cross-compilation (see [language/CROSS-COMPILE.md](language/CROSS-COMPILE.md))

Windows works (`--target x86_64-pc-windows-gnu` → running `.exe`). Follow-ups:
- [ ] **macOS cross-compile** — needs the macOS SDK. Wire up
  [osxcross](https://github.com/tpoechtrager/osxcross): detect `OSXCROSS_ROOT`/SDK,
  pass `--sysroot` + the right `-target`. Runtime code is already portable.
- [ ] **FreeBSD/BSD full build** — object emit works; add sysroot handling
  (`--sysroot <freebsd-root>`) so linking an executable succeeds here.
- [ ] **aarch64 targets** — verify `aarch64-pc-windows-gnu` (llvm-mingw) and
  `aarch64-unknown-linux-gnu` end to end (untested; codegen should already work).
- [ ] Windows **threads** produce a `.exe` (winpthreads) but execution under wine was
  flaky — verify on real Windows.

---

## Non-goals (deliberate)
Runtime `eval`/reflection · dynamic loading of unknown code · C-text preprocessor ·
deadlock-freedom guarantee · "all" C++/Rust libraries beyond the C-ABI boundary.
