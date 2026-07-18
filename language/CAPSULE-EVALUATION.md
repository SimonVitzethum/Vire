# `capsule(){}` — Evaluation & Design

*Requirement: `capsule(a, b) { … }` — only the variables named in `()` can
go in and out; everything inside lives in its **own virtual RAM**; for important,
risky things. Decision first, then implementation.*

## Verdict: **Yes — integrate it.** And specifically as *the* opt-in lever against the M0 problem.

`capsule` combines three well-known, strong ideas in one construct:
1. **Region/arena memory** — everything allocated in the body lands in a private
   arena that is freed **in one shot** at the end.
2. **Isolation** — the body can only see the `()` inputs; nothing else from
   outside (its own "virtual RAM").
3. **Explicit interface contract** — only `()` in, only the block value out.

This is the **direct, opt-in answer to M0**: the shared/cyclic/mutating
subset was 4–108× slower because RC + the cycle collector fire. Inside a capsule
there is **no RC and no collector** — the arena owns everything and is freed as a
whole. This gives the programmer *exactly* the Rust arena win that
M0.1b measured as the ceiling (~1×) — **without** the manual index discipline that Rust
requires for it. It is the honest compromise: the solver cannot always *prove*
region borrow (M0), but the programmer can **declare** it at the important spot.

## Semantics

```vire
out = capsule(input) {
    // private arena ("virtual RAM"): every allocation here is arena-local.
    // ONLY `input` is visible (copied in); the outer scope is unreachable.
    mut g = build_graph(input)        // nodes → arena, NO RC, NO collector
    for _ in 0..40 { step(g) }        // mutates freely, arena-local
    summary(g)                         // block value → COPIED into the outer heap
}
// arena freed en bloc here (one free); `out` survives.
```

> **Correction after review — the fallacy the first version made:**
> "Only `()` in" does **not** guarantee isolation as long as the inputs carry RC
> references. The body addresses outer memory not via *names*
> but via *the input itself*. Isolation = name visibility is a
> fallacy. That is why the **guaranteed** semantics are the **pure** form:

**Rules of the pure form (what `capsule` GUARANTEES):**
- **In: deep copy, no move, no `&`.** Every `()` input is **deep-copied into the
  arena**. *Not* "copied or moved": a move would only move the *name*;
  an RC graph with refcount>1 would still live outside (aliased) and the
  arena-free body would mutate objects visible outside → exactly the dangling/race
  case that `capsule` is meant to prevent. **Only the deep copy makes the input
  truly region-local.**
- **Isolation + containment follow only from the deep copy** (not from the
  name rule): because the body *owns* **no** outer pointer, a bug/
  OOB/corruption in the body can only hit the arena. That is the actual
  safety promise — and it holds **only** without the `&` exception.
- **Memory:** all `New`/collections in the body → arena bump. No retain/release,
  no cycle collector (cycles die with the arena). Deterministic, leak-free.
- **Out: deep copy of the block value** into the outer heap (the arena dies). No
  pointer into the freed arena — enforced.
- **Panic:** the arena is freed anyway (RAII) — fault containment.

## Cost curve — honest (the "~1× like Rust arena" claim is withdrawn)
The first version sold Rust-arena performance with the safety guarantees of the
expensive form. **You cannot have both together.** The pure form pays **copy-in
*and* copy-out**:
- A Rust arena-of-indices pays **no copy-out** — it returns indices into a
  *surviving* `Vec`. `capsule` pays it by construction (the arena
  dies). Therefore they do **not** have the same cost curve.
- If the block value is the *entire processed graph* (M0 PageRank literally: large
  graph in, large ranks out), copy-in + copy-out potentially eat up the savings
  — **net gain unmeasured, possibly negative**.
- `capsule` wins when **a lot of work on large internal structures results in a
  SMALL result**: aggregation, a scalar, a small report. Parsers/
  deserializers/validators with small output are the sweet spot.
- **Open measurement (M0.2):** measure `capsule` PageRank *with* copy-in+copy-out
  against the Rust arena before any performance promise appears in the document.

## Feasibility on FastLLVM
- **Arena allocator** (`jrt_arena_push/_pop/_alloc`, bump) — small.
- **Reroute allocation** in the body to the active arena; RC ops on arena-local
  objects become no-ops (`immortal` flag, already in the model). Correct *within*
  the arena.
- **Deep-copy-in + deep-copy-out** recursively over `jrt_array_clone`/field copy —
  **this is the real effort, not "~30 lines"**: for cyclic inputs/
  results the copy must detect the cycles (visited set), otherwise infinite loop/
  duplicates; the result must reconstruct RC headers and possibly re-register with the
  collector. Not trivial.
- **Isolation check** (F3): the body sees only the `()` names — a necessary but
  (see review) **not sufficient** condition; sufficiency comes only from the deep
  copy of the inputs.

## Open research questions (do NOT sell as a finished cut)
- **`capsule(&readonly_in)` (copy-free):** breaks isolation + containment head-on —
  a `&` into the outer heap *is* the pointer to the outside that the pure form
  forbids. It requires (a) *strictly* read-only (no storing of **arena**
  pointers into the borrowed structure → escape check arena→outside = the M0 analysis) and
  (b) the guarantee that **nobody outside** mutates/frees the input during the `capsule`
  (XOR rule = the borrow checker that Vire does *not* have; otherwise
  dangling `&`, the same §9a case across the boundary). **Open**, not a finished feature.
- **Move-in without copy** for *provably unaliased* inputs: does not relocate the
  alias problem, it *is* the whole-program alias analysis from M0/§7. Open.
- **Guard-page mode** (`capsule guarded`) for `unsafe`/FFI: hardware containment
  against overflows. Extension stage.

## So what `capsule` GUARANTEES (the firm core)
The **pure** form — deep-copy-in, deep-copy-out, no `&`: **deterministic,
leak-free, RC-/collector-free processing with real fault containment**, ideal
for risky processing with **small output** (parsers, deserializers, plugins,
aggregations). This is a smaller but **watertight** promise — and the
honest answer to M0: the solver does the normal case, `capsule` gives the
human a *safe* tool at the proven inference boundary. The performance gain
over RC is workload-dependent and **yet to be measured** (M0.2), not to be claimed.

## Positioning
Vale has "regions", Rust has arena crates (`bumpalo`) + lifetimes, Zig has explicit
allocators. Vire turns this into a **language construct with an enforced interface** —
the novelty is the combination of *isolation (only `()` )* and *region (own
arena)* in one block, opt-in, without lifetime syntax. It is the declarative
complement to inferred memory management: the solver does the normal case, the
capsule does the *important, risky, hot* case — exactly where inference reaches
its limit according to M0.

**Integration:** keyword `capsule`, adopted into design (LANGUAGE/REFERENCE/PARSER) and
parser; lowering + arena runtime as its own milestone (after the
end-to-end base).
