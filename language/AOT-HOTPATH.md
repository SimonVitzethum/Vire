# AOT Hotpath Optimizer — Plan (find JIT paths statically + optimize)

*Goal (user request): an AOT compiler that finds, in the solver, the paths a JIT
would discover as hot at runtime, and then optimizes them aggressively in a JIT-like
manner — without warmup, without runtime profiling, without JIT overhead. Fits Vire's
closed-world AOT model.*

## The core idea
A JIT wins because it knows the **hot paths** (profiling) and **speculatively
specializes** them (on observed types/values). An AOT compiler has no runtime
profile — but in the closed-world model it can **estimate hotness statically** and
apply the same optimizations **ahead of time**. Result: JIT peak performance with
AOT determinism (no warmup, no deopt, no code cache).

## Pipeline (five new/extended solver passes)

### 1. Static hotness estimation (`solver/hotness.rs`, NEW)
Estimates execution frequency per function/block/call site WITHOUT execution — the
heuristics that baseline JITs also use before real counters exist:
- **Loop depth:** blocks in loops ×10 per nesting level (classic frequency
  estimation). Back edges = loops (dominator analysis).
- **Branch heuristics:** backward branches "taken", null/error branches
  "not taken", `?`/Err paths cold.
- **Call frequency propagation:** a callee inherits the hotness of the call site
  (loop-local call = hot); propagated across the call graph (fixed point).
- **Recursion = hot** (self-/mutually-recursive SCCs in the call graph).
- Result: `f64` score per function/block → classes `Hot`/`Warm`/`Cold`.

### 2. Hot-path identification (the "JIT discovery", statically)
Functions/blocks above threshold = what a JIT would have compiled after N calls.
Additionally, form **superblocks**: merge hot call chains (A→B→C all hot) into a
single optimizable region — the AOT equivalent of JIT traces.

### 3. Tiered optimization budget (extends `inline.rs`)
Like JIT tiers (interpreter → baseline → optimizing), but decided statically:
- **Hot:** aggressive — large inline budget (inline even large hot callees),
  loop unrolling, scalar replacement, full specialization. Optimized for speed.
