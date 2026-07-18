# Is a dedicated language worth it for FastLLVM? — Evaluation

*Name of the language: **Vire** — from Latin *vīrēs* ("forces, strength"): light,
yet powerful. File extension `.vr`. (Web-checked as free for use as a language name, as of July 2026.)
Details of the syntax in [LANGUAGE.md](LANGUAGE.md) and [REFERENCE.md](REFERENCE.md),
examples in [examples/](examples/), feature evaluation in
[FEATURES-EVALUATION.md](FEATURES-EVALUATION.md).*

## 1. The claim (and where it contradicts itself)

What is wanted is a language that is simultaneously:

1. **as simple as Python** (no lifetimes, no ownership, no manual memory management),
2. **memory-safe** (no use-after-free, no OOB, no null-deref),
3. **high-performance** (Rust/C level),
4. **AOT-compiled**,
5. **(almost) without runtime**,
6. **with access to all C, C++ and Rust libraries**,
7. **covers all domains of C/C++/Rust** (low-level to high-level),
8. **extremely light, yet powerful**.

Three of these points stand in genuine tension. Honest analysis first,
because it determines the entire design:

### Tension A — "memory-safe" + "no ownership" + "no runtime"

This is the **triangle of memory safety**. There are exactly three known ways
to establish memory safety, and each sacrifices exactly one of the three corners:

| Way | Example | Ownership syntax? | Runtime? |
|---|---|---|---|
| **Tracing GC** | Go, Java, C# | no ✅ | yes (collector, pauses) ❌ |
| **Ownership/Borrow** | Rust | yes ❌ (lifetimes) | no ✅ |
| **Reference counting (RC)** | Swift, Python | no ✅ | small (RC + cycles) ⚠️ |

"No ownership **and** no runtime **and** safe" simultaneously — that does not exist
in any existing language, **because it is impossible in the general case**:
safety for cyclic heap data without static annotation needs *some* form of
dynamic bookkeeping.

**FastLLVM's answer — and the actual reason the language is feasible:**
One can resolve the triangle *per program site* instead of globally. The whole-program
solver **proves ownership where possible** (→ 0 runtime, like Rust), and **falls back to
RC where not** (→ tiny runtime). That is exactly what FastLLVM already does today:

- Acyclic types → cycle collector **drops out entirely** (`-DFASTLLVM_NO_CYCLES`).
- Non-escaping objects → **stack instead of heap** (escape analysis).
- Immortal-only/borrowed locals → **retain/release drop away** (RC elision).
- The irreducible remainder → RC + Bacon-Rajan cycle collector (~2 KB).

Result in the benchmarks (DESIGN.md §9): loop-allocated objects run
**GC- AND RC-free** and beat Rust's `Box`. The programmer writes **zero**
memory annotations — the solver delivers the proof that Rust makes humans write.

→ **"No runtime for everything" is impossible; "no runtime for the provable
majority, RC for the rest, zero annotations" is built and measured.** The
language inherits this directly.

### Tension B — "as simple as Python" + "high-performance" + "AOT"

Python's simplicity comes from **dynamism**: duck typing, runtime reflection,
monkey-patching. Exactly that dynamism makes Python slow and requires an
interpreter/runtime. AOT + performance demands **static types**.

The way out is well known and decades-proven (ML, Haskell, OCaml, F#, Swift,
newer Rust ergonomics): **full type inference**. The code *looks like*
Python (no type annotations), but is statically typed — the types are
inferred (Hindley-Milner + local bidirectionality). You get Python's
lightness **without** Python's dynamism cost.

```python
# Python — dynamic, slow, runtime needed
def add(a, b): return a + b
```
```vire
// Vire — statically monomorphized, AOT, zero-cost
fn add(a, b) = a + b        // a, b: inferred; for every used type combination
                            // a specialized machine-code variant
```

**Honest about the surface:** This *does not look identical* (`fn`≠`def`, `=`≠`:`+
`return`). Vire's syntax is predominantly **Rust-shaped** (`fn`, `impl Ord for
Point`, `match`, `->`, `Self`, `?`, `..`, block-as-expression) with **Python spice**
(`elif`, `and/or/not`, comprehensions, interpolation without `f`) — a *Rust/Python
creole*. What **feels light** is not the token similarity to Python,
but the **absence of type annotations** through inference. That is exactly the
lever:

→ **The "simplicity of Python" is attainable if you replace dynamism with
inference.** The price: no real runtime `eval`/monkey-patching (nobody needs that
for performance code anyway), and the closed-world assumption (see below).

### Tension C — "all C/C++/Rust libraries" + "memory-safe/dedicated language"

This is the **hardest and most oversold** point — everywhere, not only
here. The sober reality of language interoperability:

- **C:** The C ABI is the **universal glue** of the entire software world.
  Direct FFI is simple and complete. SQLite, zlib, OpenSSL, BLAS/LAPACK,
  libcurl, FFmpeg, half the OS — all C ABI. **✅ fully feasible.**
- **C++:** Partially. The Itanium ABI (Linux) is stable enough for name mangling and
  vtables, but **templates** (header-only, need instantiation), exceptions,
  RAII destructors and `std::` types need a C++-aware binding generator
  (like Swift's C++ interop or `cxx`/`autocxx`). "All" C++ libraries incl.
  arbitrary templates: **no**. Public API via generated bindings: **yes.**
- **Rust:** **No stable ABI.** Rust crates can only be integrated if they
  export a C interface (`#[no_mangle] extern "C"`) — but then they are
  effectively C libraries. Calling idiomatic Rust (generics, traits, `&` references at
  the boundary) directly would need the Rust compiler itself. **No.**

