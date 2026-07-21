# Vire — Roadmap (open work)

Only **open** and **partial** items. Completed work has been removed. Legend:
`[ ]` open · `[~]` partial. Design basis: [language/](language/).

## Current state (2026-07)

The whole pipeline is functional and green: lexer → parser → macro expansion →
recursive inline → type inference → lowering to SSA IR → whole-program solver → LLVM
backend → `clang -O2 -flto -march=native`. `vire build`/`vire run` produce native
binaries. Traits (vtable dispatch + devirtualization), arrays, structs/records,
generics-by-inlining, `match`/sum types, `Result`/`Option` + `?`, `comptime if`,
`list()`/`map()` collections, and `log.*` compile-time-filtered logging all work.
Soundness floor: the Java heap-balance oracle stays **65/65** and the Vire heap
suite (`tests/vire_heap.sh`, now **15/15** incl. the capsule deep-copy cases) plus
all `tests/vire_*.sh` stay green after every change.

**Shipped since (this session):**
- **`@gpu` kernels** → NVPTX/PTX → CUDA launch (single-source, up to 16× vs CPU;
  bit-exact for integer kernels). See [language/GPU-KERNELS.md](language/GPU-KERNELS.md).
- **`capsule` deep-copy in/out** — arrays AND arbitrary concrete structs/graphs
  (cycles + sharing handled via a vtable copy slot + copymap), fault-contained,
  0-live. See [language/M0.2-CAPSULE-ARENA.md](language/M0.2-CAPSULE-ARENA.md).
- **VS Code extension + native debugger + LSP** ([vscode-vire/](../vscode-vire/)):
  highlighting, diagnostics/hover/go-to-def/completion/quick-fixes via a wasm-compiled
  frontend (no toolchain), and breakpoints + local-variable inspection via DWARF + lldb-dap.
- **Cross-compilation**: `--target x86_64-pc-windows-gnu` → a running `.exe`; BSD to
  an object; macOS needs the SDK. See [language/CROSS-COMPILE.md](language/CROSS-COMPILE.md).
- **Faster builds**: cached runtime bitcode (~4× smaller builds) + parallel inline-block
  verification, both lossless.

Performance vs clang++ 22: compute at/above Rust level (montecarlo 0.96×,
nbody/bitmanip ~1.0×), virtual dispatch **2.4× faster** (vcall 0.42×, devirt). Array
kernels lag (sort 1.37×, binsearch 1.16×) — data-dependent bounds checks (see #1).

---

## Performance

### Follow-ups from the perf + fuzzer session (2026-07-21)

See [memory `vire-perf-fuzz-session`] for context.

- [x] ~~**#1 distinct-array alias metadata** (`!alias.scope`/`!noalias`).~~ **RULED
  OUT — measured.** `noalias` on the allocator returns already tells LLVM distinct
  arrays don't alias, and an A/B (with vs without that attribute) is **identical**
  on graph/sort/compression/pquicksort (e.g. graph 0.067 vs 0.066). These are
  **latency/scheduling-bound** (dependent chains: the Dijkstra heap sift, the LZ4
  hash lookup, the quicksort partition), not aliasing-bound — so per-access alias
  metadata adds nothing while carrying real miscompile risk. Not worth building.

Queued:

- [x] ~~**RC inline in the backend (retain/release as IR, not runtime calls).**~~
  **BUILT then REVERTED — measured not worth it.** A prototype emitted acyclic
  `jrt_retain`/`jrt_release` as `internal alwaysinline` IR (fast path + a runtime
  `jrt_drop_at_zero` cold call) and dropped `-flto` for the acyclic mode. It was
  **correct** (Java 65/65, vire_heap 9/9, all suites — the inline inc/dec is
  sound), but the payoff failed on two counts: (1) **dropping `-flto` regresses
  perf** — retain/release aren't the only LTO-inlined hot runtime helpers (struct
  −18%; hashmap noise-level), so the robustness gain costs real time; (2) it only
  covers **acyclic** programs — btree/compiler have self-referential types
  (`Node.l: Node`) so the solver won't prove them acyclic → they keep `-flto` and
  the latent risk anyway. **The direct metadata fix (next item) achieves the same
  robustness while keeping `-flto`/perf and covering ALL programs — strictly
  better.** Do NOT re-attempt the RC-inline rebuild without a way to keep LTO's
  inlining of the *other* hot runtime helpers.
- [x] **Vtable load `!invariant.load` fixed.** (backend.rs)
  Same unsound calloc-then-write pattern as the array length that caused the LTO
  OOB miscompiles — the header is calloc'd (vtable=0) then written. Not
  demonstrated-broken (the fuzzer has no objects/virtual calls), but latent.
  Fix soundly (drop it, or `!invariant.group`/TBAA) before it bites under LTO.
  **Correctness, not perf.**
