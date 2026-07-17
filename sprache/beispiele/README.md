# Vire-Beispiele

Illustrative Programme über alle Zielbereiche und die acht Features (siehe
[../SPRACHE.md](../SPRACHE.md), [../REFERENZ.md](../REFERENZ.md),
[../FEATURES-BEWERTUNG.md](../FEATURES-BEWERTUNG.md)). Die Sprache ist **noch nicht
implementiert** — die Dateien zeigen die Zielsyntax: nicht schwerer als Python,
aber statisch, sicher und nativ kompilierbar.

## Kern-/Bereichs-Beispiele
| Datei | Bereich | Zeigt |
|---|---|---|
| [sieb.vr](sieb.vr) | systemnah/numerisch (C/Rust) | gezählte Schleifen, Arrays, Bounds-Check-Elision |
| [formen.vr](formen.vr) | funktional/OOP (Rust) | Summentypen, Traits, Generics, Pattern-Matching |
| [baum.vr](baum.vr) | Datenstrukturen | rekursive Generics, Closures, automatischer Heap+RC |
| [wortzahl.vr](wortzahl.vr) | hoch/Skript (Python/Go) | Maps, Iteratoren, Comprehensions, Option, `?` |
| [ffi.vr](ffi.vr) | Interop (C/C++/Rust via C-ABI) | `extern "C"`, Header-Bindings, `unsafe` an der Grenze |

## Feature-Demos (die acht Punkte)
| Datei | Feature | Zeigt |
|---|---|---|
| [nebenlaeufig.vr](nebenlaeufig.vr) | 1 Multithreading + Race-Sicherheit | Kanäle (move), `Mutex`/`parallel_map`, abgelehnter geteilter Zustand |
| [comptime_matrix.vr](comptime_matrix.vr) | 2 Templates | Wert-Generics, comptime-Matrixgrößen, dimensionsgeprüfte Mul |
| [reflektion.vr](reflektion.vr) | 3 Compile-Time-Reflection | `@typeinfo`, `@derive`, comptime-JSON-Serialisierung |
| [makros.vr](makros.vr) | 4 Makros | hygienische AST-Makros + `comptime if` statt Präprozessor |
| [fehler.vr](fehler.vr) | 7 Error handling à la Go | Fehler als Werte, `?`, Wrapping, typisierte Fehler |
| [logger.vr](logger.vr) | 6 Logger | strukturiert, comptime-gefilterte Level, Spans, Sinks |

(Feature 5 „Meson first-class" und 8 „Debug/Crash-Pfade" sind Build-/Backend-Themen
— beschrieben in [../REFERENZ.md](../REFERENZ.md) §15–16, kein Sprach-Sample.)

Gemeinsamer Nenner: **kein manuelles Speichermanagement, keine Lifetimes, keine
Typannotationen im Alltag** — und trotzdem AOT-kompiliert zu speichersicheren,
RC-eliminierten nativen Binaries über FastLLVMs Solver + Backend.
