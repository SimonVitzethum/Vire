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
