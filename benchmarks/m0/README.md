# M0 — Risiko-Messung (Programme)

Reproduziert die Alias-Präzisions-Messung aus [../../sprache/M0-MESSUNG.md](../../sprache/M0-MESSUNG.md).

- `Graph.java` — adversarialer PageRank-Objektgraph: geteilt/entkommend/mutierend/
  **zyklisch** (`Node[] out` → `Node`). Der Fall, den die Benchmarks §9 NICHT zeigen.
- `graph_idx.rs` — Rust idiomatisch mit **Indizes** (`Vec<Node>` + `usize`, kein RC)
  = das Oracle-Tempo.
- `run.sh` — baut & misst beide.

Kernergebnis: FastLLVM (automatische RC + Zyklen-Kollektor) ist auf diesem Fall
**>1000× langsamer** (Kollektor super-linear), 4,4× selbst ohne Kollektor, 6,3×
atomare RC. Details, Diagnose (Kollektor O(n²)) und Gate-Urteil im Bericht.
