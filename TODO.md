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
- [ ] **Allocator gap (pagerank/binary-trees).** Region inference has closed the RC
  gap on the traversal path (hot loop is retain/release-free); the residual ~2×–2.7×
  to Rust/C++ is **malloc-per-node vs bulk/arena allocation** — addressable via
  capsule/arena promotion, orthogonal to borrow analysis (see M0.2 / capsule docs).
- [ ] **(M0.3-iv) Field-/interprocedural bounds elision** for `out[k]` (length of a
  field array) — closes part of the residual toward ~1.1×.
- [ ] **(M0.3-v) Overflow default + `+%` culture** (enables vectorization) and
  **analysis caching** (compile time — M0.2 measured super-linear ~O(n^1.4)).

## Front-end completeness

- [ ] **`vire fmt`** (roundtrip AST→source) as parser-fuzz insurance.
- [~] **Error messages** — panic-mode recovery now collects multiple diagnostics;
  still open: fix suggestions and pointing near the true cause.
- [~] **Trait resolution + coherence.** Duplicate/overlapping method definitions per
  type are now rejected; still open: overlapping **generic** impls, full trait
  resolution beyond the flat monomorphic case.
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
- [ ] `Channel[T]`, `spawn`, `Mutex[T]`, `Atomic[T]` in the stdlib (`spawn` keyword
  is lexed but not yet parsed/lowered).
- [ ] `parallel_map`/`parallel_for` (fork-join).
- [ ] **Send check**: a value passed to `spawn` must be moved/copied *or* a Sync type,
  else a compile error (conservative — same analysis as the iterator check §9a).
- [ ] (M0.1c) measure real multithread atomic contention.

### [2] Template programming
Attach: monomorphization (front-end) + `comptime`.
- [ ] Generics `[T: Trait]`, multiple bounds.
- [ ] Value generics `[comptime N: Int]`, fixed arrays `[T; N]`.
- [ ] Monomorphization + static trait resolution → direct calls.

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
