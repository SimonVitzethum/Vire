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
| sort (quicksort 2M) | 0.169 | 0.126 | 0.114 | 1.43× |
| binsearch (10M lookups) | 0.478 | 0.480 | 0.449 | **1.06×** |

**binsearch now = 1.00× Rust** (0.478 vs 0.480): the constant upper/lower-bound
fixpoint (bounds.rs) proves the midpoint `0 ≤ (lo+hi)/2 ≤ n-1 < len` and elides its
data-dependent check — safely (a real out-of-bounds access still throws). Its
no-checks ceiling was 1.07× clang, so essentially every provably-safe check is gone.

## Interpretation
- **Compute (bitmanip/nbody/montecarlo): Vire = clang parity** (0.99–1.00×).
  Both go through LLVM → the same codegen optimum.
- **binsearch: Vire = 1.00× Rust / 1.06× clang** — the constant upper/lower-bound
  fixpoint elides the data-dependent midpoint check (`0 ≤ (lo+hi)/2 ≤ n-1 < len`),
  safely. This is the LLVM-safe-language ceiling (its no-checks floor is 1.07× clang).
- **matmul (256³ naive): Vire ~1.25× clang / 1.6× Rust.** Two separate things: a
  residual bounds check on the *affine* index `C[i*n+j]` (its counted-loop bound
  lives in the loop guard, which the arithmetic fixpoint does not read — the
  guard-aware affine extension is TODO #1), AND, more importantly, a **vectorization
  gap**: even with all checks off (`FASTLLVM_NO_BOUNDS`) Vire is 1.5× Rust here —
  Rust autovectorizes the inner product loop better. Bounds elision alone cannot win
  matmul; it is a codegen/vectorization case.
- **vcall = trait objects (dyn dispatch): Vire 0.41× — 2.4× FASTER than clang
  `virtual`, and essentially at Rust.** Vire's solver devirtualizes + inlines the
  vtable dispatch; clang keeps the indirect call. (Vire's vcall time roughly halved
  vs the previous snapshot as the devirt/vtable path matured.)
- **sort (quicksort): Vire 1.43× clang / 1.35× Rust.** The partition bounds `lo`/`hi`
  are **loaded from an explicit stack array** (`lostack[sp]`), so they are opaque to
  the value analysis — proving `a[j] < len` would need an array-*content* invariant
  (the stack only ever holds in-range indices), a much harder analysis. Its no-checks
  ceiling is 1.03× Rust, so bounds elision *could* win it, but not with the current
  value-based reasoning. The honest remaining lever.
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
| matmul | 0.017 | **0.015** (−10%) | 0.010 | 0.013 | open — affine index (guard-aware, TODO #1) **and** a vectorization gap |

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
3. **sort/matmul are not (yet) winnable by bounds elision**: sort's bounds are opaque
   stack loads (would need an array-content invariant); matmul is vectorization-bound
   vs Rust even with checks off. Both are documented, neither is a soundness issue.
