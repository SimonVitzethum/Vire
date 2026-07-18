# Vire vs Rust vs C++ — benchmarks (incl. official CLBG programs)

Matched programs, each optimized (`vire build` = -O2 -flto -march=native;
`rustc -O`; `clang++ -O2 -march=native`), best-of-3, outputs checked
**bit-identical**. `./run.sh` reproduces.

## Results (one machine, best-of-3)
| Bench | Kind | Vire | Rust | C++ | V/Rust | V/C++ |
|---|---|---|---|---|---|---|
| arith | compute loop | 0.905 s | 0.892 s | 0.901 s | **1.02×** | **1.00×** |
| fib | recursion | 0.076 s | 0.084 s | 0.074 s | **0.91×** | 1.03× |
| struct | stack struct | 0.307 s | 0.291 s | 0.307 s | 1.05× | **1.00×** |
| **mandelbrot** | CLBG, float compute | 0.137 s | 0.140 s | 0.118 s | **0.99×** | 1.17× |
| **binary-trees** | CLBG, alloc/GC | 0.477 s | 0.180 s | 0.139 s | **2.65×** | 3.43× |
| **nsieve** (i64-matched) | CLBG, array | 0.340 s | 0.334 s | 0.363 s | **1.02×** | **0.94×** |

## Reading
**Compute-bound = parity.** Scalar arithmetic, recursion, stack structs, and the
CLBG classic mandelbrot run at C++/Rust level (0.99–1.05× Rust). This is the
payoff of the shared LLVM backend + solver (bounds elision/inlining/escape/
devirt) + closed-world `-march=native`. C++ pulls ahead by 1.17× on mandelbrot (better
autovectorization of the inner loop).

**Allocation/GC-bound = the honest gap.** binary-trees (pure object
allocation + freeing) is **2.65× Rust / 3.43× C++** — the reference-counting tax:
retain/release per node + cascading free, against Rust's ownership (no refcount) and
C++ new/delete. **0 live** (Vire frees everything — C++ only with an explicit `delete`).
Consistent with the shared/cyclic PageRank (`../vire-m0/`, ~2–4×). No
O(n²) blowup; region-inference v1 (`language/M0.3`) has already lowered the RC share,
the oracle ceiling is parity (`--no-rc`). This gap is closed by interproc. region
inference (the open hard half) — the compute paths are already there.

## Summary
Vire = **C++/Rust parity on compute-bound code, ~2.7–3.4× on pure
object allocation** (the RC tax, with a proven ceiling at parity and without O(n²)).
