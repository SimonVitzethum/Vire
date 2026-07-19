# Benchmark suite: Vire vs Rust vs C++ (clang++)

`./run.sh` — builds each benchmark in all three languages (`vire build`, `rustc -O
-C target-cpu=native`, `clang++ -O2 -march=native`), measures best-of-5, and checks
output equality. C++ = **clang++** (LLVM, like Vire) for a fair codegen
comparison (g++/GCC diverges separately, see RECURSION-INLINING.md).

## Results (best-of-5, the same machine, measured 2026-07)
| Benchmark | Vire | Rust | clang++ | Vire/clang |
|---|---|---|---|---|
| bitmanip (popcount) | 0.187 | 0.188 | 0.187 | **1.00×** |
| matmul (256³ ikj) | 0.0036 | 0.0043 | 0.0047 | **0.77×** |
| nbody (2000, 20 steps) | 0.072 | 0.076 | 0.074 | **0.97×** |
| montecarlo (20M, LCG) | 0.041 | 0.041 | 0.041 | **0.99×** |
| vcall (dyn dispatch, 100M) | 0.115 | 0.115 | 0.276 | **0.41×** |
| sort (quicksort 2M) | 0.128 | 0.122 | 0.112 | 1.14× |
| binsearch (10M lookups) | 0.480 | 0.477 | 0.451 | **1.06×** |

**Average (this suite, Vire/Rust):** geometric mean **0.97×** (was 1.01× before the
matmul ikj port) — memory-safe Vire is at/just under Rust parity here; every benchmark is
within ±16% of Rust and three (matmul, nbody, vcall) are faster. **binsearch = 1.00× Rust** (the constant upper/lower-bound
fixpoint proves the midpoint `0 ≤ (lo+hi)/2 ≤ n-1 < len` and elides the check, safely
— a real OOB still throws); **sort = 1.05× Rust** (its uncatchable checks abort
noreturn, Rust's structure); **matmul = 0.83× Rust / 0.77× clang** — beats both
(cache-friendly ikj order → vectorized SAXPY inner loop, affine index elided).

## Interpretation
- **Compute (bitmanip/nbody/montecarlo): Vire = clang parity** (0.99–1.00×).
  Both go through LLVM → the same codegen optimum.
- **binsearch: Vire = 1.00× Rust / 1.06× clang** — the constant upper/lower-bound
  fixpoint elides the data-dependent midpoint check (`0 ≤ (lo+hi)/2 ≤ n-1 < len`),
  safely. This is the LLVM-safe-language ceiling (its no-checks floor is 1.07× clang).
- **matmul (256³ ikj): Vire 0.83× Rust / 0.77× clang — beats both.** The kernel now
  uses the cache-friendly **ikj** loop order (the same order the Java-AOT `Matmul` uses
  to beat Rust): the inner loop `c[ci+j] += aik*b[bk+j]` is unit-stride in `j` — a
  SAXPY — so LLVM **vectorizes** it (8 packed-FP ops). The earlier ijk dot-product
  order had a strided `b[k*n+col]` column read that vectorizes in no compiler, which is
  why it sat at 1.27× Rust (a scalar-scheduling residual). Same algorithm across all
  three languages (fair), bit-identical output (100659197). The affine index
  (`bounds.rs` Path 4, `N·a+b < N² ≤ len`) is still elided; a real OOB still throws.
- **vcall = trait objects (dyn dispatch): Vire 0.41× — 2.4× FASTER than clang
  `virtual`, and essentially at Rust.** Vire's solver devirtualizes + inlines the
  vtable dispatch; clang keeps the indirect call. (Vire's vcall time roughly halved
  vs the previous snapshot as the devirt/vtable path matured.)
- **sort (quicksort): Vire 1.14× clang / 1.05× Rust.** Measured finding: Rust's sort
  has the SAME bounds checks (47× more `jae` than Vire, actually) — the gap was never
  missing elision, it was Vire's **check model**. Vire's pending-exception throw
  (set-pending + continue + a `phi` folding the load result with a default) was
  costlier than Rust's noreturn panic. Now, when the whole program provably can't
  catch a runtime exception (no try/catch — always true for pure Vire), the check
  aborts via a `_fatal` noreturn helper and its failure block ends in `unreachable`,
  so the load result is direct (Rust's structure). **1.35× → 1.05× Rust**, matching
  the no-checks ceiling. Memory safety unchanged (a real OOB still throws, verified).
  The last ~5% is the explicit-stack structure Vire needs *because it can't yet pass
  arrays as parameters* — Rust writes `qsort(a: &mut [i64], …)` recursively (TODO).
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
Measured ceiling with `FASTLLVM_NO_BOUNDS=1` (a **measurement-only** flag that emits
checks off — never shipped; the shipped path stays memory-safe):

| Benchmark | Vire (safe) | Vire (no checks) | Rust | clang++ | status |
|---|---|---|---|---|---|
| **binsearch** | **0.478** | 0.482 | 0.480 | 0.449 | **won: 1.00× Rust** — check *proved* redundant and elided |
| sort | 0.166 | **0.130** (−20%) | 0.126 | 0.110 | open — partition bounds loaded from a stack array (opaque) |
| matmul | 0.0036 | ~0.0036 (elided) | 0.0043 | 0.0047 | **won: 0.83× Rust** — ikj vectorizes + affine index elided |

**Findings:**
1. **binsearch is now at parity with Rust.** The constant upper/lower-bound fixpoint
   ([bounds.rs](../../crates/solver/src/bounds.rs)) *proves* `0 ≤ (lo+hi)/2 ≤ n-1 <
   len` and elides the midpoint check — so the safe binary now measures like the
   no-checks build. **This removed the redundant check, not the safety**: a genuinely
   out-of-bounds access still throws (verified). This is exactly how Rust is fast.
2. **"under clang" is NOT reachable** — and that is no Vire weakness: even with ALL
   checks off, Vire stays at/above clang, because **clang++ has ZERO bounds checks +
   the LLVM codegen optimum → it IS the ceiling for every safe LLVM language.** Rust
   (safe, LLVM, with checks) lands at the same ~1.05–1.10× clang. So the honest,
   valuable target for a *memory-safe* language is **Rust parity**, which binsearch
   now reaches.
3. **matmul is now won** (0.83× Rust): the cache-friendly ikj order makes the inner
   loop a vectorizable SAXPY, and the affine index is elided — so the safe build already
   measures like the no-checks build. **sort is not (yet) winnable by bounds elision**:
   its bounds are opaque stack loads (would need an array-content invariant). Documented,
   not a soundness issue.
