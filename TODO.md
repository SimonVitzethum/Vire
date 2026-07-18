# Vire — Roadmap (features 1–8 + compiler pipeline)

Task list for the implementation. Ordered by dependency and risk.
Design basis: [language/](language/). Legend: `[ ]` open · `[~]` partial · `[x]` done.

---

## Current state (2026-07)

The **whole pipeline is functional**: lexer → parser → macro expansion → recursive
inline → type inference → lowering to SSA IR → whole-program solver → LLVM backend →
`clang -O2 -flto -march=native`. `vire build foo.vr -o foo` and `vire run foo.vr`
produce and run native binaries today. Traits (vtable dispatch + devirtualization),
arrays, structs/records, generics-by-inlining, and a set of example programs compile
and run; the benchmark suite (sort, binsearch, vcall, matmul, nbody, montecarlo,
bitmanip, pagerank) runs against Rust/clang/gcc.

**Soundness floor:** the Java-bytecode path's **65 heap-balance regression tests
(0 live objects at exit)** stay green after every change — the oracle for the RC/
collector/elision work.

**Performance (vs clang++ 22, best-of-5, output-verified):** at or above Rust level
on compute (montecarlo 0.96×, nbody/bitmanip ~1.0×) and **2.4× faster on virtual
dispatch** (vcall 0.42×, via solver devirtualization). Array-heavy kernels still lag
(sort 1.37×, binsearch 1.16×) — data-dependent bounds checks.

### What's still to do (priority order)

1. **Relational bounds elision** (the largest measured gap). Elide the data-dependent
   index check on `a[mid]` / quicksort partition: prove `mid = (lo+hi)/2 < len` from
   `lo < len ∧ hi < len` via a saturating lt-domain (div-sum rule + `x−1 < x` axiom,
   greatest-fixpoint on the loop invariant). Replaces today's guard-only lt-analysis
   in [crates/solver/src/bounds.rs](crates/solver/src/bounds.rs). Caps at Rust parity
   (no production compiler elides the non-affine binary-search check; Rust keeps it).
2. **Region-borrow inference (M0.3 gate-opener)** — see M0 below; prove loop-stable
   containers borrowable → drop loop retain/release → defuses the collector for free.
3. **Front-end completeness** — `comptime` evaluator, full monomorphization, trait
   coherence, error-recovery quality, `vire fmt` (P2/P3/P4 below).
4. **Stdlib breadth + FFI polish** — `Str`, `List`/`Map`/`Set`, iterators,
   `Option`/`Result`, the C-header binding generator (P5).
5. **Features 1–8** — concurrency stdlib, generics surface, comptime reflection,
   hygienic macros, Meson integration, logger, Go-style errors, debug/backtrace.

---

## M0 — risk measurement (gate) — ✅ EXECUTED, verdict: **conditional go**

Full report: **[language/M0-MEASUREMENT.md](language/M0-MEASUREMENT.md)**. Programs:
[benchmarks/m0/](benchmarks/m0/). Measured over the **real automatic pipeline** (the
solver does the inference — not hand lowering), oracle↔automatic spread.

- [x] **M0.1 alias precision.** Adversarial PageRank object graph (shared/escaping/
  mutating/cyclic). Result: **>1000× slower** than Rust at 100k (collector
  super-linear/timeout), **4.4×** without the collector, **6.3×** atomic RC
  (uncontended). The oracle(=0 RC)↔automatic spread is maximal → the inference does
  **not** recover the borrow facts in the shared/cyclic case. "Rust-level without
  annotations" is a **slogan** on this subset.
- [x] **M0.2 compile time.** Solver+backend super-linear (~O(n^1.4)): 50k LOC = 1.8 s,
  extrapolated ~5–7 s at 100k — **without** incremental caching.
- [~] **M0.1 contention** (rest): real multithread contention as a separate experiment
  is open; 6.3× uncontended is the lower bound.
- [x] **M0.1b (the decisive extra measurement):** RC separated from the object model
  (collector off, N=16k): with RC 4.4×, **without RC 1.48×**, Rust 1×. → The RC is
  **3.4× and elidable** (the loop is topology-stable = provably borrowable); the
  solver did **not** prove the borrowability (completeness gap, not a §7 wall).
  Ceiling = **~1.5×** (object model), not 1×.

