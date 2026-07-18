# Vire vs Rust vs C++ — Benchmarks (inkl. offizielle CLBG-Programme)

Gematchte Programme, jeweils optimiert (`vire build` = -O2 -flto -march=native;
`rustc -O`; `clang++ -O2 -march=native`), best-of-3, Ausgaben **bit-gleich**
geprüft. `./run.sh` reproduziert.

## Ergebnisse (eine Maschine, best-of-3)
| Bench | Art | Vire | Rust | C++ | V/Rust | V/C++ |
|---|---|---|---|---|---|---|
| arith | Compute-Loop | 0,905 s | 0,892 s | 0,901 s | **1,02×** | **1,00×** |
| fib | Rekursion | 0,076 s | 0,084 s | 0,074 s | **0,91×** | 1,03× |
| struct | Stack-Struct | 0,307 s | 0,291 s | 0,307 s | 1,05× | **1,00×** |
| **mandelbrot** | CLBG, Float-Compute | 0,137 s | 0,140 s | 0,118 s | **0,99×** | 1,17× |
| **binary-trees** | CLBG, Allok/GC | 0,477 s | 0,180 s | 0,139 s | **2,65×** | 3,43× |
| **nsieve** (i64-matched) | CLBG, Array | 0,340 s | 0,334 s | 0,363 s | **1,02×** | **0,94×** |

## Lesart
**Compute-gebunden = Parität.** Scalar-Arithmetik, Rekursion, Stack-Structs und der
CLBG-Klassiker mandelbrot laufen auf C++/Rust-Niveau (0,99–1,05× Rust). Das ist der
Payoff des gemeinsamen LLVM-Backends + Solvers (Bounds-Elision/Inlining/Escape/
Devirt) + Closed-World-`-march=native`. C++ zieht bei mandelbrot 1,17× vor (bessere
Autovektorisierung der inneren Schleife).

**Allokations-/GC-gebunden = der ehrliche Gap.** binary-trees (reine Objekt-
Allokation + Freigabe) ist **2,65× Rust / 3,43× C++** — die Referenzzähl-Steuer:
retain/release je Knoten + Kaskaden-Free, gegen Rusts Ownership (kein Refcount) und
C++ new/delete. **0 live** (Vire gibt alles frei — C++ nur mit explizitem `delete`).
Konsistent mit dem geteilt/zyklischen PageRank (`../vire-m0/`, ~2–4×). Kein
O(n²)-Blowup; Region-Inferenz-v1 (`sprache/M0.3`) hat den RC-Anteil schon gesenkt,
die Orakel-Decke ist Parität (`--no-rc`). Diesen Gap schließt interproz. Region-
Inferenz (die offene schwere Hälfte) — die Compute-Pfade sind schon dort.

## Zusammenfassung
Vire = **C++/Rust-Parität auf compute-gebundenem Code, ~2,7–3,4× auf reiner
Objekt-Allokation** (RC-Steuer, mit bewiesener Decke bei Parität und ohne O(n²)).
