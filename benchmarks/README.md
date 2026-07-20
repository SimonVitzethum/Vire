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
| **Trees**  | 1.73× | 1.80× | region inference (3.2×→1.7×); RC tax residual (2026-07) |
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

### Trees (1.68×, was 3.2×) — mostly closed by region inference
`Node` references `Node` → the type-reference graph is cyclic → the
(conservative, type-based) acyclicity analysis kept the cycle collector, which
buffers candidates per decref. **Region inference (`language/M0.3`) has since
removed most of this tax — 3.2×→1.68× C++.** The residual is the last RC/collector
bookkeeping on the tree nodes the region pass does not yet prove tree-shaped
(acyclic); a full **structure/shape analysis** would drop the collector entirely
(as it already does for type-acyclic programs) and reach the RC-lean ceiling.

## Common denominator of the remaining case
Matmul is now closed (affine elision + noreturn checks); Trees is largely closed by
region inference (shape analysis is the last step). **NBody** alone remains clearly
open — it needs interprocedural static-array-length + loop-bound constants so its
checks elide. The *infrastructure* (GVN, escape, acyclicity, region inference, affine
bounds, pending/noreturn elision) is in place — a targeted extension, not a new build.