**M0.3 decision — the repair is ONE thing, not two parallel ones:**
- [ ] **(ii) region-borrow inference** (the gate-opener): prove loop-stable containers
  (`nodes[]`, `n.out` — not reassigned in the loop) borrowable as a region → drop the
  loop retain/release. **This defuses the collector for free** (no loop releases → no
  cycle candidates → no O(n²)). Goal: 108× → ~1.5×. Soundness-delicate (0 live!): only
  with a region-/dominance-scoped "no store rebinds the borrowed slot" proof. **This
  is the ownership-inference module** — careful, not quick.
- [x] **(i) collector scaling** — DONE (adaptive threshold 2×live → linear; 108×→~7×)
  + iterative drop/collect (soundness: N=200k crash→runs). Not needed further for this
  pattern; stays relevant for *genuinely* cyclic programs. **Note the tension:**
  incremental/generational = write barriers per mutation (re-inflates the floor) +
  more runtime → pulls against "~runtime-free" (feature 5) and part of feature 3.
- [x] **(iii) SOUNDNESS bug FIXED:** iterative worklist release + iterative collector
  traversals (cwork/bwork/fwork). N=200k crash → runs, 0 live.
- [ ] **(iv) field-/interprocedural bounds elision** for `out[k]` (length of a field
  array) → closes part of the residual 1.5× toward ~1.1×.
- [ ] **(v) overflow default + `+%` culture** (vectorization, M0 report) and
  **analysis caching** (compile time).
- [ ] **(vi) M0.1c contention:** measure real multithread contention (feature-1 number).

**Core risk confirmed red, but the path is surveyed:** ~1.1–1.5× is reachable, but
needs the ownership module (ii). Front-end (P1+) was deferred until (ii)+(iii); the
rest of the pipeline is now built (see current state above).

---

## Compiler pipeline (front-end new, rest reused)

### P1 — lexer + parser → AST → plan: [language/PARSER.md](language/PARSER.md)
- [x] Lexer (token kinds, Unicode idents, numbers/strings/interpolation, comments).
- [x] Recursive-descent parser + Pratt expression parser (precedence table).
- [x] AST definitions.
- [~] Error recovery (panic mode at `}`/`\n`; multiple errors per run) — basic.
- [ ] `vire fmt` (roundtrip AST→source) as parser-fuzz insurance.

### P2 — name resolution + type inference + monomorphization
- [x] Name/module resolution (one module = file, one package = directory).
- [x] **Bidirectional HM inference** with local anchors (signatures at fn/module
  boundaries keep errors near — see [EVALUATION.md](language/EVALUATION.md) §5).
- [~] Trait resolution + coherence rules (the *real* risk, not vanilla HM).
- [~] Monomorphization (hooks into the existing inliner approach).
- [~] **Good error messages** (near the cause, fix suggestions) — ergonomics-critical.

### P3 — `comptime` + macro expander (the "preprocessor" layer, features 4/2/3)
- [~] Macro expander (`crates/vire/src/expand.rs`).
- [ ] `comptime` evaluator (interpreter over the AST/type graph; recursion limit).
- [ ] `@typeinfo`/reflection API (feature 3).
- [ ] Hygienic macros (feature 4).
- [ ] `@if`/`@when` conditional compilation (feature 4).

### P4 — lowering AST → `crates/ir` **in SSA**
- [x] Lowering (value types, sum types→tagged union, closures, `match`→`switch`).
- [~] **Iterator-mutation check** ([REFERENCE.md](language/REFERENCE.md) §9a) — local
  non-mutation analysis; not provable → compile error.
