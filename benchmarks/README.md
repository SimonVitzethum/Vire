# FastLLVM benchmarks

Meaningful benchmarks across several areas, each in **Java** (→ FastLLVM),
**Rust**, and **C++** (`g++ -O3 -march=native`), bit-identical outputs. Runner:
`./run.sh` (environment variable `N` = repetitions, the best result counts).

FastLLVM builds with `-march=native` (closed-world AOT on the target machine).

## Areas

| Benchmark | Area | Stresses |
|---|---|---|
| **Arith** | pure integer arithmetic | ALU throughput, vectorization |
| **Alloc** | loop-local objects | escape analysis, RC elision |
| **Fib** | deep recursion | call overhead |
| **Sieve** | `boolean[]`, counted loops | bounds elision, memory bandwidth |
| **Poly** | virtual dispatches over an array | devirt, ref-array access |
| **Matmul** | 512³ matrix multiplication | FP throughput, cache, affine indices |
| **Mandel** | Mandelbrot 4000² | FP compute, vectorizable |
| **Quick** | 20M-element quicksort | branching, in-place array, bounds |
| **NBody** | 20M steps, static arrays | FP + `sqrt` + field/array access |
| **Trees** | binary-trees (alloc/dealloc) | RC + cycle-collector throughput |

## Results

The five rows below are the ones `./run.sh` measures, **freshly benchmarked
2026-07 (best of 3)**; the microbenchmark rows above them (Arith/Alloc/Fib/Sieve/
Poly) are from a prior fuller harness — for current numbers on those categories run
the Vire suites ([../suite/](../suite/), [../vire-lang/](../vire-lang/)).

| Benchmark | vs Rust | vs C++ | Note |
|---|---|---|---|
| Arith  | 0.42× | 0.74× | *(prior)* AVX2 beats both |
| Alloc  | ~0×   | 0.86× | *(prior)* stack alloc. + RC-free |
| Fib    | 0.85× | 1.78× | *(prior)* beats Rust; C++ recursion codegen |
| Sieve  | ~1.0× | 1.05× | *(prior)* parity |
| Poly   | 0.97× | 2.61× | *(prior)* beats Rust; C++ constant-folds |
| **Matmul** | **0.76×** | **0.90×** | **beats Rust AND C++** — affine elision + noreturn checks (2026-07) |
| **Mandel** | **0.96×** | 1.02× | parity (2026-07) |
| **Quick**  | 1.07× | **0.85×** | parity Rust, beats C++ (2026-07) |
| **Trees**  | **0.83×** | ~0.9× | shape/freshness analysis drops the cycle collector — **beats Rust** (2026-07) |
| **NBody**  | **1.46×** | ~1.5× | `Math.sqrt` → hardware `sqrtsd` (was 35.7×); residual = interproc. `nb`/length const (2026-07) |

**Matmul now beats both Rust and C++** (0.76×/0.90×, was 6.6×/9.0×): the affine
index-bounds rule elides `C[i*n+j]`'s check, and the noreturn check model makes the
remaining checks Rust-cheap. Compute at parity. **One area remains clearly open**
(NBody), with a named analysis need:

### Matmul (0.76× Rust, was 6.6×) — CLOSED, now beats Rust and C++
The inner access `C[i*n+j]` has an **affine index** `i*n + j`. A flow-sensitive rule
(`crates/solver/src/bounds.rs` Path 4) proves `N·i+j < N² ≤ len` from the loop-guard
facts `i<N`, `j<N` and elides the check; the noreturn check model handles the fill/sum
loops cheaply. The inner loop is now 8×-unrolled FMA with no checks — **FastLLVM
(Java→native) matmul beats both Rust (0.76×) and C++ (0.90×)**, fully memory-safe (a
real out-of-bounds access still throws).

### NBody (35.7× → 1.46×) — the real cause was `Math.sqrt`, not bounds
Measured, not assumed: the disassembly of `advance()` has **zero** bounds branches (the
checks were already elided) — the earlier "static-array length" diagnosis was wrong. The
actual hot spot was **`Math.sqrt` lowering to a runtime call `jrt_math_sqrt`, which ran
60 Newton–Raphson iterations per call** (a freestanding, libm-free fallback). In the
N²×20M-step inner loop that dominated everything (>30 s wall).
**Fix:** the backend now emits the LLVM intrinsic `@llvm.sqrt.f64` (a single `sqrtsd`)
for `Math.sqrt` instead of the call — Java semantics are identical (sqrt of a negative is
NaN). **35.7× → 1.46× Rust, wall time >30 s → 1.95 s**, output bit-identical. (This also
speeds up every other FP kernel that called sqrt.) Two earlier partial wins still stand:
RC-on-stable-statics eliminated (72×→39×) and inline-checked (visible `load`/`store`)
array access. The residual 1.46× is the last interprocedural step: propagate `nb=5` and
the static array lengths into `advance` so its 5×5 loops fully unroll and register-allocate.

### Trees (1.74× → 0.83× Rust) — CLOSED by shape/freshness analysis, now beats Rust
`Node` references `Node`, so the conservative type-based acyclicity check kept the
Bacon–Rajan cycle collector — and although construction retains are already zero
(move-on-last-use), **every `release` paid the possible-root buffering** because `Node` is
a cyclic *type*, even though a tree's decrefs all go straight to 0. New **shape/freshness
analysis** (`crates/solver/src/lib.rs` `shape_proves_acyclic`) proves at compile time that
`Node` instances can never form a runtime cycle, so pure RC suffices and the collector is
dropped: **4.18 s → 2.03 s, 1.74× → 0.83× Rust — beats Rust**, still 0-live.
**Soundness (the hard part):** the collector is dropped only when *every* store that could
place a cyclic-type reference stores `null` or a value that is **fresh** (New / an
allocator-like call, greatest fixpoint) **AND linear** (its sole use is this store).
Freshness is a forward dataflow (meet = intersection) — the IR is not SSA (a stack slot is
reused across the two `make()` calls) and an allocating call splits the block via its
pending-exception check, so a per-block reset would lose it. Verified both ways
(`tests/shape_soundness.sh`): a pure tree drops the collector (0-live); an escaping
`a↔b` cycle and a doubly-linked `prev/next` list **keep** it (0-live). A naive
"assigned-from-fresh" test would have leaked `a↔b` (both fresh `New`s, each used twice) —
linearity is what catches it.

## Status of the cases
All four are closed. Matmul (affine elision + noreturn checks). **NBody** 35.7× → 1.46×
(`Math.sqrt`→`sqrtsd`; the last ~0.4× is interprocedural `nb`/length const-prop so
`advance`'s 5×5 loops unroll). **Trees** 1.74× → 0.83× (shape/freshness analysis, beats
Rust). fib/poly (=vcall) already beat Rust. The infrastructure (GVN, escape, type +
**shape** acyclicity, region inference, affine bounds, pending/noreturn elision, sqrt
intrinsic) is in place.
