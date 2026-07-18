# Execution Probability Solver — Evaluation (before building)

*User request: instead of merely estimating hotness, develop an **Execution
Probability Solver** that unifies call graph, dominator tree, loop nesting,
escape analysis, value ranges, type information, and branch heuristics into an
execution probability per block/edge. Instruction: "evaluate first" — the same
gate discipline as with M0 and the AOT hot path: first the number, then the build.*

## The decisive preliminary question
An execution probability is only **valuable** if it drives a
**decision** that (a) LLVM does not already make itself, and (b) changes something
about the measured gap. The state of measurement frames everything:

- **`!prof` branch weights from loop depth (already built):** branch-heavy
  workload 200M iter, with/without = 0.215/0.215 s → **~0%**.
- **Semantic branch heuristic (`cold` on Vire's throw functions — the ONE piece of
  info LLVM does not have from the raw IR), measured today:** pagerank 0.222→0.227 s →
  **~0% to slightly negative** (in the pending-exception model the throw returns,
  `cold` alone does not make the path dead; reverted again).
- **Region inference at the ceiling:** pagerank `normal == --no-rc == Rust` (parity);
  the remaining gap to Rust/C++ is the **allocator** (malloc/node vs bulk/arena),
  not dispatch/branches/RC. (See M0.3 v3.)
- **Compute/traversal = Rust parity** across the benchmarks.

In short: **the direct consumer of a probability (branch weights /
codegen priority) has <5% room on this compiler — measured ~0%,** because
the code the probability would optimize is already optimal.

## Component by component

### 1. Call graph — ALREADY THERE, directly used
The solver has the complete closed-world call graph and operates over it:
RTA/CHA devirtualization, pruning, inliner, `static_writes`, interprocedural
`instance_field_writes` (region inference). A *frequency* propagation on top
(callee inherits call-site hotness) would prioritize inlining/optimization — but
LLVM already inlines aggressively at `-O2` by its own cost model, and the Vire
inliner exists. **Additional benefit: low; the structure is there, the consumer
(inlining) is saturated.**

### 2. Dominator tree — NEW, but the consumer is missing
A dom tree would allow: "blocks reachable only from a cold guard
are cold." That is exactly the semantic branch heuristic — and it measures
~0% (above). LLVM builds dominators internally anyway and does the cold-path layout
itself. **Additional benefit: low; the signal it provides is measured to be worthless.**

### 3. Loop nesting — ALREADY BUILT, measured ~0%
`loop_branch_bias` (reducible CFG, back edges) → `!prof` 100:3 at the loop
exit. Already in the backend, both paths, test present. **Measured ~0%** (LLVM
already lays out loops optimally; the static weights = LLVM's own heuristic).
**Additional benefit: none (already delivered and measured).**

### 4. Escape analysis — ALREADY THERE (`escape.rs`), directly used
Drives stack allocation (not in loops). As a mere *probability*
signal it adds nothing — it is already used for its direct purpose.
**BUT:** here lies the only real lever — see "The valuable subset".

### 5. Value ranges — LLVM does this better on the emitted IR
Constant/range narrowing for branch elimination is LLVM's core competency
(SCCP, CVP, LVI, range metadata). Vire emits per-function IR, LLVM inlines and
then sees the ranges itself. The only theoretical advantage would be *cross-
function* range knowledge that disappears after inlining — a narrow,
hard-to-prove case. **Additional benefit: low; duplicate work with -O2.**

### 6. Type information — ALREADY THERE (mono + CHA devirt + CallPoly)
Monomorphization (one instance per type argument), CHA devirtualization (mono sites
→ direct calls, null check remains), `CallPoly` (2–3 types → guard cascade). That
IS type specialization. **Additional benefit: none (already delivered).**

### 7. Branch heuristics — the ONLY new information, measured ~0%
Vire *knows* which branches are null/bounds/err checks (knowledge that LLVM does not have from
the raw IR). This is the only point with real additional info. Tested
empirically today (`cold` on `jrt_throw_npe/_bounds/_throw`): **~0% to slightly
negative.** Reason: (a) LLVM already treats check-fail blocks that end in a
call as rare; (b) checks in hot loops LLVM hoists/eliminates anyway (the
bounds-heavy array example was optimized away entirely); (c) in the pending model
the throw returns → `cold` does not make the path dead.

## Overall verdict: do NOT build the 7-signal solver
- **4 of 7 signals exist** and are used for their direct purpose
  (call graph, escape, loop nesting, type info).
- **2 of 7 LLVM `-O2` does better** on the emitted IR (value ranges,
  generic branch prediction).
- **1 of 7 carries real additional info** (semantic check branches) — and it measures
  **~0%**.
- The only consumer of a unified probability (branch weights /
  codegen priority) has **<5% ceiling, measured ~0%**, because compute/traversal are already
  Rust parity.

An elegant unified `f64` probability per block would be nice engineering
work — but it would **optimize already-optimal code.** That is exactly the
case the gate discipline rejects (as with the AOT hot path steps 2–4).

## The valuable subset (if building at all)
Within the signal bundle there is ONE lever that hits the **measured** gap — but it is
not the probability, but **loop nesting × escape → allocation
strategy**:

> The only measured remaining gap to Rust/C++ is the **allocator** (malloc-per-
> node). A *loop-nested* `New` that *does not leave its region* (escape signal
> that `escape.rs` already computes) is the candidate for **arena/
> pool allocation** instead of malloc — exactly the axis (binary-trees 2.7×, pagerank-
> build 1.9×) on which Vire is not at parity.

That is a **focused escape→arena pass** (2 signals, concrete consumer =
allocation strategy), NOT a 7-signal probability solver — and coincides
with the project's existing capsule/arena lever (`jrt_arena_push/pop`). It
has a **measurable ceiling** (the malloc-vs-bulk gap: pagerank-build 1.9×,
binary-trees 2.7×), unlike the ~0% of branch probability.

## Recommendation
1. **7-signal EPS: no.** Optimizes already-optimal compute; consumer
   measured ~0%. The signals exist individually or are LLVM duplicate work.
2. **If there is optimization budget: the escape→arena pass** (loop nesting × escape →
   pool allocation of hot, non-escaping `New` sites) — it hits the only
   measured gap (allocator) with a real ceiling (1.9–2.7×), instead of ~0%.
3. **First measure the ceiling of this pass** (manually replace a hot `New` with an
   arena, time against -O2) — the same gate discipline before the pass is built.

*Evidence: `!prof` measurement (AOT-HOTPATH.md), region ceiling (M0.3 v3), `cold` measurement
(this session), benchmark parity (benchmarks/vire-lang/).*
