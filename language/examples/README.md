# Vire examples

Illustrative programs across all target areas and the eight features (see
[../LANGUAGE.md](../LANGUAGE.md), [../REFERENCE.md](../REFERENCE.md),
[../FEATURES-EVALUATION.md](../FEATURES-EVALUATION.md)). The language is **not yet
implemented** — the files show the target syntax: no harder than Python,
but static, safe, and natively compilable.

## Core/area examples
| File | Area | Shows |
|---|---|---|
| [sieve.vr](sieve.vr) | systems/numeric (C/Rust) | counted loops, arrays, bounds-check elision |
| [shapes.vr](shapes.vr) | functional/OOP (Rust) | sum types, traits, generics, pattern matching |
| [tree.vr](tree.vr) | data structures | recursive generics, closures, automatic heap+RC |
| [wordcount.vr](wordcount.vr) | high-level/script (Python/Go) | maps, iterators, comprehensions, Option, `?` |
| [ffi.vr](ffi.vr) | interop (C/C++/Rust via the C ABI) | `extern "C"`, header bindings, `unsafe` at the boundary |

## Feature demos (the eight points)
| File | Feature | Shows |
|---|---|---|
| [concurrent.vr](concurrent.vr) | 1 multithreading + race safety | channels (move), `Mutex`/`parallel_map`, rejected shared state |
| [comptime_matrix.vr](comptime_matrix.vr) | 2 templates | value generics, comptime matrix sizes, dimension-checked mul |
| [reflection.vr](reflection.vr) | 3 compile-time reflection | `@typeinfo`, `@derive`, comptime JSON serialization |
| [macros.vr](macros.vr) | 4 macros | hygienic AST macros + `comptime if` instead of a preprocessor |
| [error.vr](error.vr) | 7 error handling à la Go | errors as values, `?`, wrapping, typed errors |
| [logger.vr](logger.vr) | 6 logger | structured, comptime-filtered levels, spans, sinks |

(Feature 5 "Meson first-class" and 8 "debug/crash paths" are build/backend topics
— described in [../REFERENCE.md](../REFERENCE.md) §15–16, not a language sample.)

Common denominator: **no manual memory management, no lifetimes, no
type annotations in everyday use** — and yet AOT-compiled to memory-safe,
RC-eliminated native binaries via FastLLVM's solver + backend.
