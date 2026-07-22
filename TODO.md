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

- [ ] **Automatic interprocedural region/arena inference for short-lived heap
  graphs.** binary-trees is at 0.91× Rust; the `--no-rc` oracle is **0.46× Rust but
  leaks** — that gap is the allocator (per-node malloc/free cascade), not RC
  (move-on-last-use already zeroed construction retains). Capture it *soundly*:
  auto-fire the `capsule`-arena mechanism where escape analysis proves a whole
  subgraph dies at a known point (build→consume→drop), **without** the user writing
  `capsule`, and free the subgraph en bloc.
  - Extends a proven mechanism: thread-local `arena_top`, `while_arena_safe`
    interprocedural escape check, `tests/vire_interproc_arena.sh`, 0-live oracle all
    already exist — this generalizes the trigger from explicit `capsule` to inferred.
  - **Also lower arena fixed costs** (larger chunks, chunk recycling across
    regions) — M0.2 measured the arena *loses* below ~2.5M nodes purely on
    chunk-alloc/zeroing fixed costs; fixing that widens the win to smaller N.
  - **Soundness-critical** (a wrong escape verdict = use-after-free): pin promote
    *and* decline in both directions with new cases in
    `tests/vire_interproc_arena.sh` before enabling by default.
  - *Effort ~4–6 wk. This is the one place with real structural headroom.*

## Tier 2 — targeted, safe, medium ROI

- [ ] **NBody SoA `noalias`/`restrict` on disjoint static arrays.** NBody is the
  single remaining >1.1× compute case (**1.16× Rust / 1.31× C++**): seven same-typed
  `double[]` globals LLVM can't prove disjoint → reloads. Unlike the ruled-out
  per-access case (latency-bound), these are *statically distinct allocations* →
  provably safe to mark. Target parity. (Note: inlining `advance` makes it *worse*,
  7.5× — do not.)
- [ ] **(M0.3-iv) Field-/interprocedural bounds elision** for `out[k]` (length of a
  field array). Extends the mature `crates/solver/src/bounds.rs`. **Soundness risk
  ~zero** — elision only removes a check when provably safe; a real OOB still throws.
  Closes residual toward ~1.1×.
- [ ] **PGO on graph (Dijkstra).** Infra (`--pgo-gen`/`--pgo-use`) is already built
  but never applied to the data-dependent heap-sift branches. **Zero correctness
  risk**, cheap experiment (regular branches saw ~0%; branchy pointer-chasing may
  differ). graph is 1.64× Rust / 55 vs 30 MB RAM — also find which arrays are fully
  touched (cache pressure).

## Tier 3 — enablers with broad latent effect

- [ ] **(M0.3-v) Overflow default + `+%` culture.** Checked-overflow currently blocks
  vectorization of integer reductions; an explicit wrapping `+%` operator lets hot
  loops vectorize — **opt-in, checked stays the default** (safe). Broad latent gain
  for integer-array code. (Pairs with the overflow-in-release item under
  Cross-cutting.)
- [ ] **Explicit SIMD intrinsic path** for reductions LLVM won't auto-vectorize
  (e.g. vectorized argmin — kmeans nearest-centroid is 0.55× Rust / **1.28× C++**;
  no compiler emits SIMD for the branchy argmin). Emit `@llvm.vector.reduce.*` /
  explicit `<N x i64>` ops, or a comptime SIMD library. Opens a general capability,
  not just one bench.

## Perf — residual / parked (low ROI, keep for context)

- [ ] **Codegen scheduling / register allocation** on the FP losers (raytracer 1.9×,
  regex/pquicksort/pipeline 1.1–1.25×). Verified *not* IR quality (same program
  through `opt -O2` matches clang) — it's the LLVM **backend** reacting to subtle IR
  structure (~2× the stack spills of clang on the raytracer inner loop). Deep-codegen
  tuning, not a single fixable pass. **Parked — low ROI vs the wins already banked.**
- [ ] **sort 1.05× / pquicksort 1.23×** residual — the explicit-stack structure (a
  recursive `Array`-param version measured *slower*). Marginal.
- [ ] **Expand the differential fuzzer** (`tests/fuzz_gen.py`) — floats
  (fp-contract-matched), nested control-flow, break/continue, strings. (Correctness
  insurance, not perf, but belongs with the perf work.)
- [ ] **Analysis caching / incremental compile** — compile time measured super-linear
  ~O(n^1.4); orthogonal to runtime perf but the main compile-*speed* lever left.

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

- [ ] **`vire fmt`** (roundtrip AST→source) as parser-fuzz insurance.
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

## Stdlib + FFI

- [~] **Collections breadth.** `list()`/`map()`/`set()`, `Str` methods, and iterator
  adapters (`fold`/`sum`/`count`/`map`/`filter`/`each`, statement-bodied lambdas)
  work. `tests/vire_iter.sh`. **Open:** `Str.split` (needs a typed `list[Str]`), and
  the full `Option`/`Result` surface (`.wrap(msg)` context/chain — core `?`/`match`
  works).

---

## Features 1–8 (open parts only)

### [1] Multithreading, safe by construction
- [ ] `Mutex.lock(closure)` (scoped-guard form); `parallel_map`; typed `Channel[T]`
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

### [4] Own optional preprocessor *(= comptime/@if/macros)*
- [ ] Hygienic macros: typed parameters `block`/`pat`, token pasting, diagnostic
  spans into the expansion (typed `expr`/`ident`/`type` + hygiene already done).

### [6] Logger — remaining
- [ ] `with log.span(...)` (scoped context fields).
- [ ] Sinks (colored console / JSON / file), chosen at build time.

### [7] Go-style error handling — remaining
- [ ] `.wrap(msg)` (context, chain), typed errors with attached debug path.

### [8] Debug symbols + crash paths — remaining
- [ ] freestanding: compact symbol table instead of libc `backtrace`; map the entry
  symbol `java_main` back to `main` in the DISubprogram name (cosmetic).

---

## GPU kernels (`@gpu`) — follow-ups

*(The headline GPU perf items are promoted to the Performance Push, Tier 4 above.)*
Remaining non-perf follow-ups tracked there too.

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
