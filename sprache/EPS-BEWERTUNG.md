# Execution Probability Solver — Bewertung (vor dem Bau)

*Nutzerwunsch: statt nur Hotness zu schätzen einen **Execution Probability Solver**
entwickeln, der Call-Graph, Dominator-Tree, Loop-Nesting, Escape-Analyse,
Wertbereiche, Typinformationen und Branch-Heuristiken zu einer Ausführungs-
wahrscheinlichkeit je Block/Kante vereint. Anweisung: „bewerte erstmal" — dieselbe
Gate-Disziplin wie bei M0 und beim AOT-Hotpath: erst die Zahl, dann der Bau.*

## Die entscheidende Vorfrage
Eine Ausführungswahrscheinlichkeit ist nur dann **wertvoll**, wenn sie eine
**Entscheidung** treibt, die (a) LLVM nicht ohnehin selbst trifft, und (b) am
gemessenen Gap etwas ändert. Der Messstand rahmt alles:

- **`!prof`-Branch-Weights aus Loop-Tiefe (bereits gebaut):** branch-lastiger
  Workload 200M Iter, mit/ohne = 0,215/0,215 s → **~0%**.
- **Semantische Branch-Heuristik (`cold` auf Vires Wurf-Funktionen — die EINE Info,
  die LLVM aus dem rohen IR nicht hat), heute gemessen:** pagerank 0,222→0,227 s →
  **~0% bis leicht negativ** (im Pending-Exception-Modell kehrt der Wurf zurück,
  `cold` allein macht den Pfad nicht tot; wieder revertiert).
- **Region-Inferenz an der Decke:** pagerank `normal == --no-rc == Rust` (Parität);
  der Rest-Gap zu Rust/C++ ist der **Allokator** (malloc/Knoten vs Bulk/Arena),
  nicht Dispatch/Branches/RC. (Siehe M0.3 v3.)
- **Compute/Traversal = Rust-Parität** über die Benchmarks.

Kurz: **der direkte Konsument einer Wahrscheinlichkeit (Branch-Weights /
Codegen-Priorität) hat auf diesem Compiler <5% Spielraum — gemessen ~0%,** weil
der Code, den die Wahrscheinlichkeit optimieren würde, schon optimal ist.

## Komponente für Komponente

### 1. Call-Graph — SCHON DA, direkt genutzt
Der Solver hat den vollständigen Closed-World-Call-Graph und fährt darüber:
RTA/CHA-Devirtualisierung, Pruning, Inliner, `static_writes`, interproz.
`instance_field_writes` (Region-Inferenz). Eine *Frequenz*-Propagation obendrauf
(Callee erbt Call-Site-Hotness) würde Inlining/Optimierung priorisieren — aber
LLVM inlinet bei `-O2` schon aggressiv nach eigenem Kostenmodell, und der Vire-
Inliner existiert. **Zusatznutzen: gering; die Struktur ist da, der Konsument
(Inlining) ist gesättigt.**

### 2. Dominator-Tree — NEU, aber der Konsument fehlt
Ein Dom-Tree würde erlauben: „Blöcke, die nur von einem kalten Guard aus erreichbar
sind, sind kalt." Das ist genau die semantische Branch-Heuristik — und die misst
~0% (oben). LLVM baut intern ohnehin Dominatoren und macht die Kalt-Pfad-Anordnung
selbst. **Zusatznutzen: gering; das Signal, das er liefert, ist gemessen wertlos.**

### 3. Loop-Nesting — SCHON GEBAUT, gemessen ~0%
`loop_branch_bias` (reduzibler CFG, Rückwärtskanten) → `!prof` 100:3 am Schleifen-
Ausgang. Bereits im Backend, beide Pfade, Test vorhanden. **Gemessen ~0%** (LLVM
ordnet Loops schon optimal; die statischen Weights = LLVMs eigene Heuristik).
**Zusatznutzen: null (bereits eingelöst und vermessen).**

### 4. Escape-Analyse — SCHON DA (`escape.rs`), direkt genutzt
Treibt Stack-Allokation (nicht in Schleifen). Als bloßes *Wahrscheinlichkeits*-
Signal fügt sie nichts hinzu — sie wird schon für ihren direkten Zweck benutzt.
**ABER:** hier steckt der einzige echte Hebel — siehe „Die wertvolle Teilmenge".

### 5. Wertbereiche — LLVM macht das besser auf dem emittierten IR
Konstanten-/Bereichs-Verengung zur Zweig-Elimination ist LLVMs Kernkompetenz
(SCCP, CVP, LVI, Range-Metadaten). Vire emittiert per-Funktion-IR, LLVM inlinet und
sieht die Bereiche dann selbst. Der einzige theoretische Vorsprung wäre *cross-
function* Bereichswissen, das nach dem Inlining verschwindet — ein schmaler,
schwer beweisbarer Fall. **Zusatznutzen: gering; Doppelarbeit zu -O2.**

