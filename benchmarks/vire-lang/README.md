# Vire vs Rust vs C++ — benchmarks (incl. official CLBG programs)

Matched programs, each optimized (`vire build` = -O2 -flto -march=native;
`rustc -O`; `clang++ -O2 -march=native`), best-of-3, outputs checked
**bit-identical**. `./run.sh` reproduces.

## Results (one machine, best-of-3, measured 2026-07)
| Bench | Kind | Vire | Rust | C++ | V/Rust | V/C++ |
|---|---|---|---|---|---|---|
| arith | compute loop | 0.962 s | 0.952 s | 0.959 s | **1.01×** | **1.00×** |
| fib | recursion (fib 38) | 0.002 s | 0.087 s | 0.081 s | **0.02×** | **0.02×** |
| struct | stack struct | 0.290 s | 0.309 s | 0.331 s | **0.94×** | **0.88×** |
| **mandelbrot** | CLBG, float compute | 0.127 s | 0.152 s | 0.127 s | **0.84×** | **1.00×** |
| **binary-trees** | CLBG, alloc/GC | 0.190 s | 0.186 s | 0.154 s | **1.02×** | 1.23× |
| **nsieve** (i64-matched) | CLBG, array | 0.393 s | 0.367 s | 0.388 s | **1.07×** | **1.01×** |

## Reading
**Compute-bound = parity or better.** Scalar arithmetic, stack structs, nsieve, and
the CLBG classic mandelbrot run at C++/Rust level — mandelbrot now **beats Rust
(0.84×) and matches C++**. This is the payoff of the shared LLVM backend + solver
(bounds elision/inlining/escape/devirt) + closed-world `-march=native`.

**fib(38) — a closed-world win.** Vire's whole-program recursive inliner + LLVM
constant-propagate the fixed argument through the pure recursion and fold `fib(38)` to
a compile-time constant → ~instant (0.002 s) vs 0.08 s for `rustc -O` / `clang -O2`,
which keep the recursion. Real, but on a constant-input microbenchmark — not a claim
about general recursion throughput.

**Allocation/GC-bound = now at Rust parity.** binary-trees (pure object allocation +
freeing) is **1.02× Rust / 1.23× C++** — down from ~2.65× two snapshots ago. Region
inference (`language/M0.3`) removed the bulk of the RC tax, and **move-on-last-use**
(this round) removed the ownership-transfer churn: an owned ref whose sole use is a
field store or `return` hands its +1 to the store (no retain, no cleanup release) —
exactly Rust's move. Construction retains now drop to **zero**; the residual is the
free cascade (per-node release), which Rust also pays. Vire still frees **everything**
(0 live; C++ only with explicit `delete`), no O(n²) blowup. The `--no-rc` oracle
(0.084 s = 0.46× Rust, but *leaks*) shows the remaining lever is **interprocedural
region inference** (bulk-free the whole tree en bloc), which would beat Rust's
per-node free — the open hard half.

## Summary
Vire = **C++/Rust parity (or better) on compute-bound code**, **~1.05× Rust on pure
object allocation** (binary-trees, after region inference — was ~2.7×), and a
closed-world constant-fold win on fixed-input recursion.
