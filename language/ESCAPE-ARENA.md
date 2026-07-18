# Escape→Arena — automatic loop arena (built, measured)

*Result of the EPS assessment (`EPS-EVALUATION.md`): the 7-signal probability solver
is not worth it (~0%), but the subset **loop nesting × escape → arena/pool** hits the
only measured gap — the allocator. This pass builds exactly that.*

## The ceiling (measured first, gate discipline)
The only non-parity gap to Rust/C++ is the allocator: the hosted runtime allocates
`calloc`+`free` per node. Manual measurement with the existing capsule arena (bump
allocation, en-bloc release) on binary-trees:

| binary-trees | Time | vs normal |
|---|---|---|
| normal (calloc/free per node) | 0.49 s | — |
| capsule arena (bump, en-bloc) | 0.19 s | **2.57×** |

→ real, large ceiling (in contrast to the ~0% of branch probability).
**Worth it → build.**

## What the pass does
In Vire lowering, a `while` loop whose allocations provably **do not escape** the
iteration is automatically placed into a **per-iteration bump arena**
(`jrt_arena_push` at the body start, `jrt_arena_pop` at the body end) — an automatic
capsule. Objects in the arena body are immortal (no RC/collector), the memory is freed
en bloc at the iteration end. No `malloc`/`free` per node.

## Soundness conditions (conservative — any uncertainty ⇒ do not promote)
A `while` iteration is only promoted if its body (transitively over user callees):
- **allocates** (otherwise no benefit),
- **writes no field/index** (`x.f = …` / `a[i] = …`) — mutation of an existing
  object could store an arena ref to the outside. Constructors (`Node(a, b)`) do NOT
  count as a field write — they create fresh objects,
- **does not re-bind an outer (declared before the loop) variable with a ref**
  (`head = Node(head, i)` — a Let of an outer ref in the Vire AST → escapes),
- **contains no `return`/`break`/`continue`** at body level (would leave the arena),
- **calls only user functions + constructors** — no extern/builtin/lambda/
  comprehension/MapLit/capsule (could capture/store a ref outside).

Body-created ref locals are nulled BEFORE the pop (otherwise the function-end release
`jrt_release` would read freed arena memory → use-after-free), analogous to the
explicit capsule.

## Result (measured, best-of-9, `-O2 -march=native`)
| binary-trees | Time | |
|---|---|---|
| Vire normal | 0.49 s | 2.4× Rust |
| **Vire auto-arena** | **0.202 s** | **Rust parity (0.99×)** |
| Rust (`Box`) | 0.205 s | |
| C++ (`new`, leaks) | 0.136 s | |

→ The pass closes the allocator gap on the allocation-heavy benchmark
**automatically** (without capsule annotation) and brings Vire to **Rust parity**.

## Validation (soundness)
- **btree**: promoted, correct (7864260), 2.4× faster, no leaks.
- **List build** (`head = Node(head, i)`, used afterwards): correctly NOT promoted,
  terminates, correct result (4999950000) — the escape is detected.
- **Callee escape** (`head = attach(head, i)`, attach returns a fresh Node):
  correctly NOT promoted (1249975000).
- **Array store**: index assign → not promoted, correct.
- **Java regression 65/65** (heap balance 0-live = soundness oracle), **Vire suite
  green** (35 lower tests incl. `auto_arena_promotes_*` / `auto_arena_avoids_*`).

## Limits (honest)
- Only `while` loops (not `for` — the iteration variable is an outer element ref, RC
  interaction; later step).
- Conservative: a builtin call (`print`, `str`) in the body blocks the promotion
  (could theoretically capture) — an allowlist of pure builtins would be a
  refinement.
- Per-iteration `arena_push`/`pop` (malloc of the arena head per iteration): with very
  many iterations and little allocation per iteration, an arena_RESET (keep memory,
  only `used=0`) could amortize — not needed for the measured cases, later
  optimization.
- The remaining distance to C++ (leak-`new`, 0.136) is deliberately not a target: C++
  leaks here (no `delete`), Vire frees correctly.
