# AOT-Hotpath-Optimizer — Plan (JIT-Pfade statisch finden + optimieren)

*Ziel (Nutzerwunsch): ein AOT-Compiler, der im Solver die Pfade findet, die ein JIT
zur Laufzeit als heiß entdecken würde, und sie dann JIT-artig aggressiv optimiert —
ohne Warmup, ohne Laufzeit-Profiling, ohne JIT-Overhead. Passt zu Vires
Closed-World-AOT-Modell.*

## Die Kernidee
Ein JIT gewinnt, weil er **heiße Pfade** kennt (Profiling) und sie **spekulativ
spezialisiert** (auf beobachtete Typen/Werte). Ein AOT-Compiler hat kein
Laufzeitprofil — aber im Closed-World-Modell kann er Hotness **statisch schätzen**
und dieselben Optimierungen **vorab** anwenden. Ergebnis: JIT-Peak-Performance mit
AOT-Determinismus (kein Warmup, keine Deopt, kein Codecache).

## Pipeline (fünf neue/erweiterte Solver-Pässe)

### 1. Statische Hotness-Schätzung (`solver/hotness.rs`, NEU)
Schätzt Ausführungshäufigkeit je Funktion/Block/Call-Site OHNE Ausführung — die
Heuristiken, die auch baseline-JITs benutzen, bevor echte Zähler da sind:
- **Schleifentiefe:** Blöcke in Schleifen ×10 je Verschachtelungsebene (klassische
  Frequenz-Schätzung). Rückwärtskanten = Schleifen (Dominatoren-Analyse).
- **Verzweigungs-Heuristiken:** Rückwärts-Branches „taken", Null-/Fehler-Zweige
  „not taken", `?`/Err-Pfade kalt.
- **Call-Frequenz-Propagation:** ein Callee erbt die Hotness der Call-Site
  (Loop-lokaler Aufruf = heiß); über den Call-Graph propagiert (Fixpunkt).
- **Rekursion = heiß** (self-/mutual-rekursive SCCs im Call-Graph).
- Ergebnis: `f64`-Score je Funktion/Block → Klassen `Hot`/`Warm`/`Cold`.

### 2. Hot-Path-Identifikation (die „JIT-Entdeckung", statisch)
Funktionen/Blöcke über Schwelle = das, was ein JIT nach N Aufrufen kompiliert
hätte. Zusätzlich **Superblöcke** bilden: heiße Call-Ketten (A→B→C alle heiß) zu
einer optimierbaren Region zusammenfassen — das AOT-Äquivalent zu JIT-Traces.

### 3. Tiered-Optimierungs-Budget (erweitert `inline.rs`)
Wie JIT-Tiers (Interpreter → Baseline → Optimizing), aber statisch entschieden:
- **Hot:** aggressiv — großes Inline-Budget (auch große heiße Callees inlinen),
  Loop-Unrolling, Scalar-Replacement, volle Spezialisierung. Optimiert für Speed.
- **Warm:** moderat (heutiges Standard-Inlining).
- **Cold:** minimal — für Größe optimieren, nicht inlinen (kleinerer Icache-Druck,
  wie ein JIT kalten Code im Interpreter lässt).

### 4. Spekulative Spezialisierung (`solver/specialize.rs`, NEU) — der JIT-Kern
Ein JIT spezialisiert auf beobachtete Typen/Werte. AOT-Analoga, closed-world beweisbar:
- **Wert-Spezialisierung / Partielle Evaluation:** heiße Funktion, an heißen
  Call-Sites mit konstantem Argument gerufen → spezialisierte Kopie `f$const`,
  Konstante eingefaltet (Branches eliminiert, Loops evtl. entrollt). = das, was ein
  JIT via Constant-Feedback macht, hier statisch bewiesen.
- **Typ-Spezialisierung:** heiße monomorphe/CHA-devirtualisierte Sites → direkte,
  inlinebare Aufrufe (Solver kann das schon; hier gezielt auf heiße Sites).
- **Guard-Elision auf heißen Pfaden:** null-/Bounds-/pending-Checks, die der Solver
  beweisbar redundant zeigt, zuerst auf heißen Pfaden entfernen (heute schon da,
  aber hotness-priorisiert).

### 5. Layout/Codegen-Hints (`backend`)
- Heiße Funktionen `alwaysinline`/`hot`-Attribut an LLVM; kalte `cold`/`minsize`.
- Heiße Basisblöcke zusammen anordnen (Block-Layout nach Hotness) → Icache/BTB.
- LLVM-`!prof`-Branch-Weights aus der statischen Schätzung setzen (LLVM optimiert
  dann selbst hotness-bewusst — der billigste große Hebel, da LLVM den Rest macht).

## Was der Solver SCHON hat (Fundament steht)
RTA/CHA-Devirtualisierung, Pruning, Inliner (`inline.rs`), Escape-/Stack-Analyse,
Bounds-/Pending-/Longcmp-Elision, **Monomorphisierung** (= Typ-Spezialisierung für
Generics), interproz. Region-Inferenz. Der AOT-Hotpath-Optimizer ist primär: (a)
**Hotness-Schätzung** oben drauf, (b) diese Pässe **hotness-priorisiert** statt
uniform, (c) **partielle Evaluation** als neuer Pass, (d) **LLVM-`!prof`-Weights**
als billigster Multiplikator.

