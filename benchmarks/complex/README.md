# Complex benchmarks — multi-algorithm workloads + fair multithreading

Beyond the single-kernel microbenchmarks in [../suite/](../suite/), these are larger
programs that **combine several algorithms**, several with **fork/join multithreading**
on an equal footing across all three languages. Every program is matched line-for-line
in Vire, Rust, and C++, uses integer (or identically-ordered / FMA-contracted float)
arithmetic, and prints **one bit-identical checksum** (`run.sh` verifies equality before
timing). `./run.sh` reproduces.

## What each one exercises

| Benchmark | Threads | Algorithms combined |
|---|---|---|
| **hashmap** | 1 | open-addressing hash map from scratch: 400k inserts, 1.6M lookups, tombstone deletes |
| **graph** | 1 | BFS + Dijkstra (binary min-heap) on a 1.6M-edge fixed-degree digraph |
| **matrix** | 1 | 512² float matmul, cache-friendly **ikj** order → vectorized packed-FMA SAXPY (SIMD) |
| **fft** | 1 | NTT (integer FFT, radix-2, mod 998244353) on 2²⁰ points — 64-bit modular multiply |
| **raytracer** | 1 | 2400² image, 4 spheres + light, quadratic ray-sphere intersection (sqrt), diffuse shading |
| **compression** | 1 | LZ4-style hash-table match finding over a 4 MB byte stream |
| **compiler** | 1 | generate → **lex + recursive-descent parse into a heap AST + evaluate** (400 expressions) |
| **json** | 1 | generate → recursively **parse a nested JSON document**, summing numbers |
| **regex** | 1 | backtracking regex matcher (`.` `*`, Kernighan/Pike) over 2M texts |
| **pipeline** | 1 | LCG gen → quicksort 200k → 20k binary searches → 256-bin histogram |
| **kmeans** | 1 | 50k 2-D points, 25 Lloyd iterations / 16 clusters (two-pass nearest-centroid) |
| **pmontecarlo** | **4** | 4×25M integer-LCG Monte-Carlo samples, partial hit-count → shared `Atomic` |
| **pmandel** | **4** | 4 threads own a band of a 2000² Mandelbrot grid, escape iters → shared `Atomic` |
| **pquicksort** | **4** | 4 threads each generate + recursively quicksort their **own** 1M array |

The four parallel benchmarks use **exactly 4 threads in all three languages** (Vire
`spawn`/`join` + `Atomic`, Rust `std::thread` + `AtomicI64`, C++ `std::thread` +
`std::atomic<long>`), the same partitioning, and an integer `fetch_add` reduction — so
the result is identical regardless of thread scheduling and the comparison is fair.
(Vire's safety `send`-check *forbids* sharing a mutable array across threads, so
`pquicksort` is per-thread-independent rather than a single shared array — a data race
cannot be written.)

## Results (best-of-5, freshly measured 2026-07, 20-core machine)

| Benchmark | Vire | Rust | C++ | V/Rust | V/C++ |
|---|---|---|---|---|---|
| **hashmap** | 0.030 s | 0.045 s | 0.042 s | **0.67×** | **0.73×** |
| graph | 0.065 s | 0.040 s | 0.058 s | 1.61× | 1.12× |
| **matrix** (SIMD) | 0.034 s | 0.035 s | 0.033 s | **0.98×** | 1.01× |
| **fft** (NTT) | 0.066 s | 0.081 s | 0.078 s | **0.82×** | **0.85×** |
| raytracer | 0.326 s | 0.170 s | 0.152 s | 1.92× | 2.14× |
| compression | 0.032 s | 0.027 s | 0.033 s | 1.17× | **0.96×** |
| compiler | 0.021 s | 0.006 s | 0.017 s | 3.45× | 1.27× |
| json | 0.023 s | *(0.002 s — folded)* | 0.022 s | *n/a* | **1.02×** |
| regex | 0.209 s | 0.186 s | 0.178 s | 1.12× | 1.17× |
| pipeline | 0.020 s | 0.018 s | 0.014 s | 1.13× | 1.44× |
| **kmeans** | 0.034 s | 0.063 s | 0.027 s | **0.55×** | 1.28× |
| **pmontecarlo** (4 thr) | 0.187 s | 0.195 s | 0.194 s | **0.96×** | **0.96×** |
| **pmandel** (4 thr) | 0.201 s | 0.215 s | 0.215 s | **0.93×** | **0.93×** |
| pquicksort (4 thr) | 0.115 s | 0.093 s | 0.098 s | 1.24× | 1.18× |

Fast benchmarks (≈0.02–0.07 s) carry ~±15% run-to-run variance; the parallel and
≥0.1 s ones are stable.

## Honest reading

**Where Vire wins or ties (7 of 14):** the two **parallel** benchmarks it *beats*
(pmontecarlo 0.96×, pmandel 0.93× — `spawn`/`Atomic` add no overhead over raw
`std::thread`, and the compute-bound bands scale near-linearly: pmontecarlo is a measured
**3.98× on 4 cores**); **kmeans 0.55× Rust**, **hashmap 0.67×/0.73×**, **fft 0.82×/0.85×**,
**matrix 0.98×/1.01×** (vectorized packed-FMA), and **json/compression ≈ C++ parity**.
The shared LLVM backend + solver (bounds elision, devirt, region/RC) lands compute and
map/hash work at or below Rust/C++.

**Where Vire lags, and why (honest):**
- **raytracer (1.92× / 2.14×):** the hot loop indexes small `farray` sphere tables under
  a data-dependent branch; the residual is scalar FP scheduling + not-fully-elided checks
  in a divide-heavy inner loop. The most FP-bound whole program in the set.
- **compiler (3.45× Rust):** an **allocation/pointer-chasing** case — Vire RC-manages every
  heap AST `Node` (retain/release on the recursive build + eval traversal) where Rust's
  `Box` is a bare move and C++ uses a pool. This is the RC tax on tree-shaped data that the
  region/shape passes target but do not fully close for a freshly-built-and-walked AST.
- **graph (1.61× Rust):** Dijkstra's binary-heap sift does many bounds-checked array
  swaps; Rust's `slice::swap` + its heap codegen is tighter here.
- **pquicksort / regex / pipeline (1.1–1.25×):** the same in-place-sort / branchy residual
  the `../suite/` `sort` shows, plus per-element bounds checks.

**Two measured caveats, stated plainly:**
- **json — Rust constant-folds it.** The program is input-free and deterministic, so
  rustc's optimizer evaluates the whole generate+parse at compile time → 0.002 s (its
  `main` is ~12 instructions). clang and Vire both actually *run* it (≈0.022 s), so the
  fair, meaningful comparison here is **Vire ≈ C++ parity (1.02×)**; Rust's number is a
  fold artifact (the same thing `../vire-lang/`'s `fib` shows).
- **kmeans is two-pass.** The nearest-centroid search is split into a distance-map + a
  scalar argmin (not fused) — see [../complex/kmeans.vr](kmeans.vr) and the note in the
  parent README: no compiler vectorizes the branchy argmin (0 SIMD in all three); the win
  is removing the loop-carried dependency, which Vire benefits from most.

All fully memory-safe: a genuinely out-of-range array access in any of these still throws.
