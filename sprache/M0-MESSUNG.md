# M0 — Risiko-Messung (Gate vor dem Front-End)

*Ausführung des Gates aus [BEWERTUNG.md](BEWERTUNG.md) §7 / [../TODO.md](../TODO.md)
M0. Ziel: die **zwei unbelegten Zahlen** messen, bevor Front-End-Code entsteht —
statt sie zu designen. Programme & Rohdaten: [../benchmarks/m0/](../benchmarks/m0/).*

**Kurzfassung:** Das Gate steht auf **Gelb bis Rot**. Der adversariale RC-/zyklen-
lastige Fall ist **nicht** Rust-Niveau — er ist bei realistischer Größe
**>1000× langsamer** (Zyklen-Kollektor super-linear), und selbst ohne Kollektor
4–6×. Das ist genau die Hälfte, die §7 als unbewiesen markiert hat. „Rust-Niveau
ohne Annotationen" gilt weiter für die escape-freundliche Teilmenge — für die
geteilt/zyklische **nicht**. Vor dem Front-End sind zwei Dinge zu klären
(Inferenz-Präzision **und** Kollektor-Skalierung), sonst beantwortet die Sprache
ihr eigenes Kernversprechen auf dem interessanten Code negativ.

---

## Methode — warum das die *richtige* Hälfte misst

Die naive Form („Programm nach `crates/ir` absenken, RC zählen") misst die **falsche
Hälfte**: senkt man von Hand ab, führt man die Alias-Analyse selbst durch — man
misst „elidiert das Backend RC, *wenn die Fakten bekannt sind*", die längst bewiesene
Hälfte. Deshalb hier: **die reale automatische Pipeline** (Java-Front-End → Solver →
IR → Backend) macht die Inferenz — RTA, Escape-Analyse, RC-Elision, Borrow-Slots,
stabile-Statics, refcopy. Gemessen wird also, was **automatische Inferenz ohne
Annotationen** tatsächlich zurückgewinnt. Der **Spread** = Abstand zum Oracle:

- **Oracle (Obergrenze):** Für das Testprogramm sind *alle* Knoten für die gesamte
  Laufzeit über `nodes[]` erreichbar → eine perfekte Analyse borgt jede
  Knoten-Referenz → **0 retain/release im heißen Loop** → Rust-Indizes-Tempo.
- **Automatisch (gemessen):** was der Solver real erreicht.
- Ist der Spread groß, sitzt das Risiko in der **Inferenz-Präzision** (und, wie sich
  zeigt, in der **Kollektor-Skalierung**).

**Testprogramm** (bewusst adversarial, *kein* Sieb/Wortzähler): iterativer PageRank
auf einem Objektgraphen — **geteilte** Knoten-Referenzen (Aliasing), **entkommend**
(alle Knoten leben dauerhaft), **mutierend** (`rank`/`next` je Iteration),
**zyklenfähig** (`Node[] out` referenziert `Node`). Vergleich: Rust **idiomatisch
mit Indizes** (`Vec<Node>` + `usize` — Rusts Antwort auf Graphen, **kein RC**).

---

## M0.1 — Alias-Präzision & RC-Pfad (das Kernrisiko)

### Laufzeit, N=16000, 40 Iterationen
| Variante | Zeit | vs Rust |
|---|---|---|
| FastLLVM **automatisch** (Default: `Node` typ-zyklisch → Kollektor an) | 0,901 s | **108×** |
| FastLLVM, Kollektor **aus** erzwungen (`-DFASTLLVM_NO_CYCLES`) | 0,037 s | 4,4× |
| FastLLVM, Kollektor aus, **atomare RC** (`--threads`, uncontended) | 0,233 s | 28× |
| **Rust (Indizes, kein RC)** = das Oracle-Tempo | 0,008 s | 1× |
| JVM (Referenz) | 0,12 s | — |

### Skalierung (Default, mit Kollektor) — **super-linear**
| N | 2000 | 4000 | 8000 | 16000 | 100000 |
|---|---|---|---|---|---|
| Zeit | 0,009 s | 0,016 s | 0,118 s | 0,901 s | **Timeout (>60 s)** |

Verdopplung von N → ~7× Zeit (≈ O(n²·⁸)). Bei N=100000 bricht der Default nach
>60 s ab; bei N=200000 **Stack-Overflow** (Rekursion proportional zur Graphgröße —
läuft nur unter `ulimit -s unlimited`, dann sehr langsam).

### Diagnose (ehrlich, nicht geglättet)
1. **Der Zyklen-Kollektor ist der Killer.** Kollektor an vs. aus: **24× bei
   N=16000**, super-linear (→ Timeout bei 100k). Mechanik: der heiße Loop lässt
   Releases stehen (die Borrow-Inferenz elidiert *nicht* vollständig — 58
   release-Stellen im IR); die Knoten sind **geteilt** (refcount > 1), also gibt ein
   Release nicht frei, sondern **puffert einen Zyklen-Kandidaten**; bei Schwelle
   scannt der Kollektor die **große lebende Menge** → O(n) je Scan × viele Scans =
   **O(n²)**.
2. **Der Spread ist enorm.** Oracle = 0 RC / Rust-Tempo (0,008 s). Automatisch =
   0,901 s. Die automatische Inferenz gewinnt die „alles dauerhaft lebendig → borgen"-
   Fakten für den **geteilt/zyklischen** Fall **nicht** zurück. Genau §7.1.
3. **Selbst ohne Kollektor** bleibt ein **Konstantfaktor 4,4×** (Objekt-Header,
   verstreute Heap-Knoten, RC im Setup, Bounds-Checks) — der RC-Pfad matcht Rust-
   Indizes auch dann nicht.
4. **Atomare RC** (Threads) kostet **6,3× gegenüber nicht-atomarer** schon
   *uncontended* (0,037 → 0,233 s). Contended (mehrere Threads auf denselben
   Refcounts) ist schlechter — das ist das benannte Swift-ARC-Problem, jetzt belegt.

### Was das bedeutet
Das Kernversprechen „Rust-Niveau ohne Annotationen" ist auf der **escape-
freundlichen** Teilmenge ein Ergebnis (Benchmarks §9: Alloc/Sieve/… schlagen `Box`),
auf der **geteilt/zyklischen** Teilmenge ein **Slogan** — dort 4× (Konstant),
6× (atomar), 100–1000×+ (Kollektor). Ein Objektgraph ist *der* Normalfall
„idiomatischer" Anwendungslogik; die Sprache kann ihn nicht dem Nutzer verbieten.

---

## M0.2 — Compile-Zeit-Skalierung (Whole-Program-Kosten)

Solver + Backend (`--emit-llvm`, ohne clang), synthetische Programme:
| LOC | 4 060 | 20 288 | 50 717 |
|---|---|---|---|
| Zeit | 0,064 s | 0,45 s | 1,81 s |

Super-linear (~O(n^1,4)). Extrapoliert auf 100k LOC: **~5–7 s allein für
Solver+Backend**, ohne clang, **ohne inkrementelles Caching** (Whole-Program → jeder
Build reanalysiert alles). Genau §7.3: das untergräbt „schnelle Iteration wie
Python" bei größeren Projekten. Kein K.o., aber ein realer Ergonomie-Preis —
Analyse-Caching pro Funktion wird nötig, bevor die Sprache über Spielzeuggröße
hinaus angenehm ist.

---

## Nebenbefund — Overflow-Check vs. Vektorisierung (invalidiert eine Aussage)

Die neue Entscheidung „Overflow geprüft auch in Release" ([REFERENZ.md](REFERENZ.md)
§3.1) kollidiert mit dem AVX2-Benchmark. Gemessen (C, `-O3 -march=native`, dieselbe
Arithmetik-Schleife):
| | Zeit | AVX2 (`paddq`) |
|---|---|---|
| wrapping (ungeprüft) | 0,072 s | 5 (vektorisiert) |
| `-ftrapv` (geprüft) | 0,332 s | 2 (Vektorpfad gebrochen) |

