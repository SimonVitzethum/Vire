# Benchmark-Suite: Vire vs Rust vs C++ (clang++)

`./run.sh` — baut jede Benchmark in allen drei Sprachen (`vire build`, `rustc -O
-C target-cpu=native`, `clang++ -O2 -march=native`), misst best-of-5 und prüft
Output-Gleichheit. C++ = **clang++** (LLVM, wie Vire) für einen fairen Codegen-
Vergleich (g++/GCC weicht separat ab, s. REKURSION-INLINING.md).

## Ergebnisse (best-of-5, dieselbe Maschine)
| Benchmark | Vire | Rust | clang++ | Vire/clang |
|---|---|---|---|---|
| bitmanip (popcount) | 0,187 | 0,186 | 0,186 | **1,00×** |
| matmul (256³ naiv) | 0,012 | 0,010 | 0,013 | **0,97×** |
| nbody (2000, 20 Steps) | 0,073 | 0,072 | 0,076 | **0,95×** |
| montecarlo (20M, LCG) | 0,039 | 0,039 | 0,040 | **0,98×** |
| vcall (dyn Dispatch, 100M) | 0,244 | 0,116 | 0,273 | **0,89×** |
| sort (quicksort 2M) | 0,170 | 0,122 | 0,111 | 1,52× |
| binsearch (10M Lookups) | 0,561 | 0,481 | 0,455 | 1,23× |

## Deutung
- **Compute (bitmanip/matmul/nbody/montecarlo): Vire = clang-Parität, teils schneller**
  (matmul/nbody 0,95–0,97×). Beide über LLVM → dasselbe Codegen-Optimum.
- **vcall = Trait-Objekte (dyn Dispatch): Vire 0,89× — SCHNELLER als C++ `virtual`.**
  Vires Vtable-Dispatch (diese Session gebaut) ist so schnell wie C++, hier sogar
  etwas schneller. Rusts `dyn` ist nochmals schneller (0,116) — Rust devirtualisiert
  den monomorphen Aufruf im Benchmark teilweise.
- **Array-index-lastig (sort/binsearch): Vire 1,2–1,5× langsamer.** Der Grund sind
  **Bounds-Checks** auf jedem Array-Zugriff — der Solver (`elide_bounds`) entfernt
  viele, aber nicht die daten-abhängigen (quicksort-Partition, Binärsuche-mid). Das
  ist der klare, ehrliche Optimierungspunkt (Rust hat dasselbe Prinzip, elidiert
  aber mehr; C++ hat gar keine Checks). Der nächste Perf-Hebel für Vire.
- **DIFFs in der Tabelle** sind reine Float-Formatierung (Vire/C++ `%g` wissenschaftlich
  vs Rusts volle Präzision) bzw. Summierungs-Rundung (nbody) — identische Werte.

## Kategorie-Abdeckung (ehrlich)
Von den ~80 Kategorien der Wunschliste laufen die **compute-, speicher-, daten-
struktur-, algorithmen- und numerik-gebundenen** — die hier gemessenen decken
Mikrobenchmarks (Arith/Bit/Rekursion/Virtual-Calls/Closures/Generics — s. auch
`../vire-lang/`), Numerik (Matmul/N-Body/Monte-Carlo), Algorithmen (Sort/Suche) und
Speicher (Arena/RC/Heap — s. RAM-REDUKTION.md, ESCAPE-ARENA.md) ab.

**NICHT abgedeckt (brauchen Libraries/Features, die Vire noch nicht hat):**
- **Textverarbeitung** (Regex, JSON, XML, CSV, YAML, TOML, HTML, Markdown) — braucht
  eine String-/Parser-Bibliothek.
- **Kryptographie** (AES, SHA, BLAKE3, RSA, ECC, Argon2) — braucht Krypto-Lib
  (oder Byte-Arrays + Bit-Ops; `ArrKind::Byte` fehlt noch).
- **Parallelität** (Threadpool, Work-Stealing, Channels, Lock-Free, Parallel-Sort) —
  Vire hat nur den Java-`--threads`-Pfad (pthreads), keine High-Level-Nebenläufigkeit.
- **I/O** (Dateisystem, mmap, TCP/UDP/HTTP/WebSocket) — braucht IO-/Netzwerk-Bibliothek.
- **Komplexe Datenstrukturen** (B-Bäume, AVL/RB, Prio-Queue) — mangels typisierter
  Collections (`List[T]`) und Array-als-Parameter (s.u.) nur eingeschränkt.

## Bekannte Vire-Limitationen, die die Benchmarks berührt haben
- **Array als Funktionsparameter** (`fn f(a: Ref)` + `a[i]`) → „kein bekanntes Array":
  Ref-Params tragen keine ArrKind. sort wurde deshalb iterativ-in-main geschrieben
  (Array bleibt lokal). Eine `Array[T]`-Param-Annotation wäre der Fix.
- **`else` muss auf derselben Zeile wie `}`** stehen (Newline-terminierte Syntax).
