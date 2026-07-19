# M0 — risk measurement (programs)

Reproduces the alias-precision measurement from [../../language/M0-MEASUREMENT.md](../../language/M0-MEASUREMENT.md).

- `Graph.java` — an adversarial PageRank object graph: shared/escaping/mutating/
  **cyclic** (`Node[] out` → `Node`). The case that the benchmarks §9 do NOT show.
- `graph_idx.rs` — Rust idiomatically with **indices** (`Vec<Node>` + `usize`, no RC)
  = the oracle pace.
- `run.sh` — builds & measures both.

Core result: FastLLVM (automatic RC + cycle collector) is on this case
**>1000× slower** (the collector super-linear), 4.4× even without the collector, 6.3×
atomic RC. Details, diagnosis (collector O(n²)), and the gate verdict in the report.

**Scope (important):** this is the *deliberately adversarial cyclic* graph. The
common *acyclic* allocation case is a different story and has since improved a lot —
binary-trees is now **~1.05× Rust / 1.3× C++** after region inference (see
[../vire-lang/](../vire-lang/)). The O(n²) collector blow-up here is specific to a
truly-cyclic mutating object graph, which region/shape inference does not (yet)
prove acyclic — that is exactly why this measurement is kept.
