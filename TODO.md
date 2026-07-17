# Vire — Fahrplan (Features 1–8 + Compiler-Pipeline)

Aufgabenliste für die Umsetzung. Reihenfolge nach Abhängigkeit und Risiko.
Design-Grundlage: [sprache/](sprache/). **Backend/Solver existieren**; neu ist das
Front-End. Legende: `[ ]` offen · `[~]` teilweise · `[x]` fertig.

---

## M0 — Risiko-Messung (Gate) — ✅ AUSGEFÜHRT, Urteil: **bedingtes Weiter**

Vollständiger Bericht: **[sprache/M0-MESSUNG.md](sprache/M0-MESSUNG.md)**. Programme:
[benchmarks/m0/](benchmarks/m0/). Gemessen über die **reale automatische Pipeline**
(Solver macht die Inferenz — nicht Hand-Absenkung), Oracle↔Automatisch-Spread.

- [x] **M0.1 Alias-Präzision.** Adversarialer PageRank-Objektgraph (geteilt/
  entkommend/mutierend/zyklisch). Ergebnis: **>1000× langsamer** als Rust bei 100k
  (Kollektor super-linear/Timeout), **4,4×** ohne Kollektor, **6,3×** atomare RC
  (uncontended). Der Spread Oracle(=0 RC)↔Automatisch ist maximal → die Inferenz
  gewinnt die Borrow-Fakten im geteilt/zyklischen Fall **nicht** zurück. „Rust-
  Niveau ohne Annotationen" = **Slogan** auf dieser Teilmenge.
- [x] **M0.2 Compile-Zeit.** Solver+Backend super-linear (~O(n^1,4)): 50k LOC =
  1,8 s, extrapoliert ~5–7 s bei 100k — **ohne** inkrementelles Caching.
- [~] **M0.1-Contention** (Rest): echte Multithread-Contention als separater Versuch
  offen; 6,3× uncontended ist die Untergrenze.
- [x] **M0.3 Entscheidung:** **nicht grün.** Pflicht VOR dem Front-End: (i)
  **Kollektor-Skalierung** (O(n²)-Full-Scan → inkrementell/generationell), (ii)
  **Borrow-/Region-Inferenz** schärfen (dauerhaft-lebendig→borgen), (iii) Overflow-
  Default + `+%`-Kultur (Vektorisierung, s. M0-Bericht), (iv) Analyse-Caching für
  Compile-Zeit. Dann M0.1 erneut messen.

**Kernrisiko rot bestätigt.** Front-End (P1+) ist bis (i)+(ii) **zurückgestellt**.

---

## Compiler-Pipeline (Front-End neu, Rest wiederverwendet)

### P1 — Lexer + Parser → AST  → Plan: [sprache/PARSER.md](sprache/PARSER.md)
- [ ] Lexer (Token-Kinds, Unicode-Idents, Zahlen/Strings/Interpolation, Kommentare).
- [ ] Rekursiver-Abstieg-Parser + Pratt-Ausdrucksparser (Präzedenztabelle).
- [ ] AST-Definitionen (`crates/vire_ast`).
- [ ] Fehler-Recovery (Panic-Mode an `}`/`\n`; mehrere Fehler pro Lauf).
- [ ] `vire fmt` (Roundtrip AST→Quelltext) als Parser-Fuzz-Absicherung.

### P2 — Namensauflösung + Typinferenz + Monomorphisierung
- [ ] Namens-/Modulauflösung (ein Modul = Datei, ein Paket = Verzeichnis).
- [ ] **Bidirektionale HM-Inferenz** mit lokalen Ankern (Signaturen an Fn-/Modul-
  grenzen halten Fehler nah — s. [BEWERTUNG.md](sprache/BEWERTUNG.md) §5).
- [ ] Trait-Auflösung + Kohärenzregeln (das *echte* Risiko, nicht Vanilla-HM).
- [ ] Monomorphisierung (dockt an den vorhandenen Inliner-Ansatz an).
- [ ] **Gute Fehlermeldungen** (nahe Ursache, Fix-Vorschläge) — Ergonomie-kritisch.

### P3 — `comptime` + Makro-Expander (die „Präprozessor"-Ebene, Feature 4/2/3)
- [ ] `comptime`-Auswerter (Interpreter über den AST/Typgraphen; Rekursionslimit).
- [ ] `@typeinfo`/Reflection-API (Feature 3).
- [ ] Hygienischer Makro-Expander (Feature 4).
- [ ] `@if`/`@when` bedingte Compilierung (Feature 4).

### P4 — Absenkung AST → `crates/ir` **in SSA**
- [ ] Lowering (Werttypen, Summentypen→getaggte Union, Closures, `match`→`switch`).
- [ ] **Iterator-Mutation-Check** ([REFERENZ.md](sprache/REFERENZ.md) §9a) — lokale
  Nicht-Mutations-Analyse; nicht beweisbar → Compilefehler.
- [ ] SSA-Erzeugung (macht den GVN-gegen-Slot-Reuse-Kampf des Java-Pfads überflüssig).
- [ ] Solver + Backend unverändert anhängen (Devirt/Escape/RC/Bounds/Backend).

### P5 — Stdlib + FFI
- [ ] Kern-Stdlib (Str, List/Map/Set, Iteratoren, Option/Result) über libc.
- [ ] `extern "C"` + `unsafe`-Grenze.
- [ ] C-Header→Binding-Generator (Feature 5-Voraussetzung, Interop-Kern).

---

## Features 1–8 (jeweils mit Andockpunkt + Kernaufgaben)

