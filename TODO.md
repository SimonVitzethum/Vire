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

- [~] **#1 Relational bounds elision — headline.** The sound foundation is in
  ([crates/solver/src/bounds.rs](crates/solver/src/bounds.rs): Div/Sub syms,
  subtract axiom, transitive lt, const-length midpoint) so *loop-invariant-bounded*
  indices (`a[i]` in `while i < hi` with `hi = len-1`) elide. **Still open:** binary
  search's `a[mid]` and quicksort partition — their `lo<len ∧ hi<len` are carried by
  loop **phis**, needing a phi-aware greatest-fixpoint + tracking of the non-strict
  `<=` loop guard. No production compiler does this for these patterns (rustc keeps
  the check); payoff caps at Rust parity (~14% on binsearch), not below clang.
- [~] **Allocator gap.** Region inference closed the RC gap on traversal; the
  auto-arena (escape→arena) now covers `for`-loops and non-escaping scalar-store
  loops too, and arrays participate in the arena (all sound, heap-balanced —
  tests/vire_heap.sh). btree measures **1.08× Rust / 1.38× C++** (the cited
  malloc-per-node case is already at parity). **Still open — the real array win:**
  *stack-promote non-escaping fixed-size arrays* (`StackNewArray`→alloca, like
  objects get `StackNew`). Measured: a `for` loop over `array(16)` is ~20× Rust,
  because clang *eliminates* the non-escaping alloc entirely (scalar replacement)
  while Vire still allocates. Needs: array alloc-site escape collection +
  const-size gate + IR variant + backend sized-alloca. Bounded, high-value.
- [ ] **pagerank/ring** is the collector case (persistent shared cycle), not an
  allocator one — the distance there is the cycle collector, orthogonal to arenas.
- [ ] **(M0.3-iv) Field-/interprocedural bounds elision** for `out[k]` (length of a
  field array) — closes part of the residual toward ~1.1×.
- [ ] **(M0.3-v) Overflow default + `+%` culture** (enables vectorization) and
  **analysis caching** (compile time — M0.2 measured super-linear ~O(n^1.4)).

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

- [~] **Collections breadth.** `list()` (push/pop/len/get/set/contains/clear) and
  `map()` (put/get/has/remove/len) exist. Open: **`Str` methods** (lower/upper/split/
  trim/…), **`Set`**, **iterators/adapters** (map/filter/fold over lists & ranges),
  and the full **`Option`/`Result`** surface (`.wrap(msg)` context/chain — the core
  `?`/`match` works).

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
- [ ] `Channel[T]`; `Mutex.lock(closure)` (scoped-guard form); `parallel_map`/
  `parallel_for` (fork-join).
- [ ] (M0.1c) measure real multithread atomic contention.

### [2] Template programming
Attach: monomorphization (front-end) + `comptime`.
- [x] Generics `[T: Trait]`, multiple bounds `T: A + B`, **static trait resolution
  → direct (in fact inlined) calls** — works via monomorphization; a violated
  bound is now a precise compile error at the instantiation (enforced in the mono
  worklist). tests/vire_generics.sh.
- [ ] Value generics `[comptime N: Int]`, fixed arrays `[T; N]`. Bounds/`is_comptime`
  parse but value generics need call-site turbofish `f[N](..)` (parser lookahead vs
  indexing) + value substitution; fixed arrays need `[T; N]` in `parse_type`.
- [ ] Overlapping/coherence checking for generic impls; inference of a type arg that
  appears only in return position (defaults to `Int` today).

### [3] Compile-time reflection
Attach: whole-program type graph + `comptime`.
- [ ] `@typeinfo(T)` (fields/variants/methods/attributes, comptime-iterable).
- [ ] `@derive(Json, Eq, Hash, Ord, …)` via reflection.
- [ ] `comptime for/assert`, `emit`. **No** runtime reflection (AOT).

### [4] Own optional preprocessor *(= comptime/@if/macros)*
- [ ] Hygienic macros (`macro name(args) { … }`): **typed parameters** (`expr`/`block`/
  `ident`/`pat`/`type`), **full type-checking after expansion**, hygiene (no capture),
  diagnostic spans into the expansion.
- [ ] `@when` platform switches (the `comptime if` conditional-compilation primitive
  already lands).

### [5] Build interop, Meson first-class
Attach: clang→object (present).
- [ ] Stable compiler CLI (`--emit=obj|llvm|asm`, `-O`, `--deps` Ninja `.d`).
- [ ] Meson module `vire` (`vire.executable/static_library`), C-ABI `.o`/`.a`.
- [ ] `vire build` wrapper delegates to Meson; pkg-config deps → binding generator.

### [6] Logger — remaining
The **compile-time level filter** (disabled calls = 0 instructions) works.
- [ ] Structured fields, `with log.span(...)`.
- [ ] Sinks (colored console / JSON / file), chosen at build time.

### [7] Go-style error handling — remaining
`Result[T,E]`/`Option[T]` + `?` and `match` work end-to-end.
- [ ] `.wrap(msg)` (context, chain), typed errors with attached debug path.

### [8] Debug symbols + crash paths
Attach: LLVM debug metadata (backend extension), panic model.
- [ ] Thread line numbers front-end→IR; emit `!DILocation`/`!DISubprogram`.
- [ ] Debug runtime backtrace (`file:line:function`) on panic/bounds/null.
- [ ] Off by default in release (0 overhead), `--release --backtrace` opt-in.
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