- [x] SSA generation (removes the Java path's GVN-vs-slot-reuse fight).
- [x] Solver + backend attached unchanged (devirt/escape/RC/bounds/backend).

### P5 — stdlib + FFI
- [~] Core stdlib (Str, List/Map/Set, iterators, Option/Result) over libc.
- [x] `extern "C"` + `unsafe` boundary.
- [x] C-header→binding generator (feature-5 prerequisite, interop core).

---

## Features 1–8 (each with attachment point + core tasks)

### [1] Multithreading, safe by construction 🟢* *(light + channels/mutex is enough — confirmed)*
Attach: backend `--threads` (atomic RC, pthreads, monitor) — **present**.
- [ ] `Channel[T]`, `spawn`, `Mutex[T]`, `Atomic[T]` in the stdlib.
- [ ] `parallel_map`/`parallel_for` (fork-join).
- [ ] **Send check**: a value passed to `spawn` must be moved/copied *or* a Sync type
  — else compile error. *Conservative* (same analysis as the iterator check §9a; when
  in doubt require mutex/move). **No** total guarantee over arbitrary alias graphs —
  deliberate (EVALUATION §7.1).
- [ ] M0.1 clarifies the atomic-contention cost up front.

### [2] Template programming 🟢
Attach: monomorphization (P2) + `comptime` (P3).
- [ ] Generics `[T: Trait]`, multiple bounds.
- [ ] Value generics `[comptime N: Int]`, fixed arrays `[T; N]`.
- [ ] Monomorphization + static trait resolution → direct calls.

### [3] Compile-time reflection 🟢
Attach: whole-program type graph (P2) + `comptime` (P3).
- [ ] `@typeinfo(T)` (fields/variants/methods/attributes, comptime-iterable).
- [ ] `@derive(Json, Eq, Hash, Ord, …)` via reflection.
- [ ] `comptime for/if/assert`, `emit`. **No** runtime reflection (AOT).

### [4] Own optional preprocessor 🟢 *(= comptime/@if/macros, not C text)*
Attach: P3.
- [ ] Hygienic macros (`macro name(args) { … }`), **hygienic + type-safe**:
  - [ ] **typed parameters** (`cond: expr`, `body: block`, `ident`, `pat`, `type`, or
    a concrete type) → misuse = compile error at the call site.
  - [ ] **full type checking after expansion** (no ill-typed result possible).
  - [ ] hygiene (no name capture), diagnostic spans into the expansion.
- [ ] `@if`/`@when` (conditional compilation, platform switches) — expression-based, checked.
- [ ] `const`/`comptime {}` (compile-time values/codegen), fully type-checked. Docs: not `#define`.

### [5] Build interop, Meson first-class 🟢🟡
Attach: clang→object (present).
- [ ] Stable compiler CLI (`--emit=obj|llvm|asm`, `-O`, `--deps` Ninja `.d`).
- [ ] Meson module `vire` (`vire.executable/static_library`), C-ABI `.o`/`.a`.
- [ ] `vire build` wrapper delegates to Meson; pkg-config deps → binding generator.
- [ ] **Decision:** *adopt* Meson instead of an own build system (saves a subsystem).

### [6] Logger "done right" 🟢
Attach: stdlib + `comptime` (compile-time level filter) + debug info (location).
- [ ] Structured fields, levels, `with log.span(...)`.
- [ ] **Compile-time level filter**: disabled calls = 0 instructions (comptime `if`).
- [ ] Sinks (colored console / JSON / file), chosen at build time.

### [7] Go-style error handling 🟢* *(Go spirit, but `Result` instead of `nil`)*
Attach: value error model (backend present), `?` as lowering.
- [ ] `Result[T,E]`/`Option[T]` + `?` operator (early return).
- [ ] `.wrap(msg)` (context, chain), typed errors + `match`.
- [ ] **No `nil`, no `(T, Error)` tuple** (violates no-null). `panic` only for
  programmer errors.

### [8] Debug symbols + crash paths 🟢
Attach: LLVM debug metadata (backend extension), panic model.
- [ ] Thread line numbers front-end→IR; emit `!DILocation`/`!DISubprogram`.
- [ ] Debug runtime backtrace (`file:line:function`) on panic/bounds/null.
- [ ] Off by default in release (0 overhead), `--release --backtrace` opt-in.
- [ ] freestanding: compact symbol table instead of libc `backtrace`.

---

## Cross-cutting risks (retire early — from EVALUATION §7)
- [x] **Alias precision** (safety *and* speed depend on it) → M0.1 (measured; residual
  addressed by region inference (ii)).
- [~] **Compile time** whole-program+mono+comptime → M0.2 measured; analysis caching open.
- [~] **Inference error locality** → bidirectional anchors + fix suggestions (P2).
- [ ] **Overflow default**: checked also in release, wrapping only explicit
  ([REFERENCE.md](language/REFERENCE.md) §3.1).

## Non-goals (deliberate)
Runtime `eval`/reflection · dynamic loading of unknown code · C-text preprocessor ·
deadlock-freedom guarantee · "all" C++/Rust libraries beyond the C-ABI boundary.
