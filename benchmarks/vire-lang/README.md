# Vire vs Rust vs C++ — Benchmarks

Gematchte Programme, jeweils optimiert (`vire build` = -O2 -flto -march=native;
`rustc -O`; `clang++ -O2 -march=native`), best-of-3, Ausgaben bit-gleich geprüft.
`./run.sh` reproduziert.

## Ergebnisse (eine Maschine, best-of-3)
| Bench | Vire | Rust | C++ | V/Rust | V/C++ |
|---|---|---|---|---|---|
| arith (modularer Compute-Loop, 3·10⁸) | 0,905 s | 0,892 s | 0,901 s | **1,02×** | **1,00×** |
| fib (Rekursion, fib 38) | 0,076 s | 0,084 s | 0,074 s | **0,91×** | **1,03×** |
| struct (Stack-Struct + Feldzugriff, 10⁸) | 0,307 s | 0,291 s | 0,307 s | **1,05×** | **1,00×** |

**Scalar-Compute, Rekursion und Stack-Structs sind auf C++/Rust-Niveau (Parität).**
Das ist der Payoff des gemeinsamen LLVM-Backends + Solvers (RTA/CHA/Bounds-Elision/
Inlining/Escape-Analyse) + Closed-World-`-march=native`.

## Der ehrliche Gap: geteilt-veränderliche Objektgraphen (RC)
Der eine gemessene Nicht-Paritäts-Fall ist der geteilt/zyklische Objektgraph mit
Referenzzählung (PageRank, `../vire-m0/`): **~2× Rust (RC-only), ~4× mit
Zyklen-Kollektor** — kein O(n²)-Blowup (`sprache/M0.1c`). Region-Inferenz-v1
(`sprache/M0.3`) hat den Kollektor-Fall 4,5×→2,0× gebracht; die Orakel-Decke ist
Parität (`--no-rc` → 1,1×). Die verbleibende Lücke schließt interprozedurale
Region-Inferenz (die offene schwere Hälfte) — nicht die skalaren Pfade, die sind
schon dort.
