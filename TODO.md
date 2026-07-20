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
Soundness floor: the Java heap-balance oracle stays **65/65** and the Vire suite
**63/63** after every change.

Performance vs clang++ 22: compute at/above Rust level (montecarlo 0.96×,
nbody/bitmanip ~1.0×), virtual dispatch **2.4× faster** (vcall 0.42×, devirt). Array
kernels lag (sort 1.37×, binsearch 1.16×) — data-dependent bounds checks (see #1).

---

## Performance

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

## Non-goals (deliberate)
Runtime `eval`/reflection · dynamic loading of unknown code · C-text preprocessor ·
deadlock-freedom guarantee · "all" C++/Rust libraries beyond the C-ABI boundary.
