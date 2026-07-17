# Vire — Front-End-Bauplan (vollständig)

Der Gesamtplan des Vire-Compilers: von `.vr`-Quelltext bis zur bestehenden
Mittel-IR (`crates/ir`) in **SSA**. Ab da übernehmen Solver + Backend unverändert.
Parser-Details in [PARSER.md](PARSER.md); dieser Plan spannt alle Phasen auf,
definiert Datenstrukturen, Meilensteine und die **Java-Ablöse-Kriterien**.

## Warum jetzt (Ergebnis von M0)
[M0-MESSUNG.md](M0-MESSUNG.md) hat bewiesen: der Weg auf ~1,1–1,5× beim
geteilt/zyklischen Fall führt über **Region-Borrow-Inferenz**, und die ist auf dem
javac-IR durch **Slot-Reuse blockiert** (`Local(3)` = Owner *und* Borrow im selben
Slot). Ein eigenes Front-End, das **SSA von Anfang an** erzeugt, macht genau diese
Analyse trivial — das ist der eine Hebel, den der Bootstrap nicht liefert.

## Reuse-Grenze (was bleibt, was neu ist)
```
.vr ─► [ VIRE FRONT-END (neu) ] ─► crates/ir (SSA) ─► [ Solver + Backend (existiert) ] ─► Binary
        Lexer→Parser→Resolve→
        Infer→Comptime→Lower
```
- **Neu:** `crates/vire` (Lexer, Parser, AST, Namensauflösung, Typinferenz,
  `comptime`/Makros, SSA-Absenkung).
- **Unverändert wiederverwendet:** `crates/ir`, `crates/solver`, `crates/backend`,
  `crates/driver` (clang-Aufruf, runtime.c). Der Java-Pfad (`classfile`,
  `frontend`) ist **Bootstrap** und wird nach Kriterium unten entfernt.

## Pipeline-Phasen & Datenstrukturen

### F1 — Lexer (`vire::lexer`) — **in Arbeit**
`&str → Vec<Token>`. Token = `{ kind: TokKind, span: Span }`. Newline-als-
Terminator wie Go (PARSER.md §2.3), String-Interpolation, schachtelbare
Kommentare, `[]`-Generics (kein `<>`). **Fertig + Unit-Tests in diesem Schritt.**

### F2 — Parser (`vire::parser`) — **begonnen**
`Vec<Token> → ast::Module`. Rekursiver Abstieg (Items/Statements) + Pratt
(Ausdrücke, Präzedenztabelle PARSER.md §4.1). Fehler-Recovery (Panic-Mode),
mehrere Diagnosen pro Lauf. AST trägt Spans (für Diagnosen + Debug-Info Feature 8).

### F3 — Namensauflösung (`vire::resolve`)
Whole-Program: bindet Bezeichner an Deklarationen. Ein Modul = Datei, ein Paket =
Verzeichnis (`mod.vr`). Baut die **Symboltabelle** (Typen, Funktionen, Traits,
Impls) und den **Trait-/Impl-Index** für die Auflösung. Großschreibung=Typ als
erzwungene Regel (PARSER.md §1) → hier schon genutzt.

### F4 — Makro-/`comptime`-Expansion (`vire::comptime`)
*Vor* der Typprüfung des expandierten Codes. Hygienischer Makro-Expander
(typisierte Parameter, PARSER-hygienisch) + `comptime`-Auswerter (Interpreter über
AST/Typgraph, Rekursionslimit) + `@if`/`@when`. Liefert einen **expandierten AST**.

### F5 — Typinferenz + Trait-Auflösung (`vire::infer`)
**Bidirektionale HM-Inferenz** mit lokalen Ankern (Signaturen an Fn-/Modulgrenzen
halten Fehler nah — BEWERTUNG §5). Trait-Auflösung + **Kohärenz** (das echte
Risiko, nicht Vanilla-HM). Ergebnis: jeder AST-Knoten annotiert mit einem `Ty` in
einer Seitentabelle. **Monomorphisierungs-Aufträge** (welche Typkombinationen)
werden hier gesammelt.

### F6 — Monomorphisierung (`vire::mono`)
Pro benutzter Typkombination eine spezialisierte AST-Instanz. Dockt konzeptionell
an den vorhandenen Inliner an; erzeugt konkrete, generik-freie Funktionen für die
Absenkung.

