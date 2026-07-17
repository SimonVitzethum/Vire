# Lume-Beispiele

Illustrative Programme über alle Zielbereiche der Sprache (siehe
[../SPRACHE.md](../SPRACHE.md)). Die Sprache ist **noch nicht implementiert** —
diese Dateien zeigen, wie sich die Syntax anfühlen soll: nicht schwerer als
Python, aber statisch, sicher und nativ kompilierbar.

| Datei | Bereich | Zeigt |
|---|---|---|
| [sieb.lm](sieb.lm) | systemnah/numerisch (C/Rust) | gezählte Schleifen, Arrays, Bounds-Check-Elision |
| [formen.lm](formen.lm) | funktional/OOP (Rust) | Summentypen, Traits, Generics, Pattern-Matching |
| [baum.lm](baum.lm) | Datenstrukturen | rekursive Generics, Closures, automatischer Heap+RC |
| [wortzahl.lm](wortzahl.lm) | hoch/Skript (Python/Go) | Maps, Iteratoren, Comprehensions, Option, `?` |
| [nebenlaeufig.lm](nebenlaeufig.lm) | Nebenläufigkeit (Go) | leichte Threads, Kanäle, paralleles Reduce |
| [ffi.lm](ffi.lm) | Interop (C/C++/Rust via C-ABI) | `extern "C"`, Header-Bindings, `unsafe` nur an der Grenze |

Gemeinsamer Nenner: **kein manuelles Speichermanagement, keine Lifetimes, keine
Typannotationen im Alltag** — und trotzdem AOT-kompiliert zu speichersicheren,
RC-eliminierten nativen Binaries über FastLLVMs bestehenden Solver + Backend.
