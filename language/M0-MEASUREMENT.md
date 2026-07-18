# M0 — Risk Measurement (Gate before the Front-End)

*Execution of the gate from [EVALUATION.md](EVALUATION.md) §7 / [../TODO.md](../TODO.md)
M0. Goal: **measure the two unproven numbers** before front-end code is written —
instead of designing them. Programs & raw data: [../benchmarks/m0/](../benchmarks/m0/).*

**Summary:** The gate stands at **Yellow-to-Red**. The adversarial RC-/cycle-heavy
case is **not** at Rust level — at realistic size it is **>1000× slower** (cycle
collector super-linear), and even without the collector 4–6×. This is exactly the
half that §7 flagged as unproven. "Rust level without annotations" still holds for
the escape-friendly subset — for the shared/cyclic one it does **not**. Before the
front-end, two things must be settled (inference precision **and** collector
scaling), otherwise the language answers its own core promise negatively on the
interesting code.

---

## Method — why this measures the *right* half

The naive form ("lower the program to `crates/ir`, count RC") measures the **wrong
half**: if you lower by hand, you perform the alias analysis yourself — you measure
"does the backend elide RC *when the facts are known*", the long-proven half. Hence
this approach: **the real automatic pipeline** (Java front-end → solver → IR →
backend) does the inference — RTA, escape analysis, RC elision, borrow slots,
stable-statics, refcopy. What is measured, therefore, is what **automatic inference
without annotations** actually recovers. The **spread** = distance to the oracle:

- **Oracle (upper bound):** For the test program, *all* nodes are reachable for the
  entire runtime via `nodes[]` → a perfect analysis borrows every node reference →
  **0 retain/release in the hot loop** → Rust-indices speed.
- **Automatic (measured):** what the solver actually achieves.
- If the spread is large, the risk sits in **inference precision** (and, as it turns
  out, in **collector scaling**).

