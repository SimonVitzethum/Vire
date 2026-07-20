# FastLLVM benchmarks

Meaningful benchmarks across several areas, each in **Java** (→ FastLLVM),
**Rust**, and **C++** (`g++ -O3 -march=native`), bit-identical outputs. Runner:
`./run.sh` (environment variable `N` = repetitions, the best result counts).

FastLLVM builds with `-march=native` (closed-world AOT on the target machine).

Sibling suites: the single-kernel Vire↔Rust↔clang suites in [suite/](suite/) and
[vire-lang/](vire-lang/), and **[complex/](complex/)** — multi-algorithm workloads
(pipeline, k-means) and **fair fork/join multithreading** (parallel Monte-Carlo /
Mandelbrot, 4 threads in every language).

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

The five **bold** rows are the ones `./run.sh` measures, **freshly benchmarked 2026-07
(best of 3–5, output bit-verified, `rustc -O -C target-cpu=native` / `clang++ -O2
-march=native`)**; the microbenchmark rows above them (Arith/Alloc/Fib/Sieve/Poly) are
from a prior fuller harness — for current numbers on those categories run the Vire suites
([../suite/](../suite/), [../vire-lang/](../vire-lang/)).

| Benchmark | vs Rust | vs C++ | Note |
|---|---|---|---|
| Arith  | 0.42× | 0.74× | *(prior)* AVX2 beats both |
| Alloc  | ~0×   | 0.86× | *(prior)* stack alloc. + RC-free |
| Fib    | 0.85× | 1.78× | *(prior)* beats Rust; C++ recursion codegen |
| Sieve  | ~1.0× | 1.05× | *(prior)* parity |
| Poly   | 0.97× | 2.61× | *(prior)* beats Rust; C++ constant-folds |
| **Matmul** (512³) | **1.01×** | **1.01×** | parity both — ikj vectorizes (2026-07) |
| **Mandel** | **1.00×** | **0.93×** | parity Rust, beats C++ (2026-07) |
| **Quick**  | 1.12× | **1.00×** | parity C++; sort's stack structure (2026-07) |
| **Trees**  | **0.81×** | **0.86×** | **beats both** — shape/freshness analysis drops the cycle collector (2026-07) |
| **NBody**  | 1.16× | 1.31× | `Math.sqrt` → hardware `sqrtsd` (was **35.7×**); residual = SoA aliasing (2026-07) |

Two big moves this round: **NBody 35.7× → 1.16×** (the real cause was `Math.sqrt`, a
60-iteration Newton call, not bounds — now the `sqrtsd` intrinsic) and **Trees 1.73× →
0.81×, now beating Rust** (shape/freshness analysis drops the cycle collector for
provably tree-shaped types). Matmul/Mandel/Quick sit at Rust/C++ parity. **NBody is the
one case still >1.1×**, with a named need:

### Matmul (1.01× Rust / 1.01× C++, was 6.6×) — CLOSED, at parity
The inner access `C[i*n+j]` has an **affine index** `i*n + j`. A flow-sensitive rule
(`crates/solver/src/bounds.rs` Path 4) proves `N·i+j < N² ≤ len` from the loop-guard
facts `i<N`, `j<N` and elides the check; the noreturn check model handles the fill/sum
loops cheaply. The inner loop is now checks-free FMA and, in ikj order, vectorizes —
**Java→native matmul runs at Rust/C++ parity (1.01×)**, fully memory-safe (a real
out-of-bounds access still throws). *(The `../suite/` 256³ ikj matmul measures 0.98×
Rust / 0.91× clang.)*

### NBody (35.7× → 1.16×) — the real cause was `Math.sqrt`; residual is SoA aliasing
Measured, not assumed: the disassembly of `advance()` has **zero** bounds branches (the
checks were already elided) — the earlier "static-array length" diagnosis was wrong. The
actual hot spot was **`Math.sqrt` lowering to a runtime call `jrt_math_sqrt`, which ran
60 Newton–Raphson iterations per call** (a freestanding, libm-free fallback). In the
N²×20M-step inner loop that dominated everything (>30 s wall).
**Fix:** the backend now emits the LLVM intrinsic `@llvm.sqrt.f64` (a single `sqrtsd`)
for `Math.sqrt` instead of the call — Java semantics are identical (sqrt of a negative is
NaN). **35.7× → 1.16× Rust, wall time >30 s → 1.9 s**, output bit-identical. (This also
speeds up every other FP kernel that called sqrt.)

**What it would take to *win* (measured):** the residual ~1.16× is **aliasing**, not
bounds and not `nb`/length const-prop. The bodies are static SoA arrays `x,y,z,vx,vy,vz,
mass` — seven separate `double[]`; to LLVM they are same-typed globals that *may* alias,
so after `vx[i] -= …; vy[i] -= …` it must reload from memory (the inner body is 18
`movsd` to 12 FP ops). Rust's `advance(&mut [f64], …)` gets `noalias` for free and keeps
the accumulators in registers. The fix is **`noalias`/`alias.scope` metadata proving the
distinct static array allocations are disjoint** (they are — separate `new double[k]`), i.e.
a disjoint-allocation alias analysis feeding the SoA loads/stores. Note: **inlining
`advance` into the 20M loop makes it *worse* (7.5×)** — the aliasing blows up register
allocation in the giant loop body — so the call boundary is not the problem.

### Trees (1.73× → 0.81× Rust / 0.86× C++) — CLOSED by shape/freshness analysis, beats both
`Node` references `Node`, so the conservative type-based acyclicity check kept the
Bacon–Rajan cycle collector — and although construction retains are already zero
(move-on-last-use), **every `release` paid the possible-root buffering** because `Node` is
a cyclic *type*, even though a tree's decrefs all go straight to 0. New **shape/freshness
analysis** (`crates/solver/src/lib.rs` `shape_proves_acyclic`) proves at compile time that
`Node` instances can never form a runtime cycle, so pure RC suffices and the collector is
dropped: **4.0 s → 2.0 s, 1.73× → 0.81× Rust / 0.86× C++ — beats both**, still 0-live.
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
Matmul/Mandel/Quick at Rust/C++ parity. **Trees** 1.73× → 0.81× (shape/freshness analysis
drops the collector — beats Rust). **NBody** 35.7× → 1.16× (`Math.sqrt`→`sqrtsd`); the
remaining >1.1× is the one open item and it is **SoA aliasing** — winning it needs
`noalias` metadata for the disjoint static arrays (see above), not more const-prop.
fib/poly(=vcall) already beat Rust. The infrastructure (GVN, escape, type + **shape**
acyclicity, region inference, affine bounds, pending/noreturn elision, sqrt intrinsic) is
in place.