- **Warm:** moderate (today's default inlining).
- **Cold:** minimal — optimize for size, do not inline (smaller icache pressure,
  like a JIT leaving cold code in the interpreter).

### 4. Speculative specialization (`solver/specialize.rs`, NEW) — the JIT core
A JIT specializes on observed types/values. AOT analogues, provable closed-world:
- **Value specialization / partial evaluation:** hot function, called at hot call
  sites with a constant argument → specialized copy `f$const`, constant folded in
  (branches eliminated, loops possibly unrolled). = what a JIT does via constant
  feedback, here proved statically.
- **Type specialization:** hot monomorphic/CHA-devirtualized sites → direct,
  inlinable calls (the solver can already do this; here targeted at hot sites).
- **Guard elision on hot paths:** null/bounds/pending checks that the solver
  provably shows redundant, removed first on hot paths (already present today, but
  hotness-prioritized).

### 5. Layout/codegen hints (`backend`)
- Hot functions get `alwaysinline`/`hot` attribute to LLVM; cold ones `cold`/`minsize`.
- Arrange hot basic blocks together (block layout by hotness) → icache/BTB.
- Set LLVM `!prof` branch weights from the static estimate (LLVM then optimizes
  hotness-aware itself — the cheapest large lever, since LLVM does the rest).

## What the solver ALREADY has (foundation is in place)
RTA/CHA devirtualization, pruning, inliner (`inline.rs`), escape/stack analysis,
bounds/pending/longcmp elision, **monomorphization** (= type specialization for
generics), interprocedural region inference. The AOT hotpath optimizer is primarily:
(a) **hotness estimation** on top, (b) these passes **hotness-prioritized** instead of
uniform, (c) **partial evaluation** as a new pass, (d) **LLVM `!prof` weights** as
the cheapest multiplier.

## Order / effort
1. **`!prof` branch weights from loop depth** — small, large lever (LLVM does the
   rest). *First, because best effort/impact.*
2. **Hotness estimation** (`hotness.rs`) + tiered inline budget — medium.
3. **Partial evaluation** of hot functions with constant arguments — medium-large.
4. **Superblock formation + block layout** — large.

## Honest scoping
- This does not replace true PGO (profile-guided optimization): static estimation is
  sometimes off (it does not see data-dependent hotness). An optional **PGO path**
  (`-fprofile` instrumentation → rebuild) would be the honest complement for the
  cases where estimation is not enough — then it is "AOT with optional profile", not
  "AOT guesses everything".
- "Find JIT paths" here means **statically estimate what a JIT would have measured** —
  not measure. The gain is warmup-free + deterministic; the price is estimation
  inaccuracy. That is the honest trade, not a free lunch.

## Measurement plan (as with M0: measure first)
Before building: estimate the **ceiling** on the benchmarks (`benchmarks/`) — what does
manual `!prof` + `alwaysinline` on the hot loops yield versus -O2? If the gain is <5%,
the optimizer is not worth it (LLVM -O2 -march=native already gets almost everything);
if it is >20%, it is worth it. First the number, then the quarter — the same gate
discipline as with the frontend.

## BUILT + MEASURED: step 1 (`!prof` branch weights from loop depth)
The cheapest/most impactful plan step is implemented: `loop_branch_bias` in
`crates/backend/src/lib.rs` statically estimates (reducible CFG: edge `u→v` with
`v≤u` = back edge → loop header `v`, body `[v,u]`) which branch of a conditional
branch stays in the loop, and sets `!prof branch_weights` (100:3) at the loop-exit
branch. Runs in BOTH backends (Java + Vire). Test:
`crates/backend/tests/branch_weights.rs`. Disableable via `FASTLLVM_NO_PROF=1` (A/B).

**Ceiling measurement (gate discipline):** branch-heavy workload (200M iterations,
`if i%7 / elif i%13 / else`), 3 runs per variant:
- with `!prof`:  0.215 / 0.215 / 0.220 s
- without `!prof`: 0.216 / 0.212 / 0.220 s

→ **no measurable difference (~0%).** Confirms the prediction (<5%): LLVM
`-O2 -march=native` already arranges these branches optimally; the static weights
agree with LLVM's own loop heuristic and add nothing. The value would only lie where
LLVM guesses wrong (rare error/cold paths) — and even there small.

**Consequence (honest, gate-faithful):** step 1 is correctly + freely implemented,
but the measured ceiling does NOT justify the heavier steps 2–4 (full `hotness.rs`,
partial evaluation, superblocks) — the plan itself says "<5% → not worth it". The
real lever remains the RC/object path (region inference), not AOT branch tricks.
Steps 2–4 remain planned but unbuilt until a measured case justifies them (e.g.,
branch-heavy code with clear cold paths that LLVM mis-estimates — or the optional
PGO path).

## Investigation: are the four techniques worth it? (measurement-driven)
*Question: analyze call graph / branch probabilities / specialized versions for
frequent type combinations / multiple variants + runtime selection.*

**The decisive context first:** the benchmarks (`benchmarks/vire-lang/`) show
compute-bound code already at **C++/Rust parity** (arith 1.02×, fib 0.91×,
mandelbrot 0.99×, nsieve 1.02× Rust). The ONLY measured gap is the RC/object path
(binary-trees 2.65×). **None of the four techniques addresses memory management** —
they target compute/dispatch, which is already optimal. That frames every answer: the
headroom on the compute path is small, the lever for the real gap is region
inference, not AOT hotpath tricks. With this grounding:

1. **Analyze the whole call graph — YES, worth it, already present.** Devirt/pruning/
   inliner/`static_writes`/interprocedural region inference all run over the call
   graph. Cost low (closed world = complete graph present). No new large effort,
   rather the base on which the rest sits. **Verdict: already delivered.**

2. **Branch probabilities — CONDITIONALLY worth it, cheap version first.** LLVM
   `-O2` already estimates branches well (hence the parity). Static `!prof` weights
   from loop depth help mainly where LLVM guesses wrong: error/cold branches
   (`?`/Err, null checks). Expected gain on compute code **<5%** (it is already
   optimal), measurably more only on branch-heavy code with clear cold paths.
   **Cost small** (loop depth→`!prof`, LLVM does the rest). **Verdict: the cheap
   first step, but measure the ceiling first — on parity code there is little to
   gain.**

3. **Specialized versions for type combinations — PARTLY ALREADY PRESENT
   (monomorphization).** For generics Vire does exactly this (one instance per type
   argument). The addition would be **value specialization / partial evaluation**:
   hot function with constant argument → folded copy. Worth it ONLY if hot functions
   get constant args (config flags, fixed sizes) — rare in the benchmarks.
   **Verdict: type specialization done; value specialization situational, build only
   on a measured case, not speculatively (code bloat).**

4. **Multiple variants + runtime selection — LEAST valuable in closed-world AOT.**
   This is the most JIT-like proposal and exactly the one AOT needs least: if the
   type is statically known (closed world + monomorphization + CHA devirt), one calls
   the right variant **directly** — no runtime selection, no dispatch overhead, no
   bloat. Runtime selection helps only at **genuinely polymorphic** sites (megamorphic,
   3+ types) — and those the solver ALREADY handles via `CallPoly` (guarded
   devirtualization / polymorphic inline cache = a few variants + type-guard cascade).
   The rest would be code bloat (N variants × M functions, icache pressure) for cases
   the closed world resolves statically. **Only real niche:** value-based variants
   whose value only stabilizes at runtime (e.g., a mode flag) — there 2-variants +
   selection could be worthwhile, but that is a narrow case, not a general pass.
   **Verdict: no as a general strategy; the useful 90% (polymorphic sites) is already
   covered via `CallPoly`.**

## Overall verdict of the investigation
Priority by effort/impact, grounded in the measurements:
- **#1 (call graph):** done, foundation.
- **#2 (branch weights):** cheap, small gain (parity code) → first, but measure the
  ceiling; probably <5%.
- **#3 (type specialization):** done for generics; value specialization only on
  measured need.
- **#4 (runtime variants):** largely redundant to static mono+devirt in the closed
  world; the polymorphic niche is already present via `CallPoly`. **Do not build.**

**Key finding:** these four optimize a path that is already at C++/Rust level — the
return is marginal. The only measured gap (RC/objects, ~2.7×) is **orthogonal** to
it; interprocedural region inference closes it (v2 already brought pagerank
2.0×→1.55×), not hotpath specialization. Honest recommendation: `!prof` weights as a
cheap experiment, otherwise DEFER the AOT hotpath machinery and finish building region
inference — that is where the measured number sits.

## Replanning at the 5% scale (user: "even 5% is noticeable")
Re-measured with a lowered threshold — where are ≥5% real? Finding: **not in the
hotness/probability, but in codegen parity with clang.**

**Measurement Vire vs clang++ vs g++ (both C++ via their respective compiler,
best-of-7):**
| Benchmark | Vire | clang++ | g++ | Interpretation |
|---|---|---|---|---|
| fib | 0.080 | 0.077 | 0.042 | Vire = **clang parity**; g++ is the outlier (GCC-vs-LLVM) |
| arith | 0.939 | 0.935 | 0.653 | Vire = **clang parity**; g++ outlier |
| mandelbrot (before) | 0.142 | 0.125 | 0.113 | **real Vire-vs-LLVM gap (18%)** |

→ The apparent "C++ faster" gap on fib/arith is **GCC vs LLVM** (Vire uses clang/LLVM
like Rust; g++ optimizes naive recursion/loops better). This is NOT a Vire deficiency
and can only be obtained by a backend switch (or GCC-specific tricks) — **not
pursued** (Vire is at the LLVM optimum).

**The one real ≥5% lever — FMA contraction — BUILT:** mandelbrot was 18% behind clang
because clang by default fuses `a*b+c` into FMA (`-ffp-contract=on`) and Vire emitted
fmul/fadd WITHOUT the `contract` flag. Fix: `contract` on float ops (safest fast-math
level, only fusion, no reassociation). **mandelbrot 0.142→0.124 = clang parity**
(~13%). Verified: clang `-ffp-contract=off` (0.152) is slower than Vire — FMA was the
whole gap.

**Consequence for the AOT plan:** the ≥5% headroom lies in **codegen parity with
clang's defaults**, not in static hotness. Checklist of the clang-default levers:
- **FMA (`contract`)** — ✅ built, ~13% on float code.
- **`-O2 -flto -march=native`** — already active (= clang).
- **mem2reg/SROA of the naive alloca chain** — LLVM handles it (fib/arith = clang
  parity proves it: the store/reload chain is fully optimized away).
- **Remaining potentially ≥5%:** object header shrinking → better cache density on
  pointer chasing (RAM doc), AND FMA was the last float gap. Otherwise Vire is at the
  LLVM optimum; the hotness passes (2–4) stay ~0% (already measured) — the 5% scale
  does not change that, because the code is already LLVM-optimal.
- **Honest:** the only remaining ≥5% source would be to beat GCC on fib/arith — that
  is a backend topic (LLVM codegen quality), not an AOT pass.
