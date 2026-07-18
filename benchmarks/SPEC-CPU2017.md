# SPEC CPU 2017 — honest positioning + plan

*User request: "do SPEC CPU 2017 too". Here is the honest situation, why it does not
work 1:1, and what is meaningful instead.*

## Why SPEC CPU 2017 does not run directly on Vire
1. **Proprietary/licensed.** SPEC CPU 2017 is not a freely available benchmark set
   (license ~$1000, not freely downloadable in the repo/net). I cannot procure
   or run it here.
2. **The workloads are C/C++/Fortran, not Vire.** SPEC measures *compilers on
   standard programs*: intrate = perlbench, gcc, mcf, omnetpp, xalancbmk, x264,
   deepsjeng, leela, exchange2, xz; fprate = bwaves, cactuBSSN, namd, parest, povray,
   lbm, wrf, blender, cam4, imagick, nab, fotonik3d, roms. **These are tens of thousands
   of lines of real C/C++/Fortran** — not Vire programs. "Run SPEC on Vire"
   does not exist, because the SPEC programs are not written in Vire.
3. **Vire's backend IS clang/LLVM.** On the C/C++ SPEC workloads, "Vire performance"
   = clang performance — Vire contributes nothing there, it is not those languages. An
   honest SPEC comparison would only measure clang vs gcc vs icc, not Vire.

## What is meaningful instead
Two honest ways to measure SPEC-*representatively* without claiming SPEC:

**(a) Port SPEC-representative kernels to Vire** and measure against Rust/C++ —
one representative per SPEC characteristic:
- **519.lbm style** (Lattice-Boltzmann, float stencil over a large grid) →
  compute+array-bound. Vire can do this with `farray(n)` (typed float arrays).
  Expectation per the measurements: **parity** (compute+array = already parity, nsieve/
  mandelbrot).
- **505.mcf style** (network simplex, pointer chasing over graph nodes, int-heavy) →
  object/pointer-bound. Vire hits the **RC tax** here (~2× RC-only, like
  pagerank/binary-trees). The honest gap case.
- **557.xz style** (compression, byte arrays + bit manipulation) → needs **byte
  arrays** (today only i64 `array()`; ArrKind::Byte missing in the builtin) → currently
  not cleanly portable.

**(b) The already-running CLBG benchmarks as a proxy** (`vire-lang/`): they cover
the same axes — compute (mandelbrot, arith), recursion (fib), array (nsieve),
object allocation/GC (binary-trees). Result: **compute/array/recursion = C++/Rust
parity, object allocation = ~2.7× (RC tax)**. That is exactly the statement that a
SPEC run for a language of this construction would deliver.

## Plan (if SPEC-representative is wanted)
1. **lbm-style stencil** to Vire (`farray`, nested loops) + Rust/C++ → confirm the
   expected parity (fp-rate characteristic).
2. **mcf-style graph** (node objects, edge traversal) + Rust/C++ → quantify the RC tax
   (int-rate characteristic, the honest gap).
3. **Byte arrays** (`ArrKind::Byte` in the `array()` builtin) added → then xz style
   (compression) portable.
Not: claiming the real SPEC suite. That would be dishonest — it is neither available
nor written in Vire.

## In short
**SPEC CPU 2017 is proprietary and in C/C++/Fortran — not directly runnable on Vire,
and where it is, it would measure clang, not Vire.** The meaningful
alternative is SPEC-representative kernel ports + the CLBG suite, and they say
the same as the rest of this project: **compute parity, RC tax on objects.**