## Reihenfolge / Aufwand
1. **`!prof`-Branch-Weights aus Schleifentiefe** — klein, großer Hebel (LLVM macht
   den Rest). *Zuerst, weil bester Aufwand/Wirkung.*
2. **Hotness-Schätzung** (`hotness.rs`) + tiered Inline-Budget — mittel.
3. **Partielle Evaluation** heißer Funktionen mit konstanten Argumenten — mittel-groß.
4. **Superblock-Bildung + Block-Layout** — groß.

## Ehrliche Abgrenzung
- Das ersetzt kein echtes PGO (Profile-Guided Optimization): statische Schätzung
  liegt manchmal daneben (datenabhängige Hotness sieht sie nicht). Ein optionaler
  **PGO-Pfad** (`-fprofile`-Instrumentierung → Rebuild) wäre die ehrliche Ergänzung
  für die Fälle, wo Schätzung nicht reicht — dann ist es „AOT mit optionalem
  Profil", nicht „AOT rät alles".
- „JIT-Pfade finden" heißt hier **statisch schätzen, was ein JIT gemessen hätte** —
  nicht messen. Der Gewinn ist Warmup-frei + deterministisch; der Preis ist die
  Schätz-Ungenauigkeit. Das ist der ehrliche Trade, kein Free-Lunch.

## Messplan (wie bei M0: erst messen)
Vor dem Bau: an den Benchmarks (`benchmarks/`) die **Decke** schätzen — was bringt
manuelles `!prof` + `alwaysinline` auf den heißen Schleifen gegenüber -O2? Ist der
Gewinn <5%, lohnt der Optimizer nicht (LLVM -O2 -march=native holt schon fast alles);
ist er >20%, lohnt er. Erst die Zahl, dann das Quartal — dieselbe Gate-Disziplin wie
beim Frontend.

## GEBAUT + GEMESSEN: Schritt 1 (`!prof`-Branch-Weights aus Schleifentiefe)
Der billigste/wirkungsvollste Plan-Schritt ist umgesetzt: `loop_branch_bias` in
`crates/backend/src/lib.rs` schätzt statisch (reduzibler CFG: Kante `u→v` mit
`v≤u` = Rückwärtskante → Schleifen-Header `v`, Körper `[v,u]`), welcher Zweig
einer bedingten Verzweigung in der Schleife bleibt, und setzt `!prof
branch_weights` (100:3) am Schleifen-Ausgangs-Branch. Läuft in BEIDEN Backends
(Java + Vire). Test: `crates/backend/tests/branch_weights.rs`. Abschaltbar per
`FASTLLVM_NO_PROF=1` (A/B).

**Messung der Decke (Gate-Disziplin):** branch-lastiger Workload (200M Iterationen,
`if i%7 / elif i%13 / else`), 3 Läufe je Variante:
- mit `!prof`:  0,215 / 0,215 / 0,220 s
- ohne `!prof`: 0,216 / 0,212 / 0,220 s

→ **kein messbarer Unterschied (~0%).** Bestätigt die Vorhersage (<5%): LLVM
`-O2 -march=native` ordnet diese Branches schon optimal an; die statischen Weights
stimmen mit LLVMs eigener Schleifen-Heuristik überein und addieren nichts. Der
Wert läge nur dort, wo LLVM falsch rät (seltene Fehler-/Kalt-Pfade) — und selbst
da klein.

**Konsequenz (ehrlich, Gate-getreu):** Schritt 1 ist korrekt + kostenlos
implementiert, aber die gemessene Decke rechtfertigt die schwereren Schritte 2–4
(volle `hotness.rs`, partielle Evaluation, Superblöcke) NICHT — der Plan selbst
sagt „<5% → lohnt nicht". Der reale Hebel bleibt der RC-/Objekt-Pfad
(Region-Inferenz), nicht AOT-Branch-Tricks. Schritte 2–4 bleiben geplant, aber
ungebaut, bis ein gemessener Fall sie rechtfertigt (z.B. branch-lastiger Code mit
klaren Kalt-Pfaden, den LLVM falsch schätzt — oder der optionale PGO-Pfad).

## Untersuchung: lohnen sich die vier Techniken? (messungsgetrieben)
*Frage: Aufrufgraph analysieren / Zweig-Wahrscheinlichkeiten / spezialisierte
Versionen für häufige Typkombinationen / mehrere Varianten + Laufzeit-Auswahl.*