### 6. Typinformationen — SCHON DA (Mono + CHA-Devirt + CallPoly)
Monomorphisierung (pro Typargument eine Instanz), CHA-Devirtualisierung (mono Sites
→ direkte Calls, Null-Check bleibt), `CallPoly` (2–3 Typen → Wächter-Kaskade). Das
IST Typ-Spezialisierung. **Zusatznutzen: null (bereits eingelöst).**

### 7. Branch-Heuristiken — die EINZIGE neue Information, gemessen ~0%
Vire *weiß*, welche Branches Null-/Bounds-/Err-Checks sind (Wissen, das LLVM aus
dem rohen IR nicht hat). Das ist der einzige Punkt mit echter Zusatzinfo. Heute
empirisch getestet (`cold` auf `jrt_throw_npe/_bounds/_throw`): **~0% bis leicht
negativ.** Grund: (a) LLVM behandelt Check-Fail-Blöcke, die in einem Call enden,
schon als selten; (b) Checks in heißen Schleifen hebt/eliminiert LLVM ohnehin (das
bounds-lastige Array-Bsp. wurde komplett wegoptimiert); (c) im Pending-Modell
kehrt der Wurf zurück → `cold` macht den Pfad nicht tot.

## Gesamturteil: den 7-Signal-Solver NICHT bauen
- **4 von 7 Signalen existieren** und werden für ihren direkten Zweck genutzt
  (Call-Graph, Escape, Loop-Nesting, Typinfo).
- **2 von 7 macht LLVM `-O2` besser** auf dem emittierten IR (Wertbereiche,
  generische Zweig-Vorhersage).
- **1 von 7 trägt echte Zusatzinfo** (semantische Check-Branches) — und die misst
  **~0%**.
- Der einzige Konsument einer vereinten Wahrscheinlichkeit (Branch-Weights /
  Codegen-Priorität) hat **<5% Decke, gemessen ~0%**, weil Compute/Traversal schon
  Rust-Parität sind.

Eine elegante vereinte `f64`-Wahrscheinlichkeit je Block wäre schöne Ingenieurs-
arbeit — aber sie würde **bereits-optimalen Code optimieren.** Das ist genau der
Fall, den die Gate-Disziplin ablehnt (wie beim AOT-Hotpath Schritt 2–4).

## Die wertvolle Teilmenge (falls überhaupt bauen)
Im Signal-Bündel steckt EIN Hebel, der den **gemessenen** Gap trifft — aber es ist
nicht die Wahrscheinlichkeit, sondern **Loop-Nesting × Escape → Allokations-
Strategie**:

> Der einzige gemessene Rest-Gap zu Rust/C++ ist der **Allokator** (malloc-pro-
> Knoten). Ein *schleifen-verschachtelter* `New`, der seine *Region nicht verlässt*
> (Escape-Signal, das `escape.rs` schon berechnet), ist der Kandidat für **Arena-/
> Pool-Allokation** statt malloc — genau die Achse (binary-trees 2,7×, pagerank-
> build 1,9×), auf der Vire nicht Parität ist.

Das ist ein **fokussierter Escape→Arena-Pass** (2 Signale, konkreter Konsument =
Allokations-Strategie), KEIN 7-Signal-Wahrscheinlichkeits-Solver — und deckt sich
mit dem bestehenden capsule/Arena-Hebel des Projekts (`jrt_arena_push/pop`). Er
hat eine **messbare Decke** (der malloc-vs-Bulk-Gap: pagerank-build 1,9×,
binary-trees 2,7×), im Gegensatz zu den ~0% der Branch-Wahrscheinlichkeit.

## Empfehlung
1. **7-Signal-EPS: nein.** Optimiert bereits-optimalen Compute; Konsument
   gemessen ~0%. Die Signale existieren einzeln oder sind LLVM-Doppelarbeit.
2. **Wenn Optimierungs-Budget: den Escape→Arena-Pass** (Loop-Nesting × Escape →
   Pool-Allokation heißer, nicht-entkommender `New`-Sites) — er trifft den einzigen
   gemessenen Gap (Allokator) mit einer echten Decke (1,9–2,7×), statt ~0%.
3. **Erst die Decke dieses Passes messen** (manuell einen heißen `New` durch Arena
   ersetzen, gegen -O2 timen) — dieselbe Gate-Disziplin, bevor der Pass gebaut wird.

*Belege: `!prof`-Messung (AOT-HOTPATH.md), Region-Decke (M0.3 v3), `cold`-Messung
(diese Session), Benchmark-Parität (benchmarks/vire-lang/).*
