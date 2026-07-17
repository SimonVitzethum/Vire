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
| Variante | Zeit | vs Rust | vs nicht-atom. RC |
|---|---|---|---|
| FastLLVM **automatisch** (Default: `Node` typ-zyklisch → Kollektor an) | 0,901 s | **108×** | — |
| FastLLVM, Kollektor **aus** (`-DFASTLLVM_NO_CYCLES`) | 0,037 s | 4,4× | 1× |
| FastLLVM, Kollektor aus, **atomare RC** (`--threads`, uncontended) | 0,233 s | **29×** | **6,3×** |
| **Rust (Indizes, kein RC)** = das Oracle-Tempo | 0,008 s | 1× | — |
| JVM (Referenz) | 0,12 s | — | — |

*(Korrektur: die atomare RC ist **29× vs Rust**; die 6,3× sind der reine
Atomik-Aufpreis **gegen nicht-atomare RC**. Und das ist noch **uncontended** — die
für Feature 1 relevante **contended** Zahl (mehrere Threads auf denselben Refcounts)
ist schlechter und steht noch aus, s. M0.1c.)*

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

### M0.1b — brauchte dieser Graph überhaupt RC? (die entscheidende Frage)
M0.1 misst den **RC-Fallback** — nicht, ob er **nötig** war. Der PageRank baut den
Graphen *einmal* und ändert im heißen Loop **keine Topologie** (kein Ref-Feld, kein
Array-Element wird umgesetzt — nur `rank`/`next`-Primitive). Der Graph *ist* also
eine loop-stabile, borgbare Region. Test (N=16000, Kollektor aus): alle
retain/release im IR entfernt (= „Solver borgt alles"):

| Variante (Kollektor aus) | Zeit | vs Rust |
|---|---|---|
| mit RC (Ist-Zustand) | 0,039 s | 4,4× |
| **ohne RC (alles geborgt)** | **0,012 s** | **1,48×** |
| Rust (Indizes) | 0,008 s | 1× |

**Antwort: der Solver hat eine Borgbarkeit *nicht bewiesen*, die beweisbar war.**
Die RC macht **3,4× der 4,4×** aus und ist **elidierbar** (die Info ist da: keine
Topologie-Mutation im Loop). Das ist die **ermutigende** Verzweigung der
Review-Dichotomie: eine **Inferenz-Vollständigkeitslücke**, keine strukturelle Wand.

**Und es entschärft den Kollektor gratis:** der O(n²)-Kollektor wird *durch* die
Loop-Releases getriggert (geteilte Knoten → Zyklen-Kandidaten). Ohne Loop-Releases
werden **keine Kandidaten gepuffert** → der Kollektor läuft im Loop nicht → die 108×
**verschwinden mit derselben Reparatur**. (i) Kollektor-Fix und (ii) Borrow-Inferenz
sind also **nicht parallel** — **(ii) allein öffnet das Gate** und macht (i) für
diesen Fall überflüssig.

**Die Decke ist ~1,5×, nicht 1×.** Der Rest nach RC-Elision (1,48×) ist das
**Objektmodell**: 24-Byte-Header, verstreute Heap-Knoten (schlechtere Cache-Lage als
Rusts flaches `Vec`), Bounds-Checks. Das ist der ehrliche „Objekte statt flacher
Arrays"-Aufpreis — schmaler über Bounds-Elision/Layout, aber kein Gratis-1×.

### Was das bedeutet
„Rust-Niveau ohne Annotationen" ist auf der **escape-freundlichen** Teilmenge ein
Ergebnis (§9). Auf der **geteilt/zyklischen** Teilmenge ist es **heute** ein Slogan
(4–108×), aber M0.1b zeigt: der Weg auf **~1,1–1,5×** ist konkret und beweisbar —
eine Borrow-Inferenz für loop-stabile Regionen (build-once, iterate-in-place). Das
ist ein **Engineering-Problem**, kein struktureller Riegel — *für dieses häufige
Muster*. Der Allgemeinfall (Topologie-Mutation über Aliase im Loop) bleibt das
§7-Problem ohne annotationsfreien Allgemein-Beweis.

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

## M0.1c — Kollektor repariert (die sichere Hälfte umgesetzt)

Zwei **sichere** Runtime-Fixes umgesetzt (Suite 65/65, 0 live, Graph korrekt —
keine Borrow-Logik angetastet):
1. **Adaptive Schwelle:** Kollektor-Trigger = 2× lebende Objekte statt fix 10000 →
   Häufigkeit begrenzt → amortisiert **linear** statt O(n²). 0-live unberührt (der
   Shutdown-Collect fängt alles).
2. **Iterativer Drop/Collect (SOUNDNESS):** rekursives Release + die vier
   Bacon-Rajan-Traversierungen sprengten bei N=200k den Stack auf **gültigem**
   Graphen. Jetzt Worklists (Stacktiefe O(1)). N=200k: **Segfault → läuft, 0 live.**

| N | vorher (Default) | **nachher** | vs Rust |
|---|---|---|---|
| 16 000 | 0,90 s | **0,055 s** | 6,7× |
| 100 000 | Timeout >60 s | **0,37 s** | 6,7× |
| 200 000 | **Segfault** | **0,86 s** | 7,4× |

**108× → ~7×, linear, korrekt, crash-frei.** Das ist die vom Review vorhergesagte
Kollektor-Hälfte (~4–7×). Der Rest auf 1,1× ist die Borrow-Inferenz (M0.1b) — und
die ist auf dem javac-IR durch **Slot-Reuse blockiert**: `Local(3)` ist im selben
Slot der `NewArray`-Owner (setup) **und** der `ArrayLoad`-Borrow (Loop). Per-Slot-
Borrow ist damit unmöglich; es braucht **SSA/Slot-Splitting** — genau was Vires
Front-End nativ liefert und der Java-Bootstrap nicht hat. **Der Bootstrap hat hier
seine Optimierungsdecke für diese Klasse erreicht.**

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