**Important for positioning:** *No* language except C++ itself can use "all C++
libraries", and *no* language except Rust can use "all Rust crates"
— this holds for Python, Go, Swift, Zig, Julia **equally**. The C ABI is the
boundary for all. The claim "all three" must therefore honestly read:

> **C natively and completely; C++ and Rust via their C-ABI surfaces or
> generated bindings.** That is practically the same reach as Python C
> extensions or Swift — and covers real >90% of the important libraries, because
> the performance-critical world speaks C ABIs.

## 2. What FastLLVM already delivers today (half the compiler stands)

The expensive, risky part of such a compiler is **not** the parser — it is
codegen, memory model and safety-check elision. That is **finished and
measured**:

- **LLVM backend** (textual IR + clang, `-march=native`, LTO): Rust/C level,
  in arithmetic AVX2-vectorized faster than both.\* *(\* This number holds for
  **wrapping** arithmetic. Vire's checked-overflow default (REFERENCE §3.1) breaks
  autovectorization — empirically **4.6×** slower, see [M0-MEASUREMENT.md](M0-MEASUREMENT.md).
  Hot numeric kernels must explicitly use `+%`/`Wrapping[T]` to keep the
  vector path.)*
- **Memory model:** RC + cycle collector, escape analysis→stack, RC elision,
  acyclicity→collector elimination. Heap balance 0 live everywhere.
- **Safety-check elision:** bounds-check elision via GVN (loop guards,
  long induction, and-masks, constant bounds), null-check elision for
  non-null receivers, pending-check elision.
- **Whole-program solver:** RTA/CHA devirtualization, biconditional devirt,
  inlining, interprocedural escape summaries.
- **Platforms:** hosted (libc), freestanding/seL4 (~2 KB runtime), threads.

A dedicated language would have to build **none** of this anew. It would only need a new
**front end** (lexer, parser, type inference) that produces the same mid-level IR (`crates/ir`).
The entire solver + backend remains.

## 3. Why the Java-bytecode front end is a *disadvantage*

The strongest single argument for a dedicated language: **javac bytecode is a
poor IR source**, and that has cost us real work.

- **No SSA, aggressive slot recycling.** Exactly that made bounds-check
  elision hard: index, bound and array sit at the loop guard in *different*
  locals than at the access, although they are the same values. We had to build a complete
  **global value numbering with phi collapse**, just to *reconstruct*
  the SSA information that a dedicated front end would have **for free**.
- **Java-semantics ballast:** everything is an `Object` with header, autoboxing of
  primitives in generics, `int`-only array indices, forced classes, no
  value types, no unsigned integers, no control over the layout.
- **JNI interop** is hard instead of lightweight.

A dedicated front end that produces **SSA directly** would:
- make the solver passes **simpler and more effective** (no GVN fight against
  slot reuse),
- allow **value types/structs without header**, unsigned types, direct C layout,
- **first-class C FFI** instead of JNI,
- free the language from Java semantics (no boxing, no forced OOP).

## 4. What the language would concretely provide

1. **A niche that is genuinely empty.** There is nothing today that is *simultaneously*
   Python-light **and** Rust-fast **and** without memory annotations **and**
   AOT-native **and** without noteworthy runtime. Go has GC + pauses. Swift has
   RC, but is Apple-centric and not without runtime. Nim/Crystal come
   closest, but have GC or RC without whole-program elimination. Zig is fast,
   but manual/unsafe. **Vire = Nim's/Crystal's ergonomics + FastLLVM's provable
   RC elimination.**
2. **The proof stands.** The benchmarks show: the technique holds Rust level (and
   beats it in parts). The risk "does this even work performantly?" is already
   answered — with a *foreign*, unfavorable front-end language (Java). With
   dedicated SSA IR it gets rather better.