**Test program** (deliberately adversarial, *not* a sieve/word counter): iterative
PageRank on an object graph — **shared** node references (aliasing), **escaping**
(all nodes live permanently), **mutating** (`rank`/`next` per iteration),
**cycle-capable** (`Node[] out` references `Node`). Comparison: Rust **idiomatic
with indices** (`Vec<Node>` + `usize` — Rust's answer to graphs, **no RC**).

---

## M0.1 — Alias Precision & RC Path (the core risk)

### Runtime, N=16000, 40 iterations
| Variant | Time | vs Rust | vs non-atomic RC |
|---|---|---|---|
| FastLLVM **automatic** (default: `Node` type-cyclic → collector on) | 0.901 s | **108×** | — |
| FastLLVM, collector **off** (`-DFASTLLVM_NO_CYCLES`) | 0.037 s | 4.4× | 1× |
| FastLLVM, collector off, **atomic RC** (`--threads`, uncontended) | 0.233 s | **29×** | **6.3×** |
| **Rust (indices, no RC)** = the oracle speed | 0.008 s | 1× | — |
| JVM (reference) | 0.12 s | — | — |

*(Correction: the atomic RC is **29× vs Rust**; the 6.3× is the pure atomicity
surcharge **against non-atomic RC**. And this is still **uncontended** — the
**contended** number relevant to Feature 1 (multiple threads on the same refcounts)
is worse and is still pending, see M0.1c.)*

### Scaling (default, with collector) — **super-linear**
| N | 2000 | 4000 | 8000 | 16000 | 100000 |
|---|---|---|---|---|---|
| Time | 0.009 s | 0.016 s | 0.118 s | 0.901 s | **Timeout (>60 s)** |

Doubling N → ~7× time (≈ O(n²·⁸)). At N=100000 the default aborts after >60 s; at
N=200000 **stack overflow** (recursion proportional to graph size — runs only under
`ulimit -s unlimited`, then very slowly).

### Diagnosis (honest, not smoothed)
1. **The cycle collector is the killer.** Collector on vs. off: **24× at N=16000**,
   super-linear (→ timeout at 100k). Mechanics: the hot loop leaves releases standing
   (borrow inference does *not* elide fully — 58 release sites in the IR); the nodes
   are **shared** (refcount > 1), so a release does not free, but **buffers a cycle
   candidate**; at threshold the collector scans the **large live set** → O(n) per
   scan × many scans = **O(n²)**.
2. **The spread is enormous.** Oracle = 0 RC / Rust speed (0.008 s). Automatic =
   0.901 s. Automatic inference does **not** recover the "all permanently live →
   borrow" facts for the **shared/cyclic** case. Exactly §7.1.
3. **Even without the collector** a **constant factor of 4.4×** remains (object
   header, scattered heap nodes, RC in setup, bounds checks) — the RC path does not
   match Rust indices even then.
4. **Atomic RC** (threads) costs **6.3× over non-atomic** already *uncontended*
   (0.037 → 0.233 s). Contended (multiple threads on the same refcounts) is worse —
   this is the named Swift-ARC problem, now demonstrated.

### M0.1b — did this graph even need RC? (the decisive question)
M0.1 measures the **RC fallback** — not whether it was **necessary**. The PageRank
builds the graph *once* and changes **no topology** in the hot loop (no ref field, no
array element is reassigned — only `rank`/`next` primitives). The graph therefore
*is* a loop-stable, borrowable region. Test (N=16000, collector off): all
retain/release in the IR removed (= "solver borrows everything"):

| Variant (collector off) | Time | vs Rust |
|---|---|---|
| with RC (actual state) | 0.039 s | 4.4× |
| **without RC (everything borrowed)** | **0.012 s** | **1.48×** |
| Rust (indices) | 0.008 s | 1× |

**Answer: the solver did *not prove* a borrowability that was provable.** The RC
accounts for **3.4× of the 4.4×** and is **elidable** (the information is there: no
topology mutation in the loop). This is the **encouraging** branch of the review
dichotomy: an **inference completeness gap**, not a structural wall.

**And it defuses the collector for free:** the O(n²) collector is triggered *by* the
loop releases (shared nodes → cycle candidates). Without loop releases, **no
candidates are buffered** → the collector does not run in the loop → the 108×
**disappear with the same fix**. So (i) the collector fix and (ii) borrow inference
are **not parallel** — **(ii) alone opens the gate** and makes (i) superfluous for
this case.

**The ceiling is ~1.5×, not 1×.** The remainder after RC elision (1.48×) is the
**object model**: 24-byte header, scattered heap nodes (worse cache locality than
Rust's flat `Vec`), bounds checks. This is the honest "objects instead of flat
arrays" surcharge — narrower via bounds elision/layout, but no free 1×.

### What this means
"Rust level without annotations" is a result on the **escape-friendly** subset (§9).
On the **shared/cyclic** subset it is **today** a slogan (4–108×), but M0.1b shows:
the path to **~1.1–1.5×** is concrete and provable — a borrow inference for
loop-stable regions (build-once, iterate-in-place). This is an **engineering
problem**, not a structural bar — *for this common pattern*. The general case
(topology mutation via aliases in the loop) remains the §7 problem without an
annotation-free general proof.

---

## M0.2 — Compile-Time Scaling (Whole-Program Cost)

Solver + backend (`--emit-llvm`, without clang), synthetic programs:
| LOC | 4 060 | 20 288 | 50 717 |
|---|---|---|---|
| Time | 0.064 s | 0.45 s | 1.81 s |

Super-linear (~O(n^1.4)). Extrapolated to 100k LOC: **~5–7 s for solver+backend
alone**, without clang, **without incremental caching** (whole-program → every build
re-analyzes everything). Exactly §7.3: this undermines "fast iteration like Python"
on larger projects. Not a knockout, but a real ergonomics price — per-function
analysis caching becomes necessary before the language is pleasant beyond toy size.

---

## Side finding — overflow check vs. vectorization (invalidates a claim)

The new decision "overflow checked even in release" ([REFERENCE.md](REFERENCE.md)
§3.1) collides with the AVX2 benchmark. Measured (C, `-O3 -march=native`, the same
arithmetic loop):
| | Time | AVX2 (`paddq`) |
|---|---|---|
| wrapping (unchecked) | 0.072 s | 5 (vectorized) |
| `-ftrapv` (checked) | 0.332 s | 2 (vector path broken) |

**4.6× slower, vectorization gone.** The EVALUATION claim "arithmetic AVX2-vectorized
faster than Rust/C" (0.052 s) held for **wrapping** (Java semantics, like Rust
release). With Vire's checked default it holds **only if hot kernels explicitly use
`+%`/`Wrapping[T]`** — otherwise a silent scalar fallback. Consequence implemented:
the claim now carries an asterisk ([EVALUATION.md](EVALUATION.md) §2,
[REFERENCE.md](REFERENCE.md) §3.1) and the docs state: numeric loops opt out.

---

## M0.1c — Collector fixed (the safe half implemented)

Two **safe** runtime fixes implemented (suite 65/65, 0 live, graph correct — no
borrow logic touched):
1. **Adaptive threshold:** collector trigger = 2× live objects instead of a fixed
   10000 → frequency bounded → amortizes **linearly** instead of O(n²). 0-live
   unaffected (the shutdown collect catches everything).
2. **Iterative drop/collect (SOUNDNESS):** recursive release + the four Bacon-Rajan
   traversals blew the stack at N=200k on a **valid** graph. Now worklists (stack
   depth O(1)). N=200k: **segfault → runs, 0 live.**

| N | before (default) | **after** | vs Rust |
|---|---|---|---|
| 16 000 | 0.90 s | **0.055 s** | 6.7× |
| 100 000 | Timeout >60 s | **0.37 s** | 6.7× |
| 200 000 | **Segfault** | **0.86 s** | 7.4× |

**108× → ~7×, linear, correct, crash-free.** This is the collector half predicted by
the review (~4–7×). The remainder to 1.1× is the borrow inference (M0.1b) — and that
is **blocked by slot reuse** on the javac IR: `Local(3)` is in the same slot as the
`NewArray` owner (setup) **and** the `ArrayLoad` borrow (loop). Per-slot borrow is
thus impossible; it requires **SSA/slot splitting** — exactly what Vire's front-end
delivers natively and the Java bootstrap does not have. **The bootstrap has reached
its optimization ceiling for this class here.**

## M0.3 — Decision

**Gate verdict: conditional go, with two mandatory pieces of preliminary work — not
"green".**

The measurement did exactly what a gate should: it answered the **right** question
negatively before effort was spent. Concretely:

1. **Collector scaling is a blocker for the cyclic case.** The current
   threshold-triggered full scan is O(n²) on large live cycle sets. Needed before the
   front-end: (a) an incremental/generational collector with bounded scan, or (b)
   substantially sharper escape/region inference that removes
   shared-but-acyclically-used structures from the RC/collector path.
2. **Borrow inference must hit the "permanently-live → borrow" case.** The spread
   Oracle↔Automatic is maximal today. This is the investment-worthy spot — not
   lexer/parser.
3. **Overflow default** re-evaluated: either checked default + `+%` culture in
   kernels (documented, asterisk set) — or revise the decision.
4. **Compile-time caching** planned before projects grow.

**What remains confirmed:** the escape-friendly subset is at Rust level (measured,
§9). The safety triangle *per site* is real. But the language lives or dies with the
**shared/cyclic** subset, and there stands a red number today. The honest next step
is **not** the front-end, but improving collector + borrow inference on exactly this
test case and measuring M0.1 again.

*(Open from M0.1: real multithread **contention** — multiple threads on the same
refcounts — as a separate runtime experiment; the 6.3× uncontended is the lower
bound.)*
