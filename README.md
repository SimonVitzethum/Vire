# Vire

**Vire** is a programming language: *as light as Python, as fast as C/Rust,
memory-safe — without lifetimes, without ownership syntax, without manual memory
management.* It compiles **AOT** to native binaries through a whole-program solver
and an LLVM backend, and runs (for the provable majority) **without a runtime**.

> Name from the Latin *vīrēs* ("forces, strength") — light, yet powerful.
> File extension `.vr`. Current state: language specified; front-end, solver and
> backend built, compiling `.vr` to native binaries and benchmarked against
> Rust/C++/gcc.

```vire
fn word_counts(text: Str) -> Map[Str, Int] {
    mut counts = {}
    for word in text.lower().split_whitespace() {
        counts[word] = counts.get(word).or(0) + 1
    }
    counts
}
```

Reads like Python — compiles to a memory-safe, RC-eliminated native binary.

## The idea in one paragraph

Classically, memory safety comes with one of three costs: a garbage collector
(runtime/pauses), ownership + lifetimes (Rust's annotation burden), or reference
counting (a small runtime). Vire resolves this **per program site**: a whole-program
solver **proves** ownership where possible (→ zero runtime, like Rust), and falls
back to lean RC where necessary. The programmer writes **zero** memory annotations.
Types are fully **inferred** (Python ergonomics without Python's dynamic cost). This
is feasible because Vire is **closed-world** (all sources available at compile time)
and sits on a backend that already delivers exactly these proofs.

## Status & architecture

Vire is a **front-end** on a **built, measured backend**. The whole pipeline is
functional: `vire build foo.vr -o foo` and `vire run foo.vr` produce and execute
native binaries today.

| Layer | Status |
|---|---|
| **Vire front-end** (`crates/vire`) — lexer, parser, macro expansion, recursive inline, type inference, lowering to SSA IR | **built & working** — compiles `.vr` end-to-end to native code |
| **Mid-level IR** (`crates/ir`) | built |
| **Whole-program solver** (`crates/solver`) — devirtualization, inlining, escape/RC elision, bounds/null-check elision, field auto-narrowing, region inference | built |
| **LLVM backend** (`crates/backend`) — textual IR + clang `-O2 -flto -march=native`; TBAA, `!invariant.load`, branch weights, cold error paths; hosted/freestanding/threads | built |
| **Runtime** (`crates/driver`) — RC + Bacon–Rajan cycle collector, slab allocator, packed 16-byte header | built |

The backend was developed and hardened via a **Java-bytecode front-end prototype**
(the `fastjavac` path), whose **65 heap-balance regression tests (0 live objects at
exit)** are the soundness oracle — the floor every optimization must keep green. See
[DESIGN.md](DESIGN.md) and [benchmarks/](benchmarks/).

## Benchmarks (snapshot)

Cross-compiler on this machine (best-of-5, output-verified; Vire vs clang++ 22, g++
16, rustc 1.97, all `-O2 -flto -march=native`; measured 2026-07):

| Benchmark | Vire vs clang++ | Notes |
|---|---|---|
| montecarlo | **0.99×** | compute-bound, parity |
| nbody / bitmanip | **~1.00×** | at parity |
| **vcall** | **0.41×** (2.4× faster) | solver devirtualization; near-Rust, beats clang `virtual` |
| **binsearch (10M)** | **1.06×** (= 1.00× Rust) | midpoint check *proved* redundant + elided — safely |
| matmul (256³ naive) | 1.25× | affine-index check (#1) + a vectorization gap vs Rust |
| sort (quicksort 2M) | 1.43× | partition bounds loaded from a stack array (opaque) |

Vire is at or above clang/Rust level on compute, **2.4× faster on virtual dispatch**,
and now at **Rust parity on binary search** — the constant-bound solver *proves* the
`(lo+hi)/2` midpoint in range and drops the check while staying fully memory-safe (a
real out-of-bounds access still throws). The remaining array gaps are a vectorization
case (matmul) and an opaque-stack case (sort); see [TODO.md](TODO.md) and
[benchmarks/suite/](benchmarks/suite/). On the Java-AOT path, binary-trees is now
**1.7× C++** (was 3.6×) after region inference — see [benchmarks/](benchmarks/).

## Documents

- **[TODO.md](TODO.md)** — roadmap and remaining work (M0 risk gate, front-end
  pipeline, features 1–8, performance).
- **[DESIGN.md](DESIGN.md)** — backend architecture (solver, memory model,
  benchmarks). Describes the Java-bytecode path = the proof/bootstrap base.
- **[language/EVALUATION.md](language/EVALUATION.md)** — honest feasibility: the three
  tensions (no runtime / all libraries / Python-light) and §7 residual risks
  (alias precision, compile time).
- **[language/LANGUAGE.md](language/LANGUAGE.md)** — syntax tour (quick start).
- **[language/REFERENCE.md](language/REFERENCE.md)** — full syntax/feature reference.
- **[language/FEATURES-EVALUATION.md](language/FEATURES-EVALUATION.md)** — assessment of
  the eight requested features (multithreading, templates, comptime reflection, own
  preprocessor, Meson, logger, Go-style error handling, debug crash paths).
- **[language/PARSER.md](language/PARSER.md)** — parser/front-end build plan.
- **[language/examples/](language/examples/)** — example programs across areas and
  features.
- **[benchmarks/](benchmarks/)** — benchmark suite (Java/Rust/C++), runner, analysis.

## Core language ideas (in brief)

- **Inference over annotation** — types appear nowhere yet are all known.
- **No `null`** — `Option[T]`; no exceptions — errors are values (Go spirit) with
  `?` propagation.
- **`type`** for product and sum types (value types, no object header), **traits** +
  monomorphized **generics**.
- **`comptime`** — code that runs in the compiler: reflection, derivations,
  conditional compilation — zero-cost, no runtime metadata ballast.
- **Invisible memory** — stack/heap/RC decided by the solver; `&` optional.
- **Concurrency safe by construction** — channels (CSP) + `Mutex`/`Atomic`; the
  solver rejects shared bare mutable state.
- **C native** — `extern "C"`/header bindings; C++/Rust via the C ABI. Meson
  first-class.

The name and details are provisional and easy to change; the design is the core.