3. **Ergonomics gain:** Python syntax + static safety + zero manual
   memory management is *the* buying reason for most users. They write
   application logic as in Python and get C binaries.
4. **Low-level to high:** value types + C layout + freestanding target cover the
   C/Zig domain; traits/generics/pattern matching/sum types the Rust domain;
   inference + GC-like ergonomics the Python/Go domain. One language core, three worlds.

## 5. Effort, risk, limits (honest)

**What has to be built anew:**
- Lexer + parser (weeks).
- **Type inference** (Hindley-Milner + trait/type-class resolution + monomorphization)
  — the most demanding piece. *Vanilla* HM is textbook; **HM + trait resolution +
  coherence + monomorphization soundness is not** — Rust has worked on it for years
  (chalk, new trait solver, coherence rules). Add to it the error ergonomics:
  global inference reports unification errors notoriously *far away* from the cause
  (the ML/Haskell wart) — this undermines "simple as Python" more than the syntax
  delivers it. Countermeasure: bidirectional inference with local anchors
  (signatures at function/module boundaries) keeps errors close — but costs a few desired
  annotations. **Not a pure textbook problem, but real integration work.**
- Lowering language→`crates/ir` **in SSA** (manageable, IR exists).
- Minimal standard library (strings, collections, I/O — much via C libc).
- C-header→binding generator (for the interop claim).

**Real limits that remain:**
- **Closed world.** Whole-program ownership inference and devirtualization need
  *all* source texts at compile time. No loading of unknown code at runtime
  (plugins only over stable ABI boundaries). That is the price for "RC elimination
  without annotations" — and for FastLLVM's target audience (native binaries, seL4) given
  anyway.
- **Never 100% runtime-free.** The cyclic, non-provable rest needs RC +
  collector (~2 KB). For many programs it is gone entirely, never guaranteed.
- **"All" C++/Rust libraries** remains "all with C-ABI surface" (see §1.C).
- **Inference limits:** global type inference without *any* annotation can become
  ambiguous; at public API and FFI boundaries annotations are needed (and there
  also desirable as documentation). That stays Python-light (annotation optional, not
  mandatory like Rust lifetimes).

## 6. Verdict

**Yes — and with an unusually favorable ratio, because the hard half
(backend, memory model, check elision, solver) already stands and is proven in benchmarks.**
The Java-bytecode path was the prototype that proved that the
*technique* catches up to Rust. A dedicated language lowering to SSA clears away exactly the
friction that cost us work (GVN against slot reuse), and unlocks the
ergonomics that Java obstructs (value types, no boxing, C FFI, no OOP obligation).

**The claim must be honestly tailored at three points** — that does not make it
smaller, only correct:
1. "No runtime" → **no runtime for the provable majority, ~2 KB RC for the
   cyclic rest.**
2. "All libraries" → **C natively; C++/Rust via C ABI/bindings** (the same boundary
   as for any non-C++/Rust language).
3. "As simple as Python" → **syntax yes; semantics statically inferred** (no
   runtime `eval`, closed world).

Within this tailoring the language is **realistic, differentiated from everything
existing and technically already half finished**. Recommendation: build it as a
standalone front end on `crates/ir`, with SSA generation from the start.

**Recommended build plan (order) — measurement first, no more design:**
0. **Alias-precision spike (the decisive measurement, see §7).** A small but
   *idiomatically realistic* program with shared, escaping, mutating
   state (no sieve, no word counter) lowered by hand to `crates/ir` and
   measured: (a) what fraction stays RC-free, (b) how often the RC path fires under
   threads *atomically contended*. **This one number decides whether "Rust level without
   annotations" is a slogan or a result** — before any front-end effort.
1. Nail down syntax + type system ([LANGUAGE.md](LANGUAGE.md), [REFERENCE.md](REFERENCE.md)).
2. Lexer/parser → AST (plan: [PARSER.md](PARSER.md)).
3. Bidirectional HM inference + trait resolution/coherence + monomorphization.
4. AST→`crates/ir` in SSA (reuse solver/backend unchanged).
5. Minimal stdlib over libc + C FFI; then C-header binding generator.
6. Self-benchmark against the existing suite (goal: ≤ today's Java numbers,
   expectably better because of SSA) — **plus** the §7 compile-time test at 100k+ LOC.

Feature roadmap (points 1–8): [../TODO.md](../TODO.md).

## 7. Residual risks — where the evaluation is (rightly) under pressure

*Addendum after external critique. §§1–6 remain valid, but the risk distribution
is shifted: the residual risk lies **not** in the front end as busywork
("lexer/parser weeks"), but at two unproven spots.*

