# Lohnt sich eine eigene Sprache für FastLLVM? — Bewertung

*Arbeitsname der Sprache: **Lume** (von „lumen"/Licht, echot LLVM; provisorisch).
Details der Syntax in [SPRACHE.md](SPRACHE.md), Beispiele in [beispiele/](beispiele/).*

## 1. Der Anspruch (und wo er sich selbst widerspricht)

Gewünscht ist eine Sprache, die gleichzeitig ist:

1. **so einfach wie Python** (keine Lifetimes, kein Ownership, keine manuelle Speicherverwaltung),
2. **speichersicher** (kein use-after-free, kein OOB, kein null-deref),
3. **hochperformant** (Rust-/C-Niveau),
4. **AOT-kompiliert**,
5. **(fast) ohne Runtime**,
6. **mit Zugriff auf alle C-, C++- und Rust-Bibliotheken**,
7. **deckt alle Bereiche von C/C++/Rust ab** (Systemnah bis Hochsprache),
8. **extrem leicht, aber mächtig**.

Drei dieser Punkte stehen in echtem Spannungsverhältnis. Ehrliche Analyse zuerst,
weil sie das ganze Design bestimmt:

### Spannung A — „speichersicher" + „kein Ownership" + „keine Runtime"

Das ist das **Dreieck der Speichersicherheit**. Es gibt exakt drei bekannte Wege,
Speichersicherheit herzustellen, und jeder opfert genau eine der drei Ecken:

| Weg | Beispiel | Ownership-Syntax? | Runtime? |
|---|---|---|---|
| **Tracing-GC** | Go, Java, C# | nein ✅ | ja (Kollektor, Pausen) ❌ |
| **Ownership/Borrow** | Rust | ja ❌ (Lifetimes) | nein ✅ |
| **Referenzzählung (RC)** | Swift, Python | nein ✅ | klein (RC + Zyklen) ⚠️ |

„Kein Ownership **und** keine Runtime **und** sicher" gleichzeitig — das gibt es
in keiner existierenden Sprache, **weil es im allgemeinen Fall unmöglich ist**:
Sicherheit für zyklische Heap-Daten ohne statische Annotation braucht *irgendeine*
dynamische Buchhaltung.

**FastLLVMs Antwort — und der eigentliche Grund, warum die Sprache machbar ist:**
Man kann das Dreieck *pro Programmstelle* auflösen statt global. Der Whole-Program-
Solver **beweist Ownership, wo es geht** (→ 0 Runtime, wie Rust), und **fällt auf
RC zurück, wo nicht** (→ winzige Runtime). Genau das tut FastLLVM heute schon:

- Azyklische Typen → Zyklen-Kollektor **entfällt komplett** (`-DFASTLLVM_NO_CYCLES`).
- Nicht-entkommende Objekte → **Stack statt Heap** (Escape-Analyse).
- Immortal-only-/geborgte Locals → **retain/release fallen weg** (RC-Elision).
- Der irreduzible Rest → RC + Bacon-Rajan-Zyklen-Kollektor (~2 KB).

Ergebnis in den Benchmarks (DESIGN.md §9): loop-allozierte Objekte laufen
**GC- UND RC-frei** und schlagen Rusts `Box`. Der Programmierer schreibt **null**
Speicher-Annotationen — der Solver liefert den Beweis, den Rust den Menschen
schreiben lässt.

→ **„Keine Runtime für alles" ist unmöglich; „keine Runtime für den beweisbaren
Großteil, RC für den Rest, null Annotationen" ist gebaut und gemessen.** Die
Sprache erbt das direkt.

### Spannung B — „so einfach wie Python" + „hochperformant" + „AOT"

Pythons Einfachheit kommt aus **Dynamik**: Duck-Typing, Laufzeit-Reflexion,
Monkey-Patching. Genau die Dynamik macht Python langsam und braucht einen
Interpreter/Runtime. AOT + Performance verlangt **statische Typen**.

Der Ausweg ist bekannt und Jahrzehnte erprobt (ML, Haskell, OCaml, F#, Swift,
neuere Rust-Ergonomie): **vollständige Typinferenz**. Der Code *sieht aus* wie
Python (keine Typannotationen), ist aber statisch getypt — die Typen werden
inferiert (Hindley-Milner + lokale Bidirektionalität). Man bekommt Pythons
Leichtigkeit **ohne** Pythons Dynamikkosten.

```python
# Python — dynamisch, langsam, Runtime nötig
def add(a, b): return a + b
```
```lume
// Lume — sieht identisch aus, ist aber statisch monomorphisiert, AOT, zero-cost
fn add(a, b) = a + b        // a, b: inferiert; für jede benutzte Typkombination
                            // eine spezialisierte Maschinencode-Variante
```

→ **Die „Einfachheit von Python" ist erreichbar, wenn man Dynamik durch Inferenz
ersetzt.** Der Preis: keine echte Laufzeit-`eval`/Monkey-Patching (das braucht
sowieso niemand für Performance-Code), und die Closed-World-Annahme (s. u.).

### Spannung C — „alle C/C++/Rust-Bibliotheken" + „speichersicher/eigene Sprache"

Das ist der **härteste und am meisten überverkaufte** Punkt — überall, nicht nur
hier. Die nüchterne Realität der Sprach-Interoperabilität:

- **C:** Der C-ABI ist der **universelle Klebstoff** der gesamten Softwarewelt.
  Direktes FFI ist einfach und vollständig. SQLite, zlib, OpenSSL, BLAS/LAPACK,
  libcurl, FFmpeg, das halbe OS — alles C-ABI. **✅ vollständig machbar.**
- **C++:** Teilweise. Itanium-ABI (Linux) ist stabil genug für Name-Mangling und
  vtables, aber **Templates** (header-only, brauchen Instanziierung), Exceptions,
  RAII-Destruktoren und `std::`-Typen brauchen einen C++-Aware-Binding-Generator
  (wie Swifts C++-Interop oder `cxx`/`autocxx`). „Alle" C++-Bibliotheken inkl.
  beliebiger Templates: **nein**. Public-API über generierte Bindings: **ja.**
- **Rust:** **Kein stabiler ABI.** Rust-Crates lassen sich nur einbinden, wenn sie
  eine C-Schnittstelle exportieren (`#[no_mangle] extern "C"`) — dann sind sie aber
  effektiv C-Bibliotheken. Idiomatisches Rust (Generics, Traits, `&`-Referenzen an
  der Grenze) direkt aufzurufen bräuchte den Rust-Compiler selbst. **Nein.**

**Wichtig zur Einordnung:** *Keine* Sprache außer C++ selbst kann „alle C++-
Bibliotheken" nutzen, und *keine* Sprache außer Rust kann „alle Rust-Crates"
nutzen — das gilt für Python, Go, Swift, Zig, Julia **genauso**. Der C-ABI ist die
Grenze für alle. Der Anspruch „alle drei" muss also ehrlich heißen:

> **C nativ und vollständig; C++ und Rust über ihre C-ABI-Oberflächen bzw.
> generierte Bindings.** Das ist praktisch dieselbe Reichweite wie Python-C-
> Extensions oder Swift — und deckt real >90 % der wichtigen Bibliotheken ab, weil
> die Performance-kritische Welt C-ABIs spricht.

## 2. Was FastLLVM heute schon liefert (der halbe Compiler steht)

Der teure, riskante Teil eines solchen Compilers ist **nicht** der Parser — es ist
Codegen, Speichermodell und die Sicherheits-Check-Elision. Das ist **fertig und
gemessen**:

- **LLVM-Backend** (textuelles IR + clang, `-march=native`, LTO): Rust-/C-Niveau,
  in Arithmetik AVX2-vektorisiert schneller als beides.
- **Speichermodell:** RC + Zyklen-Kollektor, Escape-Analyse→Stack, RC-Elision,
  Azyklizität→Kollektor-Elimination. Heap-Bilanz überall 0 live.
- **Sicherheits-Check-Elision:** Bounds-Check-Elision via GVN (Schleifenwächter,
  Long-Induktion, And-Masken, konstante Schranken), Null-Check-Elision für
  nicht-null Receiver, pending-Check-Elision.
- **Whole-Program-Solver:** RTA/CHA-Devirtualisierung, bikonditionale Devirt,
  Inlining, interprozedurale Escape-Summaries.
- **Plattformen:** hosted (libc), freestanding/seL4 (~2 KB Runtime), Threads.

Eine eigene Sprache müsste davon **nichts** neu bauen. Sie bräuchte nur ein neues
**Front-End** (Lexer, Parser, Typinferenz) das dieselbe Mittel-IR (`crates/ir`)
erzeugt. Der gesamte Solver + Backend bleibt.

## 3. Warum das Java-Bytecode-Front-End ein *Nachteil* ist

Der stärkste Einzelgrund für eine eigene Sprache: **javac-Bytecode ist eine
schlechte IR-Quelle**, und das hat uns real Arbeit gekostet.

- **Kein SSA, aggressives Slot-Recycling.** Genau das machte die Bounds-Check-
  Elision schwer: Index, Schranke und Array liegen am Schleifenwächter in *anderen*
  Locals als am Zugriff, obwohl es dieselben Werte sind. Wir mussten ein komplettes
  **globales Value-Numbering mit Phi-Kollaps** bauen, nur um die SSA-Information zu
  *rekonstruieren*, die ein eigenes Front-End **gratis** hätte.
- **Java-Semantik-Ballast:** alles ist ein `Object` mit Header, Autoboxing von
  Primitiven in Generics, `int`-only Array-Indizes, erzwungene Klassen, keine
  Werttypen, keine vorzeichenlosen Ganzzahlen, keine Kontrolle über das Layout.
- **JNI-Interop** ist schwer statt leichtgewichtig.

Ein eigenes Front-End, das **direkt SSA** erzeugt, würde:
- die Solver-Passes **einfacher und effektiver** machen (kein GVN-Kampf gegen
  Slot-Reuse),
- **Werttypen/Structs ohne Header**, unsigned-Typen, direktes C-Layout erlauben,
- **first-class C-FFI** statt JNI,
- die Sprache von Java-Semantik befreien (kein Boxing, keine erzwungene OOP).

## 4. Was die Sprache konkret bringen würde

1. **Eine Nische, die real leer ist.** Es gibt heute nichts, das *gleichzeitig*
   Python-leicht **und** Rust-schnell **und** ohne Speicher-Annotationen **und**
   AOT-nativ **und** ohne nennenswerte Runtime ist. Go hat GC + Pausen. Swift hat
   RC, ist aber Apple-zentriert und nicht ohne Runtime. Nim/Crystal kommen am
   nächsten, haben aber GC bzw. RC ohne Whole-Program-Elimination. Zig ist schnell,
   aber manuell/unsicher. **Lume = Nims/Crystals Ergonomie + FastLLVMs beweisbare
   RC-Elimination.**
2. **Der Beweis steht.** Die Benchmarks zeigen: die Technik hält Rust-Niveau (und
   schlägt es teils). Das Risiko „geht das überhaupt performant?" ist bereits
   beantwortet — mit einer *fremden*, ungünstigen Front-End-Sprache (Java). Mit
   eigener SSA-IR wird es eher besser.
3. **Ergonomie-Gewinn:** Python-Syntax + statische Sicherheit + null manuelles
   Speichermanagement ist für die meisten Anwender *der* Kaufgrund. Sie schreiben
   Anwendungslogik wie in Python und bekommen C-Binaries.
4. **Systemnah bis Hoch:** Werttypen + C-Layout + freestanding-Target decken den
   C/Zig-Bereich ab; Traits/Generics/Pattern-Matching/Summen-Typen den Rust-Bereich;
   Inferenz + GC-artige Ergonomie den Python/Go-Bereich. Ein Sprachkern, drei Welten.

## 5. Aufwand, Risiko, Grenzen (ehrlich)

**Was neu gebaut werden muss:**
- Lexer + Parser (Wochen).
- **Typinferenz** (Hindley-Milner + Traits/Typklassen-Auflösung + Monomorphisierung)
  — das anspruchsvollste Stück, aber *gelöstes* Problem mit reichlich Literatur.
- Absenkung Sprache→`crates/ir` **in SSA** (überschaubar, IR existiert).
- Minimale Standardbibliothek (Strings, Collections, I/O — vieles über C-libc).
- C-Header→Binding-Generator (für den Interop-Anspruch).

**Echte Grenzen, die bleiben:**
- **Closed World.** Whole-Program-Ownership-Inferenz und Devirtualisierung brauchen
  *alle* Quelltexte zur Compilezeit. Kein Laden unbekannten Codes zur Laufzeit
  (Plugins nur über stabile ABI-Grenzen). Das ist der Preis für „RC-Elimination
  ohne Annotationen" — und für FastLLVMs Zielgruppe (native Binaries, seL4) ohnehin
  gegeben.
- **Nie 100 % runtime-frei.** Der zyklische, nicht-beweisbare Rest braucht RC +
  Kollektor (~2 KB). Für viele Programme ist er ganz weg, nie garantiert.
- **„Alle" C++/Rust-Bibliotheken** bleibt „alle mit C-ABI-Oberfläche" (s. §1.C).
- **Inferenz-Grenzen:** globale Typinferenz ohne *jede* Annotation kann mehrdeutig
  werden; an öffentlichen API- und FFI-Grenzen sind Annotationen nötig (und dort
  auch als Doku erwünscht). Das bleibt Python-leicht (Annotation optional, nicht
  wie Rust-Lifetimes verpflichtend).

## 6. Urteil

**Ja — und zwar mit ungewöhnlich günstigem Verhältnis, weil die schwere Hälfte
(Backend, Speichermodell, Check-Elision, Solver) schon steht und in Benchmarks
belegt ist.** Der Java-Bytecode-Weg war der Prototyp, der bewiesen hat, dass die
*Technik* Rust einholt. Eine eigene, in SSA absenkende Sprache räumt genau die
Reibung weg, die uns Arbeit gekostet hat (GVN gegen Slot-Reuse), und schaltet die
Ergonomie frei, die Java verbaut (Werttypen, kein Boxing, C-FFI, keine OOP-Pflicht).

**Der Anspruch muss an drei Stellen ehrlich zugeschnitten werden** — das macht ihn
nicht kleiner, nur korrekt:
1. „Keine Runtime" → **keine Runtime für den beweisbaren Großteil, ~2 KB RC für den
   zyklischen Rest.**
2. „Alle Bibliotheken" → **C nativ; C++/Rust über C-ABI/Bindings** (dieselbe Grenze
   wie für jede Nicht-C++/Rust-Sprache).
3. „So einfach wie Python" → **Syntax ja; Semantik statisch-inferiert** (kein
   Laufzeit-`eval`, Closed World).

Innerhalb dieses Zuschnitts ist die Sprache **realistisch, differenziert von allem
Existierenden und technisch bereits zur Hälfte fertig**. Empfehlung: als
eigenständiges Front-End auf `crates/ir` aufsetzen, mit SSA-Erzeugung von Beginn an.

**Empfohlener Bauplan (Reihenfolge):**
1. Syntax + Typsystem festzurren (dieser Ordner, [SPRACHE.md](SPRACHE.md)).
2. Lexer/Parser → AST.
3. Hindley-Milner-Inferenz + Trait-Auflösung + Monomorphisierung.
4. AST→`crates/ir` in SSA (Solver/Backend unverändert wiederverwenden).
5. Minimal-Stdlib über libc + C-FFI; danach C-Header-Binding-Generator.
6. Selbst-Benchmark gegen die bestehende Suite (Ziel: ≤ die heutigen Java-Zahlen,
   erwartbar besser wegen SSA).

Siehe [SPRACHE.md](SPRACHE.md) für die konkrete Syntax und [beispiele/](beispiele/)
für lauffähig gedachte Programme über alle Zielbereiche.