### F7 — Absenkung nach IR **in SSA** (`vire::lower`)
`ast (typisiert, mono) → ir::Program`. Kernpunkt:
- Werttypen → Struct-Layout; Summentypen → getaggte Union; `match` → `Switch` +
  Feldzugriff; Closures → Funktion + Environment; `?` → früher Rücksprung.
- **SSA-Erzeugung direkt** (keine Slot-Wiederverwendung wie javac!) → der ganze
  GVN-gegen-Slot-Reuse-Aufwand des Bootstraps entfällt, und **Region-Borrow wird
  trivial** (jeder Wert eine eigene Nummer; Owner vs. Borrow nie im selben Slot).
- **Iterator-Mutation-Check** (REFERENZ §9a) an dieser Stelle.
Danach: `solver::run` → `elide_bounds`/`fuse_long_compares`/`elide_redundant_ref_copies`
→ `elide_pending_checks` → `inline_program` → `stack_allocate` → `backend::emit`
→ clang. **Alles unverändert.**

### F8 — Region-Borrow (der M0-Zahltreiber, auf SSA)
Auf der SSA-IR: loop-stabile Container als borgbare Region beweisen → Loop-RC
streichen (M0.1b: 4,4×→1,5×) und der Kollektor triggert nicht (108×→weg). *Auf
SSA* ist das die einfache Analyse, die auf dem Java-IR unmöglich war. Als neuer
Solver-Pass **oder** in `lower` erzeugt.

## Crate-Layout
```
crates/vire/
  Cargo.toml            # dep: fastllvm-ir (Absenkziel)
  src/
    lib.rs              # pub fn compile(src) / parse(src)
    lexer.rs   ast.rs   parser.rs
    resolve.rs infer.rs comptime.rs mono.rs lower.rs
    diag.rs             # Diagnosen (Span, Meldung, Fix-Vorschlag)
  tests/                # Lexer-/Parser-Snapshots, Beispiel-Korpus
```
Ein `vire`-Binary (in `driver` oder `crates/vire/src/main.rs`): `vire build|run|
parse|fmt file.vr`.

## Meilensteine
- **M1 (dieser Schritt):** Lexer komplett + Parser für Funktionen/Ausdrücke/Typen/
  `match`/Kontrollfluss; `vire parse` dumpt AST; Beispiel-Korpus parst.
- **M2:** Resolve + einfache monomorphe Typprüfung; `vire build` für nicht-
  generischen Code → IR → Binary (erste lauffähige `.vr`-Programme, z.B. `sieb.vr`).
- **M3:** HM-Inferenz + Traits + Monomorphisierung; `formen.vr`/`baum.vr` laufen.
- **M4:** `comptime`/Reflection/Makros (Features 2–4).
- **M5:** Region-Borrow auf SSA → **M0.1 erneut messen** (Ziel ~1,5×, dann Bounds/
  Layout ~1,1×). Stdlib + FFI (F-Phasen P5).

## Java-Ablöse-Kriterien (wann der Bootstrap gelöscht wird)
Der Java-Pfad (`crates/classfile`, `crates/frontend`, `examples/*.java`, `tests/`)
bleibt **nur** als Backend-Soundness-Wächter (0-live-Suite) und M0-Messbasis, bis:
1. `vire build` die **portierten** Regressionstests (aus `tests/` nach `.vr`) grün
   fährt (inkl. 0-live-Heapbilanz), **und**
2. Vire den M0-Graph auf ≤~1,5× bringt (Beleg, dass SSA+Region-Borrow greift).
Dann: `classfile` + `frontend` + Java-`examples`/`tests` entfernen; die Benchmark-
Java (`benchmarks/`) bleiben als **Vergleichs-Beispiele** (Rust/C++-Baselines).
**Vorher löschen = unbewiesenes Backend + nicht baubares Projekt** — daher gestuft.

## Nicht-Ziele (Front-End)
Kein Bytecode-Import mehr (der Bootstrap-Zweck ist erfüllt), keine Java-Semantik
(Boxing, alles-ist-Object), kein `<>`-Generics, keine Slot-Wiederverwendung.