- [ ] **graph (1.64× Rust) deep-dive.** RAM 55 vs Rust 30 MB — Vire touches ~2×
  the memory (cache pressure); find which arrays are fully touched. The Dijkstra
  binary-heap sift is branchy pointer-chasing (bounds only 12%, not vectorizable);
  try **PGO** (`--pgo-gen/--pgo-use`, already built) on its data-dependent heap
  branches — measure whether it beats 0% here (regular branches saw ~0%).
- [ ] **Expand the differential fuzzer** (tests/fuzz_gen.py) — floats (carefully,
  fp-contract-matched), nested statement control-flow, break/continue, strings.
  Adding bitwise/shifts this round exposed a shift miscompile + the LTO OOB class.
- [ ] **sort 1.15× / pquicksort 1.23×:** residual is the check model + the
  explicit-stack quicksort (a recursive `Array` param version measured slower).

- [~] **#1 Relational bounds elision — headline.** Foundation in
  [crates/solver/src/bounds.rs](crates/solver/src/bounds.rs) (Div/Sub syms, subtract
  axiom, transitive lt, const-length midpoint). **Landed this round:** a constant
  upper/lower-bound Kleene fixpoint over loop phis (`compute_ub`/`compute_lb`) that
  proves `0 ≤ i ≤ ub(i) < lb(len)` across phis — so **binary search's `a[mid]` now
  elides** (`0 ≤ (lo+hi)/2 ≤ n-1 < n`) and **binsearch reaches 1.00× Rust** (was
  1.23×), soundly (a real OOB still throws; Java oracle 65/65). Also tracks lengths
  for `RegionNewArray`/`StackNewArray`, not just `NewArray`. **Also landed:** the
  **guard-aware affine** rule (Path 4) — matmul's `N*a+b < N² ≤ len` from the
  loop-guard facts `a<N`, `b<N` in the flow-sensitive `lt`. matmul 1.64×→**1.22× Rust,
  now beats clang** (0.96×); the inner loop becomes 8× FMA. The residual vs Rust is
  scalar register-alloc/scheduling, not bounds or vectorization (both are scalar).
  **quicksort `sort` — solved, but not by elision.** Measured finding: Rust's sort has
  the *same* bounds checks (in fact 47× more `jae`); the gap was Vire's **check model**,
  not missing elision. The array-content invariant is moot here (and blocked anyway by
  the relational `pi ≤ hi` quicksort invariant). Instead: when the whole program
  provably can't catch a runtime exception (no `InstanceOfPending`/`jrt_take_pending`
  anywhere — always true for pure Vire), an inline bounds/NPE failure aborts via a
  `_fatal` noreturn helper and the block ends in `unreachable`, so the checked access
  is a direct value (Rust's panic structure), not a pending-continue `phi` merge.
  **sort 1.35× → 1.05× Rust**, memory-safe (the check stays; a real OOB still throws),
  gated by the Java oracle (Catch/Finally keep the pending model). Disabled under `-g`
  (inlinedAt precision). The last ~5% is the explicit-stack structure — see below.
- [x] **Array as a function parameter** — **done** (`fn qsort(a: Array[Int], lo, hi) {
  a[i] }`). A param typed `Array[Int]`/`Array[Float]` is a `Ref` whose element kind is
  recorded in `local_arr` at param binding (lower.rs), so `a[i]`, `a[i] = v` and
  `a.len()` in the body lower to real bounds-checked array accesses (was "unknown
  array"). Sound: an OOB in the callee still throws; tests/vire_heap.sh
  `array_param_qsort` (recursive in-place sort, 0-live). **Measured finding (corrects
  the old hypothesis):** rewriting `sort` as a recursive `qsort(a, lo, hi)` is *slower*
  than the explicit `lostack`/`histack` (0.144 vs 0.128 s at 2M) — the per-call overhead
  plus the loss of cross-call bounds elision outweighs the cleaner structure. So the
  explicit stack was **not** overhead; the benchmark stays as-is. The feature's value is
  enabling array-taking helpers generally, not this benchmark.
- [x] **Allocator gap — closed for the array case.** Region inference closed the
  RC gap on traversal; the auto-arena (escape→arena) covers `for`-loops and
  scalar-store loops; and **non-escaping fixed-size primitive arrays now
  stack-promote** (`StackNewArray`→`alloca`, like objects get `StackNew`) — reuses
  the object escape analysis, so a returned/stored array correctly stays on the
  heap (no use-after-return). Measured on the `for … array(16)` loop that was
  ~20× Rust: now **0.27× Rust** (LLVM eliminates the stack array entirely);
  nested-loop variant **0.06× Rust** (was 9.9×). btree stays at 1.08× Rust. All
  sound: Java heap oracle 65/65, benchmark outputs unchanged, tests/vire_heap.sh
  (incl. the escape-return guard). See escape.rs (`STACK_ARR_CAP`).
- [x] **Second (region) stack for dynamic/large arrays + scales to multiple
  stacks.** A non-escaping array too big / dynamically-sized for the call stack
  goes into the bump-region arena instead of the RC heap when it sits in a
  promotable loop body (measured: `array(m)` in a `while`/`for` loop already
  arena-promotes, freed per iteration). The region is now **thread-local**, so
  concurrent `spawn` workers each own an independent region stack — no shared
  global `arena_top` to race on (was a documented threads limit). tests/
  vire_threads.sh `per_thread_arena` (8 workers, deterministic ×20).
- [x] **Function-scoped region** for non-escaping dynamic/large arrays not in a
  loop (allocated once in a straight-line function): bump-allocated in a per-thread
  region (`jrt_region_array`), the function bracketed with `jrt_region_enter/leave`,
  freed en bloc at return. Reuses the object escape analysis (a returned/stored
  array stays heap). Modest win — region ≈ glibc tcache for tiny arrays, ahead for
  medium/large (both memset-bound); the point is avoiding malloc/free+RC. Sound:
  Java 65/65, benchmark outputs unchanged, tests/vire_heap.sh (region_scratch +
  escape-return guard). `FASTLLVM_NO_REGION` routes to the heap (A/B knob).
- [ ] Remaining: ref-element arrays stay heap (element drops); in-loop non-const
  arrays rely on the loop arena / heap (a function region would grow per
  iteration). **pagerank/ring** is the collector case, orthogonal to the allocator.
- [ ] **(M0.3-iv) Field-/interprocedural bounds elision** for `out[k]` (length of a
  field array) — closes part of the residual toward ~1.1×.
- [ ] **(M0.3-v) Overflow default + `+%` culture** (enables vectorization) and
  **analysis caching** (compile time — M0.2 measured super-linear ~O(n^1.4)).
- [ ] **Explicit SIMD for reductions LLVM won't auto-vectorize** (e.g. a *vectorized
  argmin*: the `benchmarks/complex/kmeans` nearest-centroid loop is 0.55× Rust / 1.28× C++
  after the two-pass restructure, but no compiler emits SIMD for the branchy argmin — a
  hand-written vector distance-compute + horizontal-min-with-index would close the last
  ~1.28× to C++). Needs a backend intrinsic path (emit `@llvm.vector.reduce.*` / explicit
  `<N x i64>` ops) or a comptime SIMD library. Deferred — marginal gain over the current
  parity; the two-pass restructure already got the bulk (2.2×).
- [ ] **Codegen scheduling / register allocation (the `benchmarks/complex/` FP losers).**
  Corrected finding (an earlier note here blamed "IR quality" off a misleading asm-region
  count — retracted): Vire's emitted IR **optimizes fine**. Controlled check — the *same*
  program through `opt -O2`: a scalar FP loop matches C exactly (fmul 2 vs 4, same
  loads/allocas); `raytracer`'s whole module is comparable to clang (fmul 24 vs 27, fdiv
  10=10, FMA formed in the backend for both); the i64→i32 index `trunc`/`sext` chains are
  fully eliminated (post-opt trunc=0). So there is **no low-hanging IR-quality fruit**. The
  residual on `raytracer` (1.9×), `graph` (1.6×), `regex`/`pquicksort`/`pipeline`
  (1.1–1.25×) is the LLVM **backend** scheduling/register-allocation reacting to subtle IR
  structure (measured: ~2× the stack spills of clang's binary on the raytracer inner loop),
  plus the in-place-sort check model. This is deep-codegen tuning (instruction ordering,
  reducing live ranges at lowering), not a single fixable pass — low ROI vs the wins
  already banked (7 of 14 at/under Rust).
- [x] **Interprocedural escape/region for short-lived heap graphs** (the
  `benchmarks/complex/compiler` case — was 1.25× C++ / 20 MB, now **1.08× C++ (clang
  parity) / 17 MB**). The heap AST is built in `parse`, consumed in `eval`, dead by the next
  loop iteration. Vire previously RC-managed every node (retain/release on build + traversal).
  **Shipped:** the loop-arena escape check (`while_arena_safe` in `crates/vire/src/lower.rs`)
  is now **interprocedural**. Two context flags decide whether a control-flow statement
  escapes the *arena iteration* rather than the enclosing function, so a callee's own
  `return`/`break`/`continue` no longer disqualifies the arena — the arena is a thread-local
  `arena_top`, so every allocation the iteration transitively performs (across the
  `parse`/`eval` boundary) lands in it and is freed **en bloc** at the pop, with **zero
  per-node RC and zero heap `malloc`**. The field/index store rule now checks the
  *destination* element kind (a ref cannot be stored into an `Array[Int]` slot) resolved
  through callee parameter annotations, so the scalar buffer writes in `gen`/`parse` stop
  blocking promotion. **Soundness-critical** (a wrong escape verdict = use-after-free): pinned
  in both directions — promote *and* decline — by [`tests/vire_interproc_arena.sh`](tests/vire_interproc_arena.sh),
  which also covers two latent bugs this work closed (an outer-var store nested inside an
  `if`, and a `break`/`continue` that skipped the pop — both previously slipped past the old
  top-level-only check). Ruled out earlier: a node-pool/SoA rewrite is *slower* (0.040 s).
  The residual gap **to Rust** is the same input-free constant-fold artifact as `json`, not
  RC. Java oracle 65/65 and all 117 Vire suite cases stay green.

## Compile-time programming layer (macros + comptime + reflection, one typed AST)

**Framing (deliberate).** Avoid the term *preprocessor* in docs and naming: this
is a **compile-time programming layer**, not text substitution. Macros, `comptime`,
and reflection (`@typeinfo`/`@derive`) all operate on the *same typed AST / type
graph* — so users get the power of metaprogramming without the classic C-preprocessor
failure modes (blind text splicing, name capture, no type checking). Everything runs
*after* parse+inference on typed nodes and is re-checked after expansion. This unifies
features **[2] comptime**, **[3] reflection**, and **[4] macros** below — they are one
subsystem, sequenced.

**Rebuild path (chosen): typed AST first.** No typed representation survives past
`lower.rs` today, so the foundation comes before features:
- [x] **Phase 0 — persisted type graph.** [tygraph.rs](crates/vire/src/tygraph.rs):
  a source-level, structural `TypeGraph::build(&Module)` (product/sum types with
  generics + variants, trait method sigs, impls, fn sigs) built *after* inference,
  decoupled from lowering (lower.rs untouched, still builds its own IR-erased maps).
  Preserves what the IR lattice erases — generics, nested type apps, borrow marks —
  i.e. exactly what reflection reads. Introspect with `vire types FILE.vr`.
  tests/vire_types.sh (15/15).
- [x] **Phase 1 — typed expressions.** [infer.rs](crates/vire/src/infer.rs)
  `infer_module_typed` now returns an `ExprTypes` side-table: the resolved type of
  every expression keyed by source span (`Span` gained `Hash`/`Ord`). AST nodes have
  no identity, so the byte-range span is the key. `InferTy` = Int/Float/Bool/Ref/
  Unit/Unknown — `Unknown` is an honest "inference couldn't constrain it", not a
  default. Inference logic unchanged (recording is a pure addition); `infer_module`
  is a thin wrapper. Introspect with `vire infer FILE.vr`. tests/vire_infer.sh (8/8).
  Still open (Phase 1b): richer than the scalar lattice — user-type/generic identity
  per expression (today they collapse to `Ref`), and synthesized/`Span(0,0)` nodes
  from desugaring share keys.
- [~] **Phase 2 — move passes after inference.** comptime folding now lives in a
  dedicated post-inference pass ([comptime.rs](crates/vire/src/comptime.rs)
  `eval_comptime`, run after `infer_module`), not fused inside lowering: it collects
  module `const` declarations into a compile-time environment, inlines `const`
  references to literals (respecting lexical shadowing — a local of the same name
  wins), and folds `comptime`/`comptime if` on the AST. **`const` now actually works**
  (value, `comptime`, array size — all previously broken: `unknown variable`).
  Best-effort/non-regressive: unresolvable comptime (e.g. a value-generic `N`) defers
  to lowering. tests/vire_comptime.sh (5/5). Still open: move **macro expansion**
  after inference too (it still runs before — the untyped anti-pattern), and have the
  pass consult the type graph / typed AST (type-aware `comptime if`).
- [ ] **Phase 3+ — features on the foundation:** the sequence below.

Feature sequence on top:

- [x] **(a) comptime evaluator core** — a budget-limited interpreter in
  [comptime.rs](crates/vire/src/comptime.rs) (`Interp`): comptime `let`/assignment,
  `for`/`while` executed at compile time (value accumulation), `if`, and calls to
  pure module functions (`comptime f(x)`) with recursion — all with a step +
  recursion budget (an infinite comptime loop is a compile error, not a hang) and
  lexical isolation (a callee sees only its params + consts). Powers const
  initializers (`const F = fact(6)`), comptime array sizes (`array(comptime fact(4))`),
  and comptime blocks. Anything non-constant (runtime op, unbound name) defers to
  lowering. tests/vire_comptime.sh (9/9). Open: comptime `for` *unrolling* into
  runtime statements (this executes at comptime to a value; unrolling is separate),
  comptime over reference/aggregate values (scalars only today), `return`/`break` in
  a comptime body.
- [~] **(b) typed reflection over the type graph** — **`@derive(Eq, Show, Ord, Hash)`**
  works ([derive.rs](crates/vire/src/derive.rs)): a `@name(args)` attribute parses onto
  a `type` (new `Attr` AST node + `parse_attrs`), and a post-macro pass reads the type
  structure and synthesizes ordinary methods that infer+lower like hand-written ones —
  **product types**: `eq`/`show`/`cmp` (lexicographic -1/0/1)/`hash` (31-combiner;
  numeric/Bool by value, `Str` via `hashCode()`, per-field-type aware); **sum types**:
  `eq`/`show` via a `match` on the tag (dataless + multi-field variants). An explicit
  method of the same name overrides the derive. **Full matrix — product AND sum:**
  Eq/Show/Ord/Hash/Json. (Sum Ord orders by variant declaration ordinal via a nested
  match, then payloads lexicographically; sum Hash folds ordinal + payload; sum Json
  renders `{"V":[…]}` / `"V"`.) Rejected with a clear message: unknown derives,
  nested-user-type fields (Ord/Hash/Json), generic targets. The type graph reflects
  declared derives (`vire types`). tests/vire_derive.sh (13/13). Open: **generic**
  types (needs generic-method monomorphization in lower.rs — inherent methods on a
  generic type do not currently instantiate); nested-user-type fields (recursive
  derive); JSON string escaping; and the deeper **`@typeinfo(T)`** as a
  *comptime-iterable typed value* (needs aggregate comptime values — the interpreter is
  scalar-only today), from which derives would be written in-language rather than
  hard-coded in Rust.
- [~] **(c) hygienic item macros** — `macro name(P: type, n: ident, e: expr) { <items> }`
  invoked `name!(args)` ([itemmacro.rs](crates/vire/src/itemmacro.rs)): expands to
  declarations (`fn`/`type`/`impl`/`const`). Safe by construction — the C-preprocessor
  hazards cannot occur: **AST-level** (no text/token pasting), **kind-checked params**
  (`type`/`ident`/`expr` — an arg of the wrong kind is a hard error, so an expression
  can't be spliced where a type belongs), **hygiene** (macro-body bindings gensym-renamed
  per expansion → no capture either way), **type-checked after expansion** (runs before
  inference, so generated items go through the full checker), and **duplicate generated
  names are a clear front-end error**, never a silent merge. tests/vire_itemmacro.sh (8/8).
  **Nested invocations** (a macro body invokes another item macro) expand to a
  fixpoint with a round limit (a diverging/self-invoking macro is a compile error,
  not a hang). **Generic type arguments** in a `type` parameter work — `holder!(H,
  Box[Int])` lands `boxed: Box[Int]` as a real type application (`vire types` now runs
  the full front-end incl. item-macro + derive expansion, so generated declarations
  show up). tests/vire_itemmacro.sh (11/11). Open: token **pasting** (deliberately
  omitted — needs identifier interpolation; for now pass each generated name as its own
  `ident` param); multi-argument generics (`Map[K, V]`); `block`/`pat` parameter kinds.
  Expression macros (`macro name(p) = <expr>`) are unchanged.
- [x] `@when(platform)` conditional compilation — landed (see [4]).
- [x] `comptime assert(cond[, "msg"])` — done: the condition is evaluated at compile
  time (via the comptime interpreter, incl. `comptime` fn calls); false/zero → a compile
  error with the message; a non-constant condition is rejected. Folds to a no-op.
  tests/vire_comptime.sh.
- [ ] `comptime for` (loop unrolling to runtime statements) / `emit` surface syntax.

## Front-end completeness

- [ ] **`vire fmt`** (roundtrip AST→source) as parser-fuzz insurance.
- [~] **Error messages** — panic-mode recovery now collects multiple diagnostics;
  still open: fix suggestions and pointing near the true cause.
- [~] **Trait resolution + coherence.** Duplicate/overlapping method definitions per
  type are rejected; **bounded generics `[T: Trait]` resolve + are enforced** (see
  [2]). Still open: overlapping **generic** impls, coherence across impls.
- [~] **Monomorphization** — works via the inliner/`instantiate`; full value-generic
  monomorphization (`[comptime N: Int]`, distinct instances per N) is open.
- [~] **`comptime` evaluator.** `comptime if` (conditional compilation, drops the
  untaken branch) and const-folding work. Open: a real interpreter over the
  AST/type-graph — comptime `let`/`for` (loop unrolling), comptime function calls,
  recursion limit.
- [~] **Macro expander** ([crates/vire/src/expand.rs](crates/vire/src/expand.rs)) —
  expression macros work; hygienic block/typed-parameter macros are open (see #4).
- [~] **Iterator-mutation check** ([REFERENCE.md](language/REFERENCE.md) §9a) — local
  non-mutation analysis; not provable → compile error.

## Stdlib + FFI

- [~] **Collections breadth.** `list()` (push/pop/len/get/set/contains/clear),
  `map()` (put/get/has/remove/len), and **`set()`** (add/contains/remove/len — a
  hash Set reusing the map runtime) exist. **`Str` methods** now dispatch too
  (length/charAt/substring/indexOf/startsWith/endsWith/trim/lower/upper/isEmpty/
  equals/compareTo → `jrt_str_*`; chainable, a string receiver is a bare `Ty::Ref`).
  **Iterator adapters** (`fold`/`sum`/`count`/`map`/`filter`/`each`) now work over
  ranges and lists: the lambda body inlines per element into a generated counting
  loop (no closure object — LLVM fuses the pipeline like a hand loop). `map`/`filter`
  yield a new `$List`, so pipelines chain (`(1..=10).filter(..).map(..).sum()`).
  **Statement-bodied lambdas** now work: `each(x -> total = total + x)` (and `+=` etc.)
  — a braceless assignment body is wrapped in a unit-valued block (parser
  `parse_lambda_body`); `x -> { … }` already worked. tests/vire_iter.sh.
  Open: **`Str.split`** (needs a typed `list[Str]` — elements are pointers, not
  `Int`), and the full **`Option`/`Result`** surface (`.wrap(msg)` context/chain — the
  core `?`/`match` works).

---

## GPU kernels (`@gpu`) — built 2026-07-21

Single-source device functions: `@gpu fn` → nvptx64 LLVM module → PTX (`llc`) →
embedded in the binary → launched via the CUDA Driver API (libcuda). Kernels live
in `Program::gpu_kernels` (out of `functions`, so no host solver/RTA/inliner
touches them); the backend emits device IR + a C launch stub per kernel. Intrinsics
`gpu_gid/gpu_gsize/tid/bid/bdim/gdim`. Design adapted from NVlabs/cuda-oxide
(Apache-2.0, `crates/cuda-oxide`). Docs: [language/GPU-KERNELS.md]. Guarded by
`tests/vire_gpu.sh` (integer bit-exact vs CPU + error path). Measured **16× vs CPU**
at high intensity on an RTX 5070 ([benchmarks/gpu/]). Separate GPU track — the CPU
suite stays bit-identical.

Follow-ups (open):
- [ ] **Read-only array analysis** — skip the D2H copyback (and possibly H2D) for
  arrays a kernel only reads; v1 treats every array arg as in/out.
- [ ] **Explicit launch config** — let a kernel/call choose block size / 2-D & 3-D
  grids and shared memory, instead of the fixed `block=256, grid=ceil(N/256)`.
- [ ] **Sub-word + Ref arrays on device**, `Array<F32>` scalars, and device-side
  math intrinsics (sqrt/exp via `@llvm.nvvm.*`).
- [ ] **Persistent context / async** — reuse device buffers across launches, and a
  non-synchronous launch path (v1 syncs every launch; GPU+threads is out of scope).
- [ ] **Fair Rust-GPU baseline** — build cuda-oxide (needs its rustc backend
  toolchain) to fill the Vire-GPU vs Rust-GPU column in benchmarks/gpu.
- [x] Host `farray[i] = <int>` now coerces int→f64 (was invalid IR) — seeds float
  kernels cleanly.

## Features 1–8 (open parts only)

### [1] Multithreading, safe by construction
Attach: backend `--threads` (atomic RC, pthreads, monitor) — present.
- [x] **`spawn worker(args…)` + `join(h)`** — Vire frontend wired to the runtime
  via a generated per-worker C shim + `jrt_spawn` (function-pointer thread model);
  threads auto-enable on `spawn`. **Multi-argument** workers pack their args into
  an immortal env buffer. Workers kept as RTA roots via `Program.exported`. See
  spawn.rs.
- [x] **`Atomic`** (`.fetch_add`/`.load`) and **`Mutex`** (`.lock`/`.unlock`/
  `.get`/`.set`) — shared, race-free primitives (immortal header objects like
  `list()`). tests/vire_threads.sh (8/8; atomic + mutex counters deterministic
  ×20). Runnable demos in examples/vire/threads_*.vr.
- [x] **Send check**: a `spawn` worker's parameter must be a scalar (copied) or a
  Sync type (`Atomic`/`Mutex`); sharing a bare mutable record/list is a compile
  error — a data race cannot be written.
- [x] **`Channel`** (`.send`/`.recv`, blocking) — thread-safe FIFO message passing;
  a Sync type, may cross `spawn`. tests/vire_threads.sh, examples/vire/threads_channel.vr.
- [ ] `Mutex.lock(closure)` (scoped-guard form); `parallel_map`; typed `Channel[T]`
  for ref payloads (currently Int values).
- [ ] (M0.1c) measure real multithread atomic contention.

### [2] Template programming
Attach: monomorphization (front-end) + `comptime`.
- [x] Generics `[T: Trait]`, multiple bounds `T: A + B`, **static trait resolution
  → direct (in fact inlined) calls** — works via monomorphization; a violated
  bound is now a precise compile error at the instantiation (enforced in the mono
  worklist). tests/vire_generics.sh.
- [x] **Value generics `[comptime N: Int]`** with call-site **turbofish** `f[N](..)`
  (parser disambiguates from indexing by the trailing `(`): distinct monomorph per
  N, N substituted as a literal — so `0..N`/`array(N)` become constant (the array
  then stack-promotes). Mixed type+value turbofish; extra type params still
  inferred. tests/vire_generics.sh, examples/vire/value_generics.vr.
- [x] Array **parameter** indexing in a Vire body (`fn f(a: Array[Int]){ a[i] }`) —
  **done** (see [1]). 
- [ ] Fixed arrays `[T; N]` as a distinct inline-storage value type (a larger
  feature; value-generic `array(N)` already gives constant-size stack arrays).
- [ ] Overlapping/coherence checking for generic impls; inference of a type arg that
  appears only in return position (defaults to `Int` today).

### [3] Compile-time reflection
Attach: whole-program type graph + `comptime`.
- [ ] `@typeinfo(T)` (fields/variants/methods/attributes, comptime-iterable).
- [ ] `@derive(Json, Eq, Hash, Ord, …)` via reflection.
- [x] `comptime assert` — landed (see [4]). - [ ] `comptime for`, `emit`. **No** runtime reflection (AOT).

### [4] Own optional preprocessor *(= comptime/@if/macros)*
- [ ] Hygienic macros (`macro name(args) { … }`): **typed parameters** (`expr`/`block`/
  `ident`/`pat`/`type`), **full type-checking after expansion**, hygiene (no capture),
  diagnostic spans into the expansion.
- [x] `@when` platform switches — **done**. `@when(linux|macos|windows|unix)` on a `fn`
  or `type` includes it only for the matching target (host by default, or `--target`
  triple), dropped before inference so two same-named per-platform fns don't clash;
  `@when(unix)` = linux+macos; unknown platform is a compile error. crates/vire/src/
  platform.rs, tests/vire_comptime.sh.

### [5] Build interop, Meson first-class — **DONE**
Attach: clang→object (present).
- [x] Stable compiler CLI: `--emit=obj|asm|llvm|ir|staticlib`, `--deps` (Makefile/Ninja
      depfile), `-I DIR`. A whole `.vr` program lowers to ONE relocatable C-ABI object
      (runtime `main` included), mergeable via `clang -r`; `--emit=staticlib` → `.a`.
- [x] Meson integration `vire` (`vire.executable/static_library`), C-ABI `.o`/`.a`:
      `build-integration/meson/` — a tested stock-DSL `custom_target` pattern (builds +
      runs, links a Vire object with a C object → 42) and an optional `import('vire')`
      Python module (`vire.py`). Incremental via the `--deps` depfile.
- [x] pkg-config deps first-class: `--pkg NAME` resolves `--cflags`/`--libs` and forwards
      them to both the native-block compile and the link (tested against zlib). The
      binding generator (`vire bindgen` / `extern "C" header "…"`) already covers headers.

### [6] Logger — remaining
The **compile-time level filter** (disabled calls = 0 instructions) works.
- [x] **Structured fields** via `{}` interpolation: `log.info("user={} ms={}", id, t)`
  → `[INFO] user=<id> ms=<t>`, built at compile time (positional args, so the
  zero-cost-when-disabled property holds); a placeholder/arg mismatch is a compile error.
- [x] **Build-time level** `--log-level debug|info|warn|error|off` (env `FASTLLVM_LOG_LEVEL`),
  default info; below-threshold calls lower to nothing. tests/vire_log.sh.
- [ ] `with log.span(...)` (scoped context fields).
- [ ] Sinks (colored console / JSON / file), chosen at build time.

### [7] Go-style error handling — remaining
`Result[T,E]`/`Option[T]` + `?` and `match` work end-to-end.
- [ ] `.wrap(msg)` (context, chain), typed errors with attached debug path.

### [8] Debug symbols + crash paths
Attach: LLVM debug metadata (backend extension), panic model.
- [x] **`--backtrace`**: native backtrace on an uncaught exception / hard crash
  (SIGSEGV/SIGBUS handler), captured at the throw origin, printed only if
  uncaught. Symbol names via `-rdynamic`. Off by default → zero overhead (empty
  stubs). tests/vire_debug.sh.
- [x] **DWARF debug info** (`--debug`/`-g`): `DICompileUnit`/`DIFile`/per-function
  `DISubprogram`+`DILocation` mapping to the `.vr` source. Debug builds are
  `-O0 -no-pie` so gdb/lldb/addr2line resolve backtrace addresses to `.vr:line`
  (`--debug --backtrace` → `addr2line` → `crash.vr:6`). Source lines threaded
  front-end→IR (`Function.line`). tests/vire_debug.sh.
- [x] **Per-statement `DILocation`** (the exact crash line): lowering emits
  `DebugLine` markers per statement/tail (debug builds only; they survive the
  optimizing passes), the backend maps each to a `!DILocation`. A bounds crash now
  resolves to the precise access line (`crash.vr:4`), not the function's line.
- [x] **`inlinedAt` inline chains**: when Vire's own inliner splices a callee into
  a caller, each DebugLine carries the inline stack `(fn, line)` innermost-first;
  the backend builds a `!DILocation`→`inlinedAt`→`!DILocation` chain. `addr2line -i`
  / gdb show the full chain (`compute` at crash.vr:4, inlined at `main` crash.vr:6).
- [ ] freestanding: compact symbol table instead of libc `backtrace`; map the
  entry symbol `java_main` back to `main` in the DISubprogram name (cosmetic).
- [ ] freestanding: compact symbol table instead of libc `backtrace`.

---

## Cross-cutting

- [~] **Compile time** whole-program+mono+comptime — measured super-linear; analysis
  caching / incremental is open.
- [ ] **Overflow default**: checked also in release, wrapping only explicit
  ([REFERENCE.md](language/REFERENCE.md) §3.1).

## External usage findings — Baby-LOOM emulator (2026-07-21)

Real-workload dogfooding: a MoE-inference emulator in Vire (YARN quantizer, EXPFFN
gate/up/SwiGLU/down, top-k router, GPU matvec) — see
`~/Schreibtisch/MoE Hardware/baby-loom-sim/`. Mostly smooth; two rough edges:

- [x] **FIXED — root cause was a PARSER call-adjacency ambiguity, not lowering.** The symptom
  `lowering: call target M2: only named functions` was collateral: `parse_postfix` bound a `(`
  as a call-arg list to *any* preceding expression, even across whitespace/newline. Because Vire
  separates same-line statements at expression boundaries (Go-like NL terminators, but multiple
  stmts per physical line), a tail like `mut y = x + 0.5  (y as Int)` parsed as the **call**
  `(x+0.5)(y as Int)` — callee not an `Ident` → the M2 error. Same for `if …else… (e)` (if is an
  expression) and any `value  (…)` on one line. **Fix:** in `parse_postfix`
  ([crates/vire/src/parser.rs](crates/vire/src/parser.rs) `Tok::LParen` arm) a `(` forms a call
  only when **adjacent** to the callee (`toks[pos-1].span.1 == toks[pos].span.0`); otherwise it
  starts a new parenthesised-expression statement. `f(x)` = call, `f (x)` = two stmts. Verified:
  the whole corpus uses `f(x)` (all 83 `ident (` hits are in comments), so no real call regresses.
  Vire suites green (types/iter/heap/str/generics/infer/comptime/derive/itemmacro/gpu/threads/log
  = 109/109). Repros `bugA` (trailing cast) and `3  (a)` now build+run; baby-loom workarounds removed.
- [x] **`farray` allocation in a helper fn — same root cause.** `mut h = farray(dff)` in a
  non-`main` helper only failed because a *later* line (`… (y as Int)` / a stray `value (…)`)
  poisoned whole-program lowering. With the parser fix, `mut h = farray(n)  h[0] = 1.0` in any
  fn builds+runs. Not a real allocation restriction.

**Worked well (no action):** `farray`/`array` params with in-place writes through helpers;
nested `while`; `if/else` statements; `%`, casts, Float arithmetic; **`@gpu` kernels with
`farray` params + a `while`-loop reduction ran bit-exact vs CPU** (matvec, 128/128 rows). The
whole `vire run` pipeline (incl. NVPTX→CUDA) was reliable for a non-trivial numeric workload.

## Cross-compilation (see language/CROSS-COMPILE.md)

Measured from a Linux host. **Windows works** now (`--target x86_64-pc-windows-gnu`
→ running `.exe`, via the `_WIN32` time branch + `-fuse-ld=lld`). Follow-ups:

- [ ] **macOS cross-compile** — needs the macOS SDK (not redistributable). Wire up
  [osxcross](https://github.com/tpoechtrager/osxcross): detect an `OSXCROSS_ROOT`/
  SDK and pass `--sysroot` + the right `-target` (`arm64-apple-macos`,
  `x86_64-apple-darwin`) so `runtime.c` compiles against Darwin headers instead of
  falling back to the host's Linux `stdio.h`. The runtime code itself is already
  portable; only the SDK is missing.
- [ ] **FreeBSD/BSD full build** — object emit works today; add sysroot handling
  (`--sysroot <freebsd-root>`) so linking an executable succeeds here rather than
  needing to link on the target. `@when(freebsd)`/unix-family gating already
  resolves (platform.rs).
- [ ] **aarch64 targets** — verify `aarch64-pc-windows-gnu` (llvm-mingw) and
  `aarch64-unknown-linux-gnu` end to end (untested; codegen should already work).
- [ ] Windows **threads** produce a `.exe` (winpthreads) but execution under wine
  was flaky — verify on real Windows.

## Compile-speed follow-ups (see below / this session)

- [x] **Cache the runtime bitcode — DONE.** `runtime.c` is identical every build yet
  its `-O2 -flto -c` bitcode gen was **~0.4 s — ~80% of a small build**. Now
  precompiled to a cached `.o` in `~/.cache/vire/` keyed by (content, `-D` flags,
  target, clang version) and fed to the LTO link (main.rs `cached_runtime_object`).
  Lossless — same bitcode in → same LTO out (all vire suites green incl. heap 0-live,
  Java oracle 65/65, outputs identical). **Measured: empty build 0.48 s → 0.12 s,
  no-inline 0.51 s → 0.14 s (~4×).** Skipped under PGO (runtime shares the program's
  instrumentation) and under `-g`/freestanding (no `-flto`).
- [ ] **Parallelize native-block verification.** Cold `@c`/`@asm` verification
  (clang `-emit-llvm` + CSolver symbolic exec) runs sequentially per block; the
  content-addressed PASS cache already makes warm builds instant, but multi-block
  cold builds could verify blocks concurrently (CSolver is a library call).

## Non-goals (deliberate)
Runtime `eval`/reflection · dynamic loading of unknown code · C-text preprocessor ·
deadlock-freedom guarantee · "all" C++/Rust libraries beyond the C-ABI boundary.
