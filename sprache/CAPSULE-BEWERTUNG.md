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

> **Korrektur nach Review — der Fehlschluss, den die erste Fassung machte:**
> „Nur `()` rein" garantiert Isolation **nicht**, solange die Eingaben RC-
> Referenzen tragen. Der Rumpf adressiert äußeren Speicher nicht über *Namen*,
> sondern über *die Eingabe selbst*. Isolation = Namens-Sichtbarkeit ist ein
> Trugschluss. Deshalb ist die **garantierte** Semantik die **reine** Form:

**Regeln der reinen Form (das, was `capsule` GARANTIERT):**
- **Rein: tiefe Kopie, kein Move, kein `&`.** Jede `()`-Eingabe wird **tief in die
  Arena kopiert**. *Nicht* „kopiert oder gemoved": Move würde nur den *Namen*
  moven; ein RC-Graph mit refcount>1 lebte weiter draußen (aliasiert) und der
  arena-freie Rumpf mutierte draußen sichtbare Objekte → genau der dangling-/Race-
  Fall, den `capsule` verhindern soll. **Erst die Deep-Copy macht die Eingabe
  wirklich region-lokal.**
- **Isolation + Containment folgen erst aus der Deep-Copy** (nicht aus der
  Namensregel): weil der Rumpf **keinen** äußeren Zeiger *besitzt*, kann ein Bug/
  OOB/Korruption im Rumpf nur die Arena treffen. Das ist das eigentliche
  Sicherheitsversprechen — und es gilt **nur** ohne `&`-Ausnahme.
- **Speicher:** alle `New`/Sammlungen im Rumpf → Arena-Bump. Kein retain/release,
  kein Zyklen-Kollektor (Zyklen sterben mit der Arena). Deterministisch, leckfrei.
- **Raus: tiefe Kopie des Blockwerts** in den äußeren Heap (die Arena stirbt). Kein
  Zeiger in die freigegebene Arena — erzwungen.
- **Panic:** Arena wird trotzdem frei (RAII) — Fault-Containment.

## Kostenkurve — ehrlich (die „~1× wie Rust-Arena"-Behauptung ist zurückgezogen)
Die erste Fassung verkaufte Rust-Arena-Performance mit den Sicherheitsgarantien der
teuren Form. **Beides zusammen gibt es nicht.** Die reine Form zahlt **Copy-in
*und* Copy-out**:
- Eine Rust-Arena-of-Indices zahlt **kein Copy-out** — sie gibt Indizes in einen
  *überlebenden* `Vec` zurück. `capsule` zahlt es per Konstruktion (die Arena
  stirbt). Deshalb haben sie **nicht** dieselbe Kostenkurve.
- Ist der Blockwert der *ganze verarbeitete Graph* (M0-PageRank wörtlich: großer
  Graph rein, große Ranks raus), fressen Copy-in + Copy-out die Ersparnis
  potenziell auf — **Nettogewinn ungemessen, evtl. negativ**.
- `capsule` gewinnt, wenn **viel Arbeit auf großen internen Strukturen in ein
  KLEINES Ergebnis mündet**: Aggregation, ein Skalar, ein kleiner Report. Parser/
  Deserialisierer/Validierer mit kleinem Output sind der Sweet Spot.
- **Offene Messung (M0.2):** `capsule`-PageRank *mit* Copy-in+Copy-out gegen die
  Rust-Arena messen, bevor irgendein Perf-Versprechen im Dokument steht.

## Feasibility auf FastLLVM
- **Arena-Allokator** (`jrt_arena_push/_pop/_alloc`, Bump) — klein.
- **Allokation umrouten** im Rumpf auf die aktive Arena; RC-Ops auf arena-lokale
  Objekte No-Op (`immortal`-Flag, schon im Modell). *Innerhalb* der Arena korrekt.
- **Deep-Copy-in + Deep-Copy-out** rekursiv über `jrt_array_clone`/Feldkopie —
  **das ist der reale Aufwand, nicht „~30 Zeilen"**: für zyklische Eingaben/
  Ergebnisse muss die Kopie die Zyklen erkennen (Besuchsmenge), sonst Endlosschleife/
  Duplikate; das Ergebnis muss RC-Header rekonstruieren und ggf. beim Kollektor
  wieder anmelden. Nicht trivial.
- **Isolation-Check** (F3): der Rumpf sieht nur die `()`-Namen — notwendige, aber
  (siehe Review) **nicht hinreichende** Bedingung; hinreichend ist erst die Deep-
  Copy der Eingaben.

## Offene Forschungsfragen (NICHT als fertiger Zuschnitt verkaufen)
- **`capsule(&readonly_in)` (kopiefrei):** bricht Isolation + Containment frontal —
  ein `&` in den äußeren Heap *ist* der Zeiger nach draußen, den die reine Form
  verbietet. Es braucht (a) *strikt* read-only (kein Speichern von **Arena**-
  Zeigern in die geborgte Struktur → Escape-Check Arena→außen = die M0-Analyse) und
  (b) die Garantie, dass **niemand draußen** die Eingabe während der `capsule`
  mutiert/freigibt (XOR-Regel = der Borrow-Checker, den Vire *nicht* hat; sonst
  dangling `&`, derselbe §9a-Fall über die Grenze). **Offen**, kein fertiges Feature.
- **Move-in ohne Kopie** bei *beweisbar unaliasierten* Eingaben: verlagert das
  Alias-Problem nicht, es *ist* die Whole-Program-Alias-Analyse aus M0/§7. Offen.
- **Guard-Page-Modus** (`capsule guarded`) für `unsafe`/FFI: Hardware-Containment
  gegen Überläufe. Ausbaustufe.

## Was `capsule` also GARANTIERT (der feste Kern)
Die **reine** Form — Deep-Copy-in, Deep-Copy-out, kein `&`: **deterministische,
leckfreie, RC-/Kollektor-freie Verarbeitung mit echtem Fault-Containment**, ideal
für riskante Verarbeitung mit **kleinem Output** (Parser, Deserialisierer, Plugins,
Aggregationen). Das ist ein kleineres, aber **wasserdichtes** Versprechen — und die
ehrliche Antwort auf M0: der Solver macht den Normalfall, `capsule` gibt dem
Menschen an der bewiesenen Inferenz-Grenze ein *sicheres* Werkzeug. Der Perf-Gewinn
gegenüber RC ist workload-abhängig und **erst zu messen** (M0.2), nicht zu behaupten.

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
