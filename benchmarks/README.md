# FastLLVM-Benchmarks

Aussagekräftige Benchmarks über mehrere Bereiche, jeweils in **Java** (→ FastLLVM),
**Rust** und **C++** (`g++ -O3 -march=native`), bit-gleiche Ausgaben. Runner:
`./run.sh` (Umgebungsvariable `N` = Wiederholungen, bestes Ergebnis zählt).

FastLLVM baut mit `-march=native` (Closed-World-AOT auf der Zielmaschine).

## Bereiche

| Benchmark | Bereich | Belastet |
|---|---|---|
| **Arith** | reine Ganzzahl-Arithmetik | ALU-Durchsatz, Vektorisierung |
| **Alloc** | Loop-lokale Objekte | Escape-Analyse, RC-Elision |
| **Fib** | tiefe Rekursion | Call-Overhead |
| **Sieve** | `boolean[]`, gezählte Schleifen | Bounds-Elision, Speicherbandbreite |
| **Poly** | virtuelle Dispatches über Array | Devirt, Ref-Array-Zugriff |
| **Matmul** | 512³ Matrixmultiplikation | FP-Durchsatz, Cache, affine Indizes |
| **Mandel** | Mandelbrot 4000² | FP-Compute, vektorisierbar |
| **Quick** | 20M-Element Quicksort | Verzweigung, In-Place-Array, Bounds |
| **NBody** | 20M Schritte, statische Arrays | FP + `sqrt` + Feld-/Array-Zugriff |
| **Trees** | binary-trees (Alloc/Dealloc) | RC + Zyklen-Collector-Durchsatz |

## Ergebnisse (Stand dieser Session, best of 3–7, vs Rust / vs C++)

| Benchmark | vs Rust | vs C++ | Anmerkung |
|---|---|---|---|
| Arith  | **0,42×** | **0,74×** | AVX2 schlägt beide |
| Alloc  | **~0×**   | **0,86×** | Stack-Allok. + RC-frei |
| Fib    | **0,85×** | 1,78× | schlägt Rust; C++ Rekursions-Codegen |
| Sieve  | **~1,0×** | **1,05×** | Parität |
| Poly   | **0,97×** | 2,61× | schlägt Rust; C++ konstant-faltet |
| Mandel | **1,00×** | 1,06× | Parität |
| Quick  | **1,03×** | **0,82×** | Parität Rust, schlägt C++ |
| Matmul | 6,6×  | 9,0× | **offen** — affine Index-Bounds |
| NBody  | 39×   | 40× | **offen** — interproz. Array-Länge |
| Trees  | 3,2×  | 3,6× | **offen** — Zyklen-Collector auf Baum |

**7 von 10 auf/über Rust-Parität.** Drei offene Bereiche, alle mit klar
benanntem, substanziellem Analyse-Bedarf:

### Matmul (6,6×) — affine Index-Bounds-Elision
Der innere Zugriff `C[i*n+j]` hat einen **affinen Index** `i*n + j`. Die heutige
GVN-Bounds-Elision beweist gezählte Schleifen (`arr[i]`, `i < len`) und
And-Masken, aber nicht `i*n + j < n*n`. Nötig: eine flusssensitive **obere-
Schranken-Analyse** (Intervall, nur Obergrenzen), die aus den Wächtern `i<n`,
`j<n` und `len=n²` die Schranke `(n-1)·n + (n-1) < n²` herleitet und über
`Mul`/`Add` propagiert. Erst dann sind die Zugriffe throw-frei → die
pending-Prüfungen fallen weg → LLVM vektorisiert die FMA-Schleife (wie Rust/C++).
Solange die Prüfung bleibt, blockiert der pending-Check die Vektorisierung.

### NBody (39×) — interprozedurale/statische Array-Länge
Die Arrays sind **statische Felder**, in `main` erzeugt, in `advance()` benutzt.
Zwei Teil-Fixes dieser Session griffen bereits:
- **RC-auf-stabilen-Statics eliminiert** (72×→39×): ein statisches Feld, das eine
  Funktion + Callees nicht schreibt, ist während ihrer Ausführung konstant →
  `GetStatic` liefert eine stabile, von der Static-Wurzel gehaltene Referenz und
  braucht kein retain/release (war zuvor 66 RC-Ops je `advance`).
- **Inline-geprüfter Array-Zugriff**: Zugriffe sind jetzt sichtbare `load`/`store`
  (hoistbar) statt opaker `jrt_daload`-Calls.
Es bleibt: die **Länge** der statischen Arrays ist in `advance` unbekannt (kein
`NewArray` dort) → Bounds nicht elidierbar → pending-Prüfungen bleiben. Nötig:
statische Array-Längen whole-program verfolgen (`static T[] f = new T[k]` ⇒ Länge
`k`) **plus** die Schleifenschranke `nb` als interprozedurale Konstante.

### Trees (3,2×) — Zyklen-Collector auf azyklischen Bäumen
`Node` referenziert `Node` → der Typ-Referenzgraph ist zyklisch → die
(konservative, typbasierte) Azyklizitäts-Analyse behält den Zyklen-Collector, der
je decref Kandidaten puffert. Der Baum ist real azyklisch. Nötig: eine
**Struktur-/Shape-Analyse** (oder Region/Ownership-Inferenz), die Baum-förmige
Allokationsmuster als azyklisch beweist — dann entfällt der Collector (wie schon
heute für typ-azyklische Programme) und die Allokation läuft RC-schlank.

## Gemeinsamer Nenner der offenen Fälle
Alle drei brauchen **stärkere statische Beweise** (affine Intervalle,
interprozedurale Konstanten/Längen, Shape-Analyse), damit Sicherheits-Checks
und RC-Buchhaltung entfallen. Die *Infrastruktur* dafür (GVN, Escape, Azyklizität,
pending-Elision) steht; es sind gezielte Erweiterungen, keine Neubauten.
