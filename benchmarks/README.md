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

## Results (as of this session, best of 3–7, vs Rust / vs C++)

| Benchmark | vs Rust | vs C++ | Note |
|---|---|---|---|
| Arith  | **0.42×** | **0.74×** | AVX2 beats both |
| Alloc  | **~0×**   | **0.86×** | stack alloc. + RC-free |
| Fib    | **0.85×** | 1.78× | beats Rust; C++ recursion codegen |
| Sieve  | **~1.0×** | **1.05×** | parity |
| Poly   | **0.97×** | 2.61× | beats Rust; C++ constant-folds |
| Mandel | **1.00×** | 1.06× | parity |
| Quick  | **1.03×** | **0.82×** | parity Rust, beats C++ |
| Matmul | 6.6×  | 9.0× | **open** — affine index bounds |
| NBody  | 39×   | 40× | **open** — interproc. array length |
| Trees  | 3.2×  | 3.6× | **open** — cycle collector on a tree |

**7 of 10 at/above Rust parity.** Three open areas, all with a clearly
named, substantial analysis need:

### Matmul (6.6×) — affine index-bounds elision
The inner access `C[i*n+j]` has an **affine index** `i*n + j`. Today's
GVN bounds elision proves counted loops (`arr[i]`, `i < len`) and
and-masks, but not `i*n + j < n*n`. Needed: a flow-sensitive **upper-bound
analysis** (interval, upper bounds only) that derives from the guards `i<n`,
`j<n` and `len=n²` the bound `(n-1)·n + (n-1) < n²` and propagates over
`Mul`/`Add`. Only then are the accesses throw-free → the
pending checks drop out → LLVM vectorizes the FMA loop (like Rust/C++).
As long as the check stays, the pending check blocks vectorization.

### NBody (39×) — interprocedural/static array length
The arrays are **static fields**, created in `main`, used in `advance()`.
Two partial fixes this session already took effect:
- **RC-on-stable-statics eliminated** (72×→39×): a static field that a
  function + its callees do not write is constant during their execution →
  `GetStatic` yields a stable reference held by the static root and
  needs no retain/release (previously 66 RC ops per `advance`).
- **Inline-checked array access**: accesses are now visible `load`/`store`
  (hoistable) instead of opaque `jrt_daload` calls.
What remains: the **length** of the static arrays is unknown in `advance` (no
`NewArray` there) → bounds not elidable → the pending checks stay. Needed:
track static array lengths whole-program (`static T[] f = new T[k]` ⇒ length
`k`) **plus** the loop bound `nb` as an interprocedural constant.

### Trees (3.2×) — cycle collector on acyclic trees
`Node` references `Node` → the type-reference graph is cyclic → the
(conservative, type-based) acyclicity analysis keeps the cycle collector, which
buffers candidates per decref. The tree is really acyclic. Needed: a
**structure/shape analysis** (or region/ownership inference) that proves tree-shaped
allocation patterns acyclic — then the collector drops out (as it already does
today for type-acyclic programs) and the allocation runs RC-lean.

## Common denominator of the open cases
All three need **stronger static proofs** (affine intervals,
interprocedural constants/lengths, shape analysis) so that safety checks
and RC bookkeeping drop out. The *infrastructure* for this (GVN, escape, acyclicity,
pending elision) is in place; these are targeted extensions, not new builds.
