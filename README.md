# Vire

**Vire** ist eine Programmiersprache: *so leicht wie Python, so schnell wie C/Rust,
speichersicher — ohne Lifetimes, ohne Ownership-Syntax, ohne manuelle
Speicherverwaltung.* Sie kompiliert **AOT** zu nativen Binaries über einen
Whole-Program-Solver und ein LLVM-Backend und kommt (für den beweisbaren Großteil)
**ohne Runtime** aus.

> Name von lateinisch *vīrēs* („Kräfte, Stärke") — leicht, aber mächtig.
> Dateiendung `.vr`. Arbeitsstand: Sprache spezifiziert, Backend gebaut & gemessen.

```vire
fn word_counts(text: Str) -> Map[Str, Int] {
    mut counts = {}
    for word in text.lower().split_whitespace() {
        counts[word] = counts.get(word).or(0) + 1
    }
    counts
}
```

Liest wie Python — kompiliert zu einer speichersicheren, RC-eliminierten nativen
Binary.

## Idee in einem Absatz

Speichersicherheit gibt es klassisch nur mit einer von drei Kröten: Garbage
Collector (Runtime/Pausen), Ownership+Lifetimes (Rusts Annotationslast) oder
Referenzzählung (kleine Runtime). Vire löst das **pro Programmstelle**: ein
Whole-Program-Solver **beweist** Ownership, wo möglich (→ 0 Runtime, wie Rust), und
fällt auf schlanke RC zurück, wo nötig. Der Programmierer schreibt **null**
Speicher-Annotationen. Typen sind vollständig **inferiert** (Python-Ergonomie ohne
Pythons Dynamikkosten). Das ist machbar, weil Vire **Closed-World** ist (alle
Quellen zur Compilezeit) und auf einem Backend aufsetzt, das genau diese Beweise
schon liefert.

## Status & Architektur

Vire = **neues Front-End** auf einem **bereits gebauten, gemessenen Backend**:

| Schicht | Status |
|---|---|
| **Vire-Front-End** (Lexer, Parser, Typinferenz, `comptime`, Makros → SSA-IR) | **spezifiziert** (dieser Ordner `sprache/`), noch nicht implementiert |
| **Mittel-IR** (`crates/ir`) | gebaut |
| **Whole-Program-Solver** (Devirt, Inlining, Escape/RC-Elision, Bounds-/Null-Check-Elision, Azyklizität) | gebaut |
| **LLVM-Backend** (textuelles IR + clang, `-march=native`, LTO; hosted/freestanding/threads) | gebaut |

Das Backend wurde über einen **Java-Bytecode-Front-End-Prototyp** entwickelt und
gegen Rust **und** C++ gebenchmarkt (siehe [DESIGN.md](DESIGN.md) §9,
[benchmarks/](benchmarks/)): 7 von 10 Benchmarks auf/über Rust-Niveau,
Arithmetik/Allokation auch unter C++. Damit ist der teure, riskante Teil (Codegen,
Speichermodell, Sicherheits-Check-Elision) **belegt** — Vire erbt ihn direkt. Der
Java-Weg war das Beweismittel; das eigene, in SSA absenkende Front-End räumt dessen
Reibung weg (siehe [sprache/BEWERTUNG.md](sprache/BEWERTUNG.md) §3).

## Dokumente

- **[sprache/BEWERTUNG.md](sprache/BEWERTUNG.md)** — Machbarkeit ehrlich: die drei
  Spannungen (keine Runtime / alle Libs / Python-leicht) und wie Vire sie auflöst.
- **[sprache/SPRACHE.md](sprache/SPRACHE.md)** — Syntax-Tour (Schnelleinstieg).
- **[sprache/REFERENZ.md](sprache/REFERENZ.md)** — vollständige Syntax-/Feature-
  Referenz.
- **[sprache/FEATURES-BEWERTUNG.md](sprache/FEATURES-BEWERTUNG.md)** — Bewertung der
  acht gewünschten Features (Multithreading, Templates, comptime-Reflection,
  Makros, Meson, Logger, Go-Error-Handling, Debug-Crash-Pfade).
- **[sprache/beispiele/](sprache/beispiele/)** — lauffähig gedachte Programme über
  alle Bereiche und Features.
- **[DESIGN.md](DESIGN.md)** — Architektur des Backends (Solver, Speichermodell,
  Benchmarks). Beschreibt den heutigen Java-Bytecode-Pfad = das Beweismittel/die
  Bootstrap-Basis.
- **[benchmarks/](benchmarks/)** — Benchmark-Suite (Java/Rust/C++), Runner, Analyse.

## Kernideen der Sprache (Kurzform)

- **Inferenz statt Annotation** — Typen stehen nirgends, sind aber alle bekannt.
- **Kein `null`** — `Option[T]`; keine Exceptions — Fehler sind Werte (Go-Geist) mit
  `?`-Propagation.
- **`type`** für Produkt- und Summentypen (Werttypen, kein Objekt-Header),
  **Traits** + monomorphisierte **Generics**.
- **`comptime`** — Code, der im Compiler läuft: Reflection, Ableitungen, bedingte
  Compilierung — zero-cost, kein Runtime-Metadaten-Ballast.
- **Unsichtbarer Speicher** — Stack/Heap/RC entscheidet der Solver; `&` optional.
- **Nebenläufigkeit sicher by construction** — Kanäle (CSP) + `Mutex`/`Atomic`, der
  Solver lehnt geteilten nackten mutablen Zustand ab.
- **C nativ** — `extern "C"`/Header-Bindings; C++/Rust über C-ABI. Meson first-class.

Der Name/Details sind provisorisch und leicht änderbar; das Design ist der Kern.