> **Now measured ([M0-MEASUREMENT.md](M0-MEASUREMENT.md)):** The adversarial
> shared/cyclic case is **>1000× slower** than Rust (cycle collector
> super-linear, → timeout at 100k nodes), even without collector 4.4×, atomic RC
> 6.3×. The conjectures of §7.1/7.3 are thereby **proven**, no longer merely named.
> Gate verdict: **conditional continue** — first collector scaling + borrow inference,
> then front end.

### 7.1 The one load-bearing, unproven assumption — alias precision
Everything hinges on the solver reconstructing aliasing/escape/ownership
**precisely enough without annotations** to be *simultaneously* safe **and** Rust-fast.
Proven and measured is only the **backend half** (escape→stack, RC elision,
acyclicity). The **front-end half** — does the inference master the adversarial
alias cases? — is not shown. Decisive, and glossed over in §§1–6: where the
solver *cannot* prove, the fallback is indeed **safe** (RC), but **not
Rust-fast**. "Rust level without annotations" therefore holds only for the *provable
subset* — and **its size in idiomatic code is the one number that is not
measured**. The benchmarks show exactly the escape-friendly cases
(loop-local, non-escaping → trivially beats `Box`), *not* the RC-heavy
case. And there it gets expensive: **atomic refcounts under threads, contended, are
exactly the Swift ARC problem** that slows down hot paths. "Provable RC elimination"
is strong; "the RC path competes with Rust where it fires" is a **separate,
unproven** claim.

### 7.2 The concrete gap — mutation under aliasing / iterator invalidation
The heart of Rust's borrow checker is the **XOR rule** (one mutable *or*
many shared) — and Vire throws away exactly the annotations that make it decidable.
Example:
```vire
mut xs = [1, 2, 3]
for x in xs { xs.push(x) }     // backing store reallocated while the iterator
                               // points into it
```
RC prevents UAF at the list *object*, but the *buffer* is relocated on `push`.
Safe is that only with Python semantics (iterator holds RC on the buffer
**+** every access bounds-checked) — which collides **head-on** with "iterators are
inlined, zero-cost" (LANGUAGE §7). You cannot *at once* lower iteration to
a raw pointer walk **and** allow mutation-during-iteration — unless the solver
**proves non-mutation** over the loop. That *is* the discarded borrow proof.
**The same alias question is in `spawn`** ("value must be
moved" = alias analysis across the thread boundary = Send/Sync, for which Rust
*needs* auto-traits). **Iterator, concurrency, borrow are one and the same
problem.**

→ **Design decision (adopted, see [REFERENCE.md](REFERENCE.md) §9a):** Vire
solves this *not* through silent slow RC iteration, but **conservatively and
locally**: the compiler checks *specifically* whether the loop body mutates the iterated collection
(or a local alias of it). Provably non-mutating → zero-cost
inline walk. Not provable → **compile-time error** that
demands explicit intent (`for x in xs.snapshot()` or index loop). This *one-collection-one-
loop* check is far more tractable than general alias analysis — but it is
**real analysis, not omission**, and the general precision from §7.1 remains the
hard core.

### 7.3 Two "advantages" that are also disadvantages
- **Whole-program / one pass / no headers** (LANGUAGE §12) is for
  *ergonomics* a **disadvantage**: no separate compilation, no usable
  incremental caching — every build re-analyzes everything. Whole-program escape/RC
  **+** monomorphization **+** `comptime` evaluation stacks three expensive phases and
  removes the module boundaries that allow caching. Rust is already notorious for compile
  times because of mono; Vire lays whole-program *on top*. For "as light as
  Python" (= fast iteration) that is the direct undermining of the buying argument.
  **Scaling to 100k+ LOC: unmeasured** (Zig's `comptime` shows that exactly this
  path leads to compile-time/memory problems). Countermeasures to evaluate:
  analysis caching per function with invalidation over the call graph; `comptime`
  budgets; separate analysis layer for "just build fast, optimize later".
- **Global inference** is not a "solved problem" (§5, corrected): HM + traits +
  coherence + mono soundness is integration work, and error locality suffers.

### 7.4 Consequence
The verdict "yes, feasible, half finished" remains — but **the most honest next step
is measurement, not design** (build-plan step 0). Two numbers decide everything:
(1) the RC-free fraction in *idiomatic* code + the atomic contention rate under
threads (§7.1), (2) the compile time at 100k+ LOC (§7.3). As long as those are missing,
"Rust level without annotations" is a *well-founded slogan*, not a result. What **stands
firm**: resolving the safety triangle *per site* is real; the expensive half
is finished and measured (where most language projects fail); the modern
fundamental decisions (Option instead of null, errors as values, comptime instead of RTTI,
hygienic macros) are well-founded. That carries — the two measurements say *how
far*.

See [LANGUAGE.md](LANGUAGE.md)/[REFERENCE.md](REFERENCE.md) for the syntax and
[examples/](examples/) for programs across all target domains.
