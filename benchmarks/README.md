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
| **Mandel** | **0.97×** | 1.02× | parity (2026-07) |
| **Quick**  | 1.05× | **0.83×** | parity Rust, beats C++ (2026-07) |
| **Trees**  | 1.68× | 1.77× | **improved** 3.2×→1.68× via region inference (2026-07) |
| **Matmul** | 2.1×  | 2.4×  | **improved** 6.6×→2.1×; open — affine index bounds (2026-07) |
| **NBody**  | 35.7× | 36.4× | **open** — interproc. static-array length (2026-07) |

**Compute at parity; Trees now within 1.8× C++.** Two areas remain clearly open,
each with a named analysis need:

### Matmul (2.1×, was 6.6×) — affine index-bounds elision
The inner access `C[i*n+j]` has an **affine index** `i*n + j`. Today's
GVN bounds elision proves counted loops (`arr[i]`, `i < len`) and
and-masks, but not `i*n + j < n*n`. Needed: a flow-sensitive **upper-bound
analysis** (interval, upper bounds only) that derives from the guards `i<n`,
`j<n` and `len=n²` the bound `(n-1)·n + (n-1) < n²` and propagates over
`Mul`/`Add`. Only then are the accesses throw-free → the
pending checks drop out → LLVM vectorizes the FMA loop (like Rust/C++).
As long as the check stays, the pending check blocks vectorization.

### NBody (35.7×) — interprocedural/static array length
The arrays are **static fields**, created in `main`, used in `advance()`.
Two partial fixes already took effect:
- **RC-on-stable-statics eliminated** (72×→39×→35.7×): a static field that a
  function + its callees do not write is constant during their execution →
  `GetStatic` yields a stable reference held by the static root and
  needs no retain/release (previously 66 RC ops per `advance`).
- **Inline-checked array access**: accesses are now visible `load`/`store`
  (hoistable) instead of opaque `jrt_daload` calls.
What remains: the **length** of the static arrays is unknown in `advance` (no
`NewArray` there) → bounds not elidable → the pending checks stay. Needed:
track static array lengths whole-program (`static T[] f = new T[k]` ⇒ length
`k`) **plus** the loop bound `nb` as an interprocedural constant.

### Trees (1.68×, was 3.2×) — mostly closed by region inference
`Node` references `Node` → the type-reference graph is cyclic → the
(conservative, type-based) acyclicity analysis kept the cycle collector, which
buffers candidates per decref. **Region inference (`language/M0.3`) has since
removed most of this tax — 3.2×→1.68× C++.** The residual is the last RC/collector
bookkeeping on the tree nodes the region pass does not yet prove tree-shaped
(acyclic); a full **structure/shape analysis** would drop the collector entirely
(as it already does for type-acyclic programs) and reach the RC-lean ceiling.

## Common denominator of the open cases
Matmul and NBody need **stronger static proofs** (affine intervals,
interprocedural constants/lengths) so the safety checks drop out; Trees is now
largely closed by region inference, with a shape analysis as the last step. The
*infrastructure* (GVN, escape, acyclicity, region inference, pending elision) is in
place — these are targeted extensions, not new builds.
