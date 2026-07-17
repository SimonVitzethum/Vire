# `capsule(){}` — Bewertung & Design

*Anforderung: `capsule(a, b) { … }` — nur die in `()` genannten Variablen können
rein und raus, alles darin lebt in einem **eigenen virtuellen RAM**; für wichtige,
riskante Sachen. Entscheidung zuerst, dann Umsetzung.*

## Urteil: **Ja — integrieren.** Und zwar als *der* opt-in-Hebel gegen das M0-Problem.

`capsule` verbindet drei bekannte, starke Ideen in einem Konstrukt:
1. **Region-/Arena-Speicher** — alles im Rumpf Allozierte landet in einer privaten
   Arena, die am Ende **in einem Rutsch** freigegeben wird.
2. **Isolation** — der Rumpf kann nur die `()`-Eingaben sehen; sonst nichts von
   außen (eigenes „virtuelles RAM").
3. **Expliziter Interface-Vertrag** — nur `()` rein, nur der Blockwert raus.

Das ist die **direkte, opt-in-Antwort auf M0**: die geteilt/zyklisch/mutierende
Teilmenge war 4–108× langsamer, weil RC + Zyklen-Kollektor feuern. In einer capsule
gibt es **kein RC und keinen Kollektor** — die Arena besitzt alles und wird als
Ganzes frei. Der Programmierer bekommt damit *genau* den Rust-Arena-Gewinn, den
M0.1b als Decke (~1×) gemessen hat — **ohne** die manuelle Index-Disziplin, die Rust
dafür verlangt. Es ist der ehrliche Kompromiss: der Solver kann Region-Borrow nicht
immer *beweisen* (M0), aber der Programmierer kann es an der wichtigen Stelle
**deklarieren**.

## Semantik

```vire
out = capsule(input) {
    // private Arena („virtuelles RAM"): jede Allokation hier ist arena-lokal.
    // Sichtbar ist NUR `input` (hineinkopiert); der äußere Scope ist unerreichbar.
    mut g = build_graph(input)        // Knoten → Arena, KEIN RC, KEIN Kollektor
    for _ in 0..40 { step(g) }        // mutiert frei, arena-lokal
    summary(g)                         // Blockwert → in den äußeren Heap KOPIERT
}
// Arena hier en bloc freigegeben (ein free); `out` überlebt.
```

**Regeln (resolve/lower erzwingen sie):**
- **Rein:** nur die `()`-Variablen; sie werden in die Arena **kopiert** (oder
  gemoved). Damit sind sie region-lokal — kein Zeiger nach draußen.
- **Isolation by construction:** der Rumpf darf **keinen** Namen des umgebenden
  Scopes nennen außer den `()`-Eingaben → er kann außen liegenden Speicher gar nicht
  adressieren (Compilezeit-Fehler sonst). Keine Hardware nötig für *sicheren* Code.
- **Speicher:** alle `New`/Sammlungen im Rumpf → Arena-Bump-Allokation. **Kein
  retain/release, kein Zyklen-Kollektor** (Zyklen in der Arena sind egal — sie
  werden mit der Arena frei). Deterministisch, leckfrei.
- **Raus:** nur der Blockwert. Da die Arena stirbt, wird er **tief in den äußeren
  Heap kopiert** (kein Zeiger in freigegebene Arena — erzwungen).
- **Fehler/Panic im Rumpf:** die Arena wird trotzdem freigegeben (RAII-artig) —
  Fault-Containment.

## Warum „für wichtige, riskante Sachen" passt
- **Riskant = Bugs:** ein Fehler im Rumpf (OOB, Korruption in `unsafe`) kann per
  Konstruktion nur die Arena treffen, nicht den Rest des Programms. Beim Verlassen
  ist alles weg → kein Leck, kein dangling.
- **Riskant = untrusted Input** (Parser, Deserialisierer, Plugin): klare
  Angriffsfläche (nur `()`), begrenzter Blast-Radius (die Arena).
- **Wichtig = heiß:** genau die Stellen, wo RC weh tut (M0), holt man in eine
  capsule und ist RC-/Kollektor-frei.

## Feasibility auf FastLLVM (konkret, niedriges Risiko)
Alle Bausteine existieren oder sind klein:
- **Arena-Allokator** in `runtime.c`: `jrt_arena_push()`/`_pop()` + `jrt_arena_alloc`
  (Bump). Klein, ~30 Zeilen.
- **Allokation umrouten:** im capsule-Rumpf zeigt `jrt_alloc`/`jrt_alloc_array` auf
  die aktive Arena statt auf den RC-Heap (thread-lokaler „aktueller Allokator"-
  Zeiger). RC-Ops (`retain`/`release`) auf arena-lokale Objekte sind **No-Ops**
  (Header-Flag `arena` gesetzt → wie „immortal", schon im RC-Modell vorhanden!).
- **Deep-Copy-Out:** der Blockwert wird über die vorhandene `jrt_array_clone`/
  Feld-Kopie-Maschinerie in den Heap kopiert (rekursiv).
- **Isolation-Check:** reine Namensauflösungs-Regel (F3) — der Rumpf hat nur die
  `()`-Namen im Scope. Null Runtime-Kosten, Compilezeit.
- **Lowering:** `capsule(args){body}` → `arena_push`; body (mit umgeroutetem
  Allokator); `out = deep_copy(bodywert)`; `arena_pop`; `out`. Als IR-Sequenz oder
  Solver-Pass.

Der Header trägt schon `rcflags` mit einem „immortal"-Konzept (negativer refcount) —
arena-lokale Objekte nutzen genau diesen No-Op-Pfad. **Das RC-Modell muss dafür
nicht angefasst werden.**

## Grenzen / Zuschnitt (ehrlich)
- **Copy-in/out kostet.** Große Eingaben tief zu kopieren ist real. Zuschnitt:
  `capsule(&readonly_in)` erlaubt **geborgte, nur-lesbare** Eingaben ohne Kopie
  (der äußere Speicher bleibt außen, read-only → sicher); nur mutierbare/besessene
  Eingaben werden kopiert.
- **Kein Zeiger darf raus.** Der Blockwert wird kopiert; eine capsule kann keine
  Referenz in ihre eigene (sterbende) Arena zurückgeben — erzwungen.
- **Nicht für alles.** Es ist *opt-in* für heiße/riskante Scopes, nicht der
  Default. Der Default bleibt Solver-inferiertes RC/Escape.
- **„Virtuelles RAM" = arena + Sprach-Isolation**, nicht per se Hardware-Schutz.
  Für *sicheren* Code ist die Sprach-Isolation vollständig (kein außen-Zeiger
  nennbar). Für `unsafe`/FFI im Rumpf kann ein **Guard-Page-Modus**
  (`capsule guarded`) die Arena mit Schutzseiten umgeben, sodass Überläufe
  faulten statt zu korrumpieren — starke, aber optionale Ausbaustufe.

## Einordnung
Vale hat „regions", Rust hat Arena-Crates (`bumpalo`) + Lifetimes, Zig hat explizite
Allokatoren. Vire macht daraus ein **Sprach-Konstrukt mit erzwungenem Interface** —
das Neue ist die Kombination aus *Isolation (nur `()` )* und *Region (eigene
Arena)* in einem Block, opt-in, ohne Lifetime-Syntax. Es ist die deklarative
Ergänzung zur inferierten Speicherverwaltung: der Solver macht den Normalfall, die
capsule macht den *wichtigen, riskanten, heißen* Fall — genau dort, wo die Inferenz
laut M0 an ihre Grenze kommt.

**Integration:** Schlüsselwort `capsule`, in Design (SPRACHE/REFERENZ/PARSER) und
Parser aufgenommen; Lowering + Arena-Runtime als eigener Meilenstein (nach der
End-to-End-Basis).