**4,6× langsamer, Vektorisierung weg.** Die BEWERTUNG-Aussage „Arithmetik AVX2-
vektorisiert schneller als Rust/C" (0,052 s) galt für **wrapping** (Java-Semantik,
wie Rust-Release). Mit Vires checked-Default gilt sie **nur, wenn heiße Kernels
explizit `+%`/`Wrapping[T]` nutzen** — sonst stiller Skalar-Fallback. Konsequenz
umgesetzt: die Aussage trägt jetzt ein Sternchen ([BEWERTUNG.md](BEWERTUNG.md) §2,
[REFERENZ.md](REFERENZ.md) §3.1) und die Doku sagt: numerische Loops opten aus.

---

## M0.3 — Entscheidung

**Gate-Urteil: bedingtes Weiter, mit zwei Pflicht-Vorarbeiten — nicht „grün".**

Die Messung hat genau das getan, was ein Gate soll: die **richtige** Frage negativ
beantwortet, bevor Aufwand entstand. Konkret:

1. **Kollektor-Skalierung ist ein Blocker für den zyklischen Fall.** Der aktuelle
   schwellen-getriggerte Full-Scan ist O(n²) auf großen lebenden Zyklen-Mengen. Vor
   dem Front-End nötig: (a) inkrementeller/generationeller Kollektor mit
   beschränktem Scan, oder (b) deutlich schärfere Escape-/Region-Inferenz, die
   geteilte-aber-azyklisch-genutzte Strukturen aus dem RC/Kollektor-Pfad nimmt.
2. **Borrow-Inferenz muss den „dauerhaft-lebendig → borgen"-Fall treffen.** Der
   Spread Oracle↔Automatisch ist heute maximal. Das ist die investitionswürdige
   Stelle — nicht Lexer/Parser.
3. **Overflow-Default** neu bewertet: entweder checked-Default + `+%`-Kultur in
   Kernels (dokumentiert, Sternchen gesetzt) — oder die Entscheidung revidieren.
4. **Compile-Zeit-Caching** einplanen, bevor Projekte wachsen.

**Was bestätigt bleibt:** die escape-freundliche Teilmenge ist Rust-Niveau (gemessen,
§9). Das Sicherheitsdreieck-*pro-Stelle* ist real. Aber die Sprache lebt oder stirbt
mit der **geteilt/zyklischen** Teilmenge, und dort steht heute eine rote Zahl. Der
ehrliche nächste Schritt ist **nicht** das Front-End, sondern Kollektor +
Borrow-Inferenz auf genau diesem Testfall zu verbessern und M0.1 erneut zu messen.

*(Offen aus M0.1: echte Multithread-**Contention** — mehrere Threads auf denselben
Refcounts — als separater Laufzeit-Versuch; die 6,3× uncontended sind die
Untergrenze.)*