### [1] Multithreading, safe by construction 🟢* *(leicht + Kanäle/Mutex genügt — bestätigt)*
Andock: FastLLVM `--threads` (atomare RC, pthreads, Monitor) — **vorhanden**.
- [ ] `Channel[T]`, `spawn`, `Mutex[T]`, `Atomic[T]` in der Stdlib.
- [ ] `parallel_map`/`parallel_for` (Fork-Join).
- [ ] **Send-Prüfung**: ein an `spawn` übergebener Wert muss gemoved/kopiert *oder*
  ein Sync-Typ sein — sonst Compilefehler. *Konservativ* (dieselbe Analyse wie der
  Iterator-Check §9a; im Zweifel Mutex/move verlangen). **Keine** Totalgarantie über
  beliebige Alias-Graphen — bewusst (BEWERTUNG §7.1).
- [ ] M0.1 klärt vorab die Atomic-Contention-Kosten.

### [2] Template-Programmierung 🟢
Andock: Monomorphisierung (P2) + `comptime` (P3).
- [ ] Generics `[T: Trait]`, Mehrfachschranken.
- [ ] Wert-Generics `[comptime N: Int]`, Fixarrays `[T; N]`.
- [ ] Monomorphisierung + statische Trait-Auflösung → Direktaufrufe.

### [3] Compile-Time-Reflection 🟢
Andock: Whole-Program-Typgraph (P2) + `comptime` (P3).
- [ ] `@typeinfo(T)` (Felder/Varianten/Methoden/Attribute, comptime-durchlaufbar).
- [ ] `@derive(Json, Eq, Hash, Ord, …)` über Reflection.
- [ ] `comptime for/if/assert`, `emit`. **Keine** Laufzeit-Reflection (AOT).

### [4] Eigener optionaler Präprozessor 🟢 *(= comptime/@if/Makros, kein C-Text)*
Andock: P3.
- [ ] Hygienische Makros (`macro name(args) { … }`), **hygienisch + typsicher**:
  - [ ] **typisierte Parameter** (`cond: expr`, `body: block`, `ident`, `pat`,
    `type`, oder konkreter Typ) → Fehlverwendung = Compilefehler am Aufrufort.
  - [ ] **volle Typprüfung nach Expansion** (kein ill-typisiertes Ergebnis möglich).
  - [ ] Hygiene (keine Namens-Einfänge), Diagnose-Spans bis in die Expansion.
- [ ] `@if`/`@when` (bedingte Compilierung, Plattform-Weichen) — ausdrucksbasiert, geprüft.
- [ ] `const`/`comptime {}` (Compilezeit-Werte/Codegen), voll typgeprüft. Doku: kein `#define`.

### [5] Build-Interop, Meson first-class 🟢🟡
Andock: clang→Objekt (vorhanden).
- [ ] Stabile Compiler-CLI (`--emit=obj|llvm|asm`, `-O`, `--deps` Ninja-`.d`).
- [ ] Meson-Modul `vire` (`vire.executable/static_library`), C-ABI-`.o`/`.a`.
- [ ] `vire build`-Wrapper delegiert an Meson; pkg-config-Deps → Binding-Generator.
- [ ] **Entscheidung:** Meson *adoptieren* statt eigenem Build (spart ein Subsystem).

### [6] Logger „in gut" 🟢
Andock: Stdlib + `comptime` (compile-time Level-Filter) + Debug-Info (Ort).
- [ ] Strukturierte Felder, Level, `with log.span(...)`.
- [ ] **Compile-Zeit-Level-Filter**: deaktivierte Aufrufe = 0 Instruktionen (comptime-`if`).
- [ ] Sinks (Konsole farbig / JSON / Datei), beim Build gewählt.

### [7] Fehlerbehandlung à la Go 🟢* *(Go-Geist, aber `Result` statt `nil`)*
Andock: Wert-Fehlermodell (Backend vorhanden), `?` als Absenkung.
- [ ] `Result[T,E]`/`Option[T]` + `?`-Operator (früher Rücksprung).
- [ ] `.wrap(msg)` (Kontext, Kette), typisierte Fehler + `match`.
- [ ] **Kein `nil`, kein `(T, Error)`-Tupel** (verletzt kein-null). `panic` nur für
  Programmierfehler.

### [8] Debug-Symbole + Crash-Pfade 🟢
Andock: LLVM-Debug-Metadaten (Backend-Ausbau), Panic-Modell.
- [ ] Zeilennummern Front-End→IR durchreichen; `!DILocation`/`!DISubprogram` emittieren.
- [ ] Debug-Runtime-Backtrace (`datei:zeile:funktion`) bei panic/Bounds/Null.
- [ ] Release standardmäßig aus (0 Overhead), `--release --backtrace` opt-in.
- [ ] freestanding: kompakte Symboltabelle statt libc-`backtrace`.

---

## Querschnitts-Risiken (früh retiren — aus BEWERTUNG §7)
- [ ] **Alias-Präzision** (Sicherheit *und* Tempo hängen daran) → M0.1.
- [ ] **Compile-Zeit** Whole-Program+Mono+comptime → M0.2 + Analyse-Caching prüfen.
- [ ] **Inferenz-Fehlerlokalität** → bidirektionale Anker + Fix-Vorschläge (P2).
- [ ] **Overflow-Default**: geprüft auch in Release, Wrapping nur explizit ([REFERENZ.md](sprache/REFERENZ.md) §3.1).

## Nicht-Ziele (bewusst)
Laufzeit-`eval`/-Reflection · dynamisches Nachladen unbekannten Codes · C-Text-
Präprozessor · Deadlock-Freiheits-Garantie · „alle" C++/Rust-Libs jenseits der
C-ABI-Grenze.