**Der entscheidende Kontext zuerst:** die Benchmarks (`benchmarks/vire-lang/`) zeigen
compute-gebundenen Code schon bei **C++/Rust-Parität** (arith 1,02×, fib 0,91×,
mandelbrot 0,99×, nsieve 1,02× Rust). Der EINZIGE gemessene Gap ist der RC-/Objekt-
Pfad (binary-trees 2,65×). **Keine der vier Techniken adressiert Speicherverwaltung** —
sie zielen auf Compute/Dispatch, der schon optimal ist. Das rahmt jede Antwort: der
Spielraum auf dem Compute-Pfad ist klein, der Hebel für den echten Gap ist Region-
Inferenz, nicht AOT-Hotpath-Tricks. Mit dieser Erdung:

1. **Ganzen Aufrufgraphen analysieren — JA, lohnt sich, ist schon da.** Devirt/Pruning/
   Inliner/`static_writes`/interproz. Region-Inferenz laufen alle über den Call-Graph.
   Kosten niedrig (Closed World = vollständiger Graph vorhanden). Kein neuer großer
   Aufwand, eher die Basis, auf der der Rest sitzt. **Verdikt: bereits eingelöst.**

2. **Zweig-Wahrscheinlichkeiten — LOHNT SICH BEDINGT, billige Version zuerst.** LLVM
   `-O2` schätzt Branches schon gut (deshalb die Parität). Statische `!prof`-Weights
   aus Schleifentiefe helfen v.a. dort, wo LLVM falsch rät: Fehler-/Kaltzweige
   (`?`/Err, null-checks). Erwarteter Gewinn auf compute-Code **<5%** (er ist schon
   optimal), messbar mehr nur bei branch-lastigem Code mit klaren Kalt-Pfaden.
   **Kosten klein** (Loop-Tiefe→`!prof`, LLVM macht den Rest). **Verdikt: der billige
   erste Schritt, aber Decke vorher messen — auf Parität-Code ist wenig zu holen.**

3. **Spezialisierte Versionen für Typkombinationen — TEILS SCHON DA (Monomorphisierung).**
   Für Generics macht Vire genau das (pro Typargument eine Instanz). Der Zusatz wäre
   **Wert-Spezialisierung / partielle Evaluation**: heiße Funktion mit konstantem
   Argument → gefaltete Kopie. Lohnt sich NUR, wenn heiße Funktionen konstante Args
   bekommen (Config-Flags, feste Größen) — in den Benchmarks selten. **Verdikt:
   Typ-Spezialisierung erledigt; Wert-Spezialisierung situativ, erst bei gemessenem
   Fall bauen, nicht spekulativ (Code-Bloat).**

4. **Mehrere Varianten + Laufzeit-Auswahl — AM WENIGSTEN wert im Closed-World-AOT.**
   Das ist der JIT-artigste Vorschlag und genau der, den AOT am wenigsten braucht:
   Wenn der Typ statisch bekannt ist (Closed World + Monomorphisierung + CHA-Devirt),
   ruft man die richtige Variante **direkt** — keine Laufzeit-Auswahl, kein Dispatch-
   Overhead, kein Bloat. Laufzeit-Auswahl hilft nur an **genuin polymorphen** Sites
   (megamorph, 3+ Typen) — und die behandelt der Solver SCHON via `CallPoly`
   (guarded devirtualization / polymorphic inline cache = ein paar Varianten + Typ-
   Wächter-Kaskade). Der Rest wäre Code-Bloat (N Varianten × M Funktionen, Icache-
   Druck) für Fälle, die die geschlossene Welt statisch auflöst. **Einzige echte
   Nische:** Wert-basierte Varianten, deren Wert erst zur Laufzeit stabil wird (z.B.
   ein Modus-Flag) — dort könnte 2-Varianten + Auswahl lohnen, aber das ist ein
   schmaler Fall, kein allgemeiner Pass. **Verdikt: nein als generelle Strategie;
   die nützliche 90% (polymorphe Sites) ist über `CallPoly` schon abgedeckt.**

## Gesamturteil der Untersuchung
Priorität nach Aufwand/Wirkung, geerdet an den Messungen:
- **#1 (Call-Graph):** erledigt, Fundament.
- **#2 (Branch-Weights):** billig, kleiner Gewinn (Parität-Code) → als Erstes, aber
  Decke messen; wahrscheinlich <5%.
- **#3 (Typ-Spezialisierung):** für Generics erledigt; Wert-Spezialisierung nur bei
  gemessenem Bedarf.
- **#4 (Laufzeit-Varianten):** überwiegend redundant zu statischer Mono+Devirt im
  Closed World; die polymorphe Nische ist via `CallPoly` schon da. **Nicht bauen.**

**Kernbefund:** Diese vier optimieren einen Pfad, der bereits auf C++/Rust-Niveau
ist — der Ertrag ist marginal. Der einzige gemessene Gap (RC/Objekte, ~2,7×) liegt
**orthogonal** dazu; ihn schließt interprozedurale Region-Inferenz (v2 hat pagerank
schon 2,0×→1,55× gebracht), nicht Hotpath-Spezialisierung. Ehrliche Empfehlung:
`!prof`-Weights als billiges Experiment, sonst die AOT-Hotpath-Maschinerie
ZURÜCKSTELLEN und die Region-Inferenz fertigbauen — dort sitzt die gemessene Zahl.
