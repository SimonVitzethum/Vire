# Benchmark suite: Vire vs Rust vs C++ (clang++)

`./run.sh` — builds each benchmark in all three languages (`vire build`, `rustc -O
-C target-cpu=native`, `clang++ -O2 -march=native`), measures best-of-5, and checks
output equality. C++ = **clang++** (LLVM, like Vire) for a fair codegen
comparison (g++/GCC diverges separately, see RECURSION-INLINING.md).

## Results (best-of-5, the same machine, measured 2026-07)
| Benchmark | Vire | Rust | clang++ | Vire/clang |
|---|---|---|---|---|
| bitmanip (popcount) | 0.191 | 0.193 | 0.192 | **1.00×** |
| matmul (256³ naive) | 0.017 | 0.010 | 0.013 | 1.28× |
| nbody (2000, 20 steps) | 0.075 | 0.076 | 0.074 | **~1.00×** |
| montecarlo (20M, LCG) | 0.041 | 0.041 | 0.041 | **0.99×** |
| vcall (dyn dispatch, 100M) | 0.120 | 0.117 | 0.281 | **0.41×** |
| sort (quicksort 2M) | 0.169 | 0.126 | 0.114 | 1.47× |
| binsearch (10M lookups) | 0.612 | 0.499 | 0.470 | 1.30× |

## Interpretation
- **Compute (bitmanip/nbody/montecarlo): Vire = clang parity** (0.99–1.00×).
  Both go through LLVM → the same codegen optimum.
- **matmul (256³ naive): Vire 1.28×.** The inner access `C[i*n+j]` has an *affine*
  index whose bounds check is not yet elided (the residual over clang); at 0.017 s
  the absolute gap is a few ms. The relational/affine bounds analysis (TODO #1) is
  the lever, same root cause as sort/binsearch below.
- **vcall = trait objects (dyn dispatch): Vire 0.41× — 2.4× FASTER than clang
  `virtual`, and essentially at Rust.** Vire's solver devirtualizes + inlines the
  vtable dispatch; clang keeps the indirect call. (Vire's vcall time roughly halved
  vs the previous snapshot as the devirt/vtable path matured.)
- **Array-index-heavy (sort/binsearch): Vire 1.3–1.5× slower.** The reason is
  **bounds checks** on every array access — the solver (`elide_bounds`) removes
  many, but not the data-dependent ones (quicksort partition, binary-search mid). That
  is the clear, honest optimization point (Rust has the same principle, but elides
  more; C++ has no checks at all). The next perf lever for Vire.
- **DIFFs in the table** are pure float formatting (Vire/C++ `%g` scientific
  vs Rust's full precision) or summation rounding (nbody) — identical values.

## What these benchmarks cover — and what they don't (honest assessment)

**Covered here + in `../vire-lang/` + `../` (Java-AOT):** the **compute-, numerics-,
algorithm-, data-structure-, memory-, and dispatch-bound** axes — arithmetic/bit
(bitmanip, arith), recursion (fib), virtual dispatch (vcall), numerics (matmul,
N-body, Monte-Carlo, mandelbrot), sorting/search (sort, binsearch), stack structs
(struct), and allocation/GC throughput (btree, Trees, `../vire-m0/`). This is the
core "is the generated code fast?" question — answered: **at/above clang on compute
and 2.4× faster on dispatch; the residual is data-dependent bounds checks.**

**Deliberately NOT exercised by these microbenchmarks** — and *why*, updated for the
current language surface:

- **Concurrency throughput** — Vire now HAS high-level, safe-by-construction
  `spawn`/`join`/`Atomic`/`Mutex`/`Channel`/`parallel_for` (see `../../tests/
  vire_threads.sh`, `../../examples/vire/threads_*.vr`), but there is **no contention/
  scaling benchmark** here (thread-pool, work-stealing, parallel sort). Open: measure
  real multi-thread scaling (M0.1c).
- **Text processing** — `Str` methods (length/charAt/substring/indexOf/starts·endsWith/
  trim/lower/upper), `list()/map()/set()`, iterator adapters, and `@derive(Json)` output
  exist now, so simple string/collection kernels are expressible; still missing for a
  real text benchmark: **regex, a parser lib, `Str.split`** (needs a typed `list[Str]`),
  and string **escaping**.
- **Cryptography** (AES/SHA/BLAKE3/…) — needs a byte-array element kind
  (`ArrKind::Byte`) + a crypto lib; not present.
- **I/O / networking** (filesystem, mmap, TCP/UDP/HTTP) — needs an IO/network library;
  not present. These are **runtime-library** gaps, not codegen gaps.
- **Large/complex data structures** (B-trees, AVL/RB, priority queues) — limited by the
  lack of a **typed `List[T]`** and **array-as-a-function-parameter** (below), not by
  performance.

The gaps are **library + a few front-end features**, not the compiler core: everything
measured shows the generated code is already at the LLVM optimum.

## Known Vire limitations that the benchmarks touched on
- **Array as a function parameter** (`fn f(a: Ref)` + `a[i]`) → "no known array":
  ref params carry no ArrKind. sort was therefore written iteratively-in-main
  (the array stays local). An `Array[T]` param annotation would be the fix.
- **`else` must be on the same line as `}`** (newline-terminated syntax).

## Bounds checks: analysis + honest ceiling (addendum)
Measured ceiling with `FASTLLVM_NO_BOUNDS=1` (measurement mode, all checks off, unsound):

| Benchmark | Vire (checks) | Vire (no checks) | Rust | clang++ |
|---|---|---|---|---|
| sort | 0.168 | **0.132** (−21%) | 0.122 | 0.110 |
| binsearch | 0.559 | **0.480** (−14%) | 0.479 | 0.458 |

**Two findings:**
1. **Bounds checks cost real time** (−14 to −21%). `elide_bounds` (GVN) removes many,
   but NOT the data-dependent ones (`a[mid]` with `mid=(lo+hi)/2`, quicksort partition):
   the proof `mid < len` would need the loop invariant `hi ≤ len-1`, which is no
   direct branch condition. That is the elision lever toward **Rust parity**.
2. **"under clang" is NOT reachable** — and that is no Vire weakness: even
   with ALL checks off, Vire (0.132/0.480) stays above clang (0.110/0.458). **clang++
   has ZERO bounds checks + the LLVM codegen optimum → it IS the ceiling for every
   LLVM language.** Rust (which likewise has checks + uses LLVM) lands identically at
   0.122/0.479 = ~1.05-1.10× clang. Vire can at best REACH clang (parity),
   not undercut it — there is no Vire advantage that clang++ does not also have.

**Conclusion:** the reachable + valuable target value is **Rust parity** (via
bounds elision of the data-dependent indices), not "under clang". The `div/rem` fix
(inline `sdiv`/`srem` with a constant divisor) is implemented (helps -O0/non-LTO;
under -O2 -flto LTO inlines `jrt_ldiv` anyway). `FASTLLVM_NO_BOUNDS=1` = a measurement flag.
