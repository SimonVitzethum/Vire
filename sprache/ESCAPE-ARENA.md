# Escape→Arena — automatische Schleifen-Arena (gebaut, gemessen)

*Ergebnis der EPS-Bewertung (`EPS-BEWERTUNG.md`): der 7-Signal-Wahrscheinlichkeits-
Solver lohnt nicht (~0%), aber die Teilmenge **Loop-Nesting × Escape → Arena/Pool**
trifft den einzigen gemessenen Gap — den Allokator. Dieser Pass baut genau das.*

## Die Decke (zuerst gemessen, Gate-Disziplin)
Der einzige Nicht-Paritäts-Gap zu Rust/C++ ist der Allokator: der Hosted-Runtime
alloziert per Knoten `calloc`+`free`. Manuelle Messung mit der bestehenden capsule-
Arena (Bump-Allokation, en-bloc-Freigabe) auf binary-trees:

| binary-trees | Zeit | vs normal |
|---|---|---|
| normal (calloc/free je Knoten) | 0,49 s | — |
| capsule-Arena (Bump, en-bloc) | 0,19 s | **2,57×** |

→ echte, große Decke (im Gegensatz zu den ~0% der Branch-Wahrscheinlichkeit).
**Lohnt sich → bauen.**

## Was der Pass tut
In der Vire-Absenkung wird eine `while`-Schleife, deren Allokationen die Iteration
nachweislich **nicht verlassen**, automatisch in eine **per-Iteration-Bump-Arena**
gelegt (`jrt_arena_push` am Rumpf-Anfang, `jrt_arena_pop` am Rumpf-Ende) — eine
automatische capsule. Objekte im Arena-Rumpf sind immortal (kein RC/Kollektor), der
Speicher wird am Iterationsende en bloc frei. Kein `malloc`/`free` je Knoten.

## Soundness-Bedingungen (konservativ — jede Unsicherheit ⇒ nicht promoten)
Eine `while`-Iteration wird nur promotet, wenn ihr Rumpf (transitiv über
Nutzer-Callees):
- **alloziert** (sonst kein Nutzen),
- **kein Feld/Index schreibt** (`x.f = …` / `a[i] = …`) — Mutation eines
  existierenden Objekts könnte eine Arena-Ref nach außen speichern. Konstruktoren
  (`Node(a, b)`) zählen NICHT als Feld-Schreibung — sie erzeugen frische Objekte,
- **keine äußere (vor der Schleife deklarierte) Variable mit einer Ref re-bindet**
  (`head = Node(head, i)` — im Vire-AST ein Let einer äußeren Ref → entkommt),
- **kein `return`/`break`/`continue`** auf Rumpf-Ebene enthält (verließe die Arena),
- **nur Nutzerfunktionen + Konstruktoren** ruft — kein extern/builtin/Lambda/
  Comprehension/MapLit/capsule (könnten eine Ref einfangen/außen speichern).

Rumpf-erzeugte Ref-Locals werden VOR dem Pop genullt (sonst läse die Funktions-
Ende-Freigabe `jrt_release` freien Arena-Speicher → use-after-free), analog zur
expliziten capsule.

## Ergebnis (gemessen, best-of-9, `-O2 -march=native`)
| binary-trees | Zeit | |
|---|---|---|
| Vire normal | 0,49 s | 2,4× Rust |
| **Vire auto-arena** | **0,202 s** | **Rust-Parität (0,99×)** |
| Rust (`Box`) | 0,205 s | |
| C++ (`new`, leakt) | 0,136 s | |

→ Der Pass schließt den Allokator-Gap auf der allokationslastigen Benchmark
**automatisch** (ohne capsule-Annotation) und bringt Vire auf **Rust-Parität**.

## Validierung (Soundness)
- **btree**: promotet, korrekt (7864260), 2,4× schneller, keine Leaks.
- **Listen-Bau** (`head = Node(head, i)`, danach genutzt): korrekt NICHT promotet,
  terminiert, korrektes Ergebnis (4999950000) — der Escape wird erkannt.
- **Callee-Escape** (`head = attach(head, i)`, attach gibt frische Node zurück):
  korrekt NICHT promotet (1249975000).
- **Array-Store**: index-assign → nicht promotet, korrekt.
- **Java-Regression 65/65** (Heap-Bilanz 0-live = Soundness-Oracle), **Vire-Suite
  grün** (35 lower-Tests inkl. `auto_arena_promoviert_*` / `auto_arena_meidet_*`).

## Grenzen (ehrlich)
- Nur `while`-Schleifen (nicht `for` — die Iterationsvariable ist eine äußere
  Element-Ref, RC-Wechselwirkung; späterer Schritt).
- Konservativ: ein builtin-Aufruf (`print`, `str`) im Rumpf blockt die Promotion
  (könnte theoretisch fangen) — Allowlist reiner Builtins wäre eine Verfeinerung.
- Per-Iteration `arena_push`/`pop` (malloc des Arena-Kopfs je Iteration): bei sehr
  vielen Iterationen mit wenig Allok pro Iteration könnte ein arena_RESET
  (Speicher behalten, nur `used=0`) amortisieren — nicht nötig für die gemessenen
  Fälle, spätere Optimierung.
- Der übrige Abstand zu C++ (leak-`new`, 0,136) ist bewusst kein Ziel: C++ leakt
  hier (kein `delete`), Vire gibt korrekt frei.
