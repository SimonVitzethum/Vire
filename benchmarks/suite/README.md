# Benchmark suite: Vire vs Rust vs C++ (clang++)

`./run.sh` — builds each benchmark in all three languages (`vire build`, `rustc -O
-C target-cpu=native`, `clang++ -O2 -march=native`), measures best-of-5, and checks
output equality. C++ = **clang++** (LLVM, like Vire) for a fair codegen
comparison (g++/GCC diverges separately, see RECURSION-INLINING.md).

## Results (best-of-5, the same machine)
| Benchmark | Vire | Rust | clang++ | Vire/clang |
|---|---|---|---|---|
| bitmanip (popcount) | 0.187 | 0.186 | 0.186 | **1.00×** |
| matmul (256³ naive) | 0.012 | 0.010 | 0.013 | **0.97×** |
| nbody (2000, 20 steps) | 0.073 | 0.072 | 0.076 | **0.95×** |
| montecarlo (20M, LCG) | 0.039 | 0.039 | 0.040 | **0.98×** |
| vcall (dyn dispatch, 100M) | 0.244 | 0.116 | 0.273 | **0.89×** |
| sort (quicksort 2M) | 0.170 | 0.122 | 0.111 | 1.52× |
| binsearch (10M lookups) | 0.561 | 0.481 | 0.455 | 1.23× |

## Interpretation
- **Compute (bitmanip/matmul/nbody/montecarlo): Vire = clang parity, partly faster**
  (matmul/nbody 0.95–0.97×). Both via LLVM → the same codegen optimum.
- **vcall = trait objects (dyn dispatch): Vire 0.89× — FASTER than C++ `virtual`.**
  Vire's vtable dispatch (built this session) is as fast as C++, here even
  somewhat faster. Rust's `dyn` is faster still (0.116) — Rust devirtualizes
  the monomorphic call in the benchmark partly.
- **Array-index-heavy (sort/binsearch): Vire 1.2–1.5× slower.** The reason is
  **bounds checks** on every array access — the solver (`elide_bounds`) removes
  many, but not the data-dependent ones (quicksort partition, binary-search mid). That
  is the clear, honest optimization point (Rust has the same principle, but elides
  more; C++ has no checks at all). The next perf lever for Vire.
- **DIFFs in the table** are pure float formatting (Vire/C++ `%g` scientific
  vs Rust's full precision) or summation rounding (nbody) — identical values.

## Category coverage (honest)
Of the ~80 categories on the wishlist, the **compute-, memory-, data-structure-,
algorithm-, and numerics-bound** ones run — the ones measured here cover
microbenchmarks (arith/bit/recursion/virtual-calls/closures/generics — see also
`../vire-lang/`), numerics (matmul/N-body/Monte-Carlo), algorithms (sort/search), and
memory (arena/RC/heap — see RAM-REDUCTION.md, ESCAPE-ARENA.md).

**NOT covered (need libraries/features that Vire does not yet have):**
- **Text processing** (regex, JSON, XML, CSV, YAML, TOML, HTML, Markdown) — needs
  a string/parser library.
- **Cryptography** (AES, SHA, BLAKE3, RSA, ECC, Argon2) — needs a crypto lib
  (or byte arrays + bit ops; `ArrKind::Byte` still missing).
- **Parallelism** (thread pool, work stealing, channels, lock-free, parallel sort) —
  Vire has only the Java `--threads` path (pthreads), no high-level concurrency.
- **I/O** (filesystem, mmap, TCP/UDP/HTTP/WebSocket) — needs an IO/network library.
- **Complex data structures** (B-trees, AVL/RB, priority queue) — for lack of typed
  collections (`List[T]`) and array-as-parameter (see below) only limited.

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
