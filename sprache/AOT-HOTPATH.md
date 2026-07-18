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
EOF
echo "AOT-Plan geschrieben"