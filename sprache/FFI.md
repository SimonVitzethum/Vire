# Fremdsprachen-Interop (C / C++ / Python)

Vire spricht die **C-ABI** direkt: eine `extern "C"`-Deklaration macht eine C-
Funktion unter ihrem Namen aufrufbar (kein Mangling). Das Backend deklariert die
gerufene Funktion, `clang` linkt sie. So erreicht Vire C, und über den Standard-
`extern "C"`-Brückenweg auch C++ und Python — genau wie jede ernsthafte Sprache
Cross-Language-Interop macht.

## C / libc / libm — direkt
```vire
extern "C" {
    fn sqrt(x: F64) -> F64
    fn pow(base: F64, exp: F64) -> F64
    fn llabs(n: Int) -> Int
}
fn main() { print(sqrt(16.0)) }   // 4.0
```
`vire run c_math.vr` — `libm` wird immer gelinkt. Weitere Bibliotheken: `-l NAME`.

## C++ — über eine `extern "C"`-Fassade
C++ hat Name-Mangling; die portable Brücke ist eine `extern "C"`-Fassade (Standard).
Die Interna dürfen voll C++ sein (STL etc.).
```cpp
// cpp_helper.cpp — std::vector/std::sort innen, C-ABI außen
extern "C" int64_t cpp_median_of_squares(int64_t n) { /* … STL … */ }
```
```vire
extern "C" { fn cpp_median_of_squares(n: Int) -> Int }
fn main() { print(cpp_median_of_squares(101)) }   // 2500
```
`vire build --obj cpp_helper.cpp -l stdc++ -o bin cpp_call.vr`

## Python — über die CPython-C-API (Shim)
Python-Bibliotheken sind über die CPython-C-API erreichbar (selbst reines C). Ein
kleiner C-Shim initialisiert den Interpreter und ruft die Bibliothek:
```c
// py_shim.c
#include <Python.h>
extern double py_math_sqrt_times(double x, double k) { /* math.sqrt(x)*k */ }
```
```vire
extern "C" { fn py_math_sqrt_times(x: F64, k: F64) -> F64 }
fn main() { print(py_math_sqrt_times(256.0, 3.0)) }   // 48.0
```
```sh
clang -c -O2 -I$(python3 -c 'import sysconfig;print(sysconfig.get_config_var("INCLUDEPY"))') py_shim.c -o py_shim.o
vire build --obj py_shim.o -l python3.14 -o bin py_call.vr
```

## Selbstständig: `link` in der Quelle + eingebettete `native`-Blöcke
Damit eine `.vr`-Datei **ohne CLI-Flags und ohne Extra-Dateien** läuft:

**Link-Libs in der Quelle** — `link "lib"` direkt im `extern`-Block:
```vire
extern "C" link "m" {
    fn cbrt(x: F64) -> F64
}
print(cbrt(27.0))            // 3.0 — `vire run` genügt, kein -l nötig
```

**Eingebetteter Fremdcode** — `native "abi" [link "lib"]* """ …code… """`. Der Block
wird automatisch mitkompiliert und gelinkt (Endung nach ABI). Kein separates File:
```vire
native "c++" """
#include <vector>
#include <algorithm>
extern "C" long median_sq(long n) { /* …STL… */ }
"""
extern "C" { fn median_sq(n: Int) -> Int }
print(median_sq(101))        // 2500 — C++-Stdlib automatisch gelinkt
```

**Python komplett automatisch** — `native "python"` zieht Include-Pfad + `libpython`
selbst (aus `python3`/sysconfig):
```vire
native "python" """
#include <Python.h>
extern double pyval(double x) { /* math.sqrt(x) via CPython-C-API */ }
"""
extern "C" { fn pyval(x: F64) -> F64 }
print(pyval(625.0))          // 25.0 — kein -I, kein -lpython, kein Extra-File
```
`"""…"""` ist ein mehrzeiliger Roh-String (keine Escapes) — ideal für Fremdcode.

## Typ-Abbildung (skalar, sauber)
`Int`→`int64_t`, `I32`→`int`, `F64`→`double`, `F32`→`float`, `Bool`→`int`.
Zeiger-/String-Interop (Vire-`Str` ist ein Objekt mit Header, kein `char*`) braucht
einen Shim, der die Bytes übergibt — noch offen (Erweiterung: `cstr(s)`-Builtin).

## Flags
- `-l NAME` — Bibliothek linken (z.B. `-l stdc++`, `-l python3.14`).
- `--obj FILE` — Objekt/Quelle mitlinken (`.o`, `.c`, `.cpp`, `.a`).
- `libm` wird immer gelinkt.

## Python-Libs aus REINEM Vire — ohne eigenen C-Code (eingebaute Brücke)
Für Python gibt es eine **in den Compiler eingebaute Brücke**: deklariere die
`vire_py_*`-Funktionen und rufe Python direkt aus Vire — `pybridge.c` wird
automatisch mitkompiliert und libpython gelinkt. **Kein Nutzer-C, kein Shim, kein Flag.**
```vire
extern "C" {
    fn vire_py_eval_f(code: Str, x: F64) -> F64
    fn vire_py_eval_i(code: Str, x: Int) -> Int
}
print(vire_py_eval_f("__import__('math').sqrt(x)", 625.0))       // 25.0
print(vire_py_eval_i("__import__('math').factorial(x)", 6))      // 720
```
`x` ist im Ausdruck als Argument gebunden; `__import__('lib')` erreicht JEDE
installierte Python-Bibliothek (numpy, …). Ergebnis kommt als Skalar zurück.

**Zusammenfassung „ohne eigenen Fremdcode nutzbar?"**
- **C-Libs:** ja, direkt — nur Signaturen in `extern "C"` deklarieren + `link`.
- **C++-Libs:** nur wenn die Lib ein C-API exportiert; gemangeltes C++ braucht
  prinzipbedingt (keine stabile ABI) eine `extern "C"`-Fassade (`native`-Block).
- **Python-Libs:** ja, über die eingebaute Brücke (`vire_py_*`) aus reinem Vire.

## C-Header automatisch binden — keine Signaturen von Hand
Zwei Stufen, damit man C-Funktionen NICHT einzeln deklarieren muss:

**`vire bindgen`** erzeugt aus einem C-Header einen `extern "C"`-Block:
```sh
vire bindgen geo.h -l geo -o geo_bind.vr    # → fn geo_hypot(a0: F64, a1: F64) -> F64 …
```
Deckt skalare + Zeiger-APIs ab; struct-by-value/Funktionszeiger/varargs werden
übersprungen (nicht sauber auf die C-ABI abbildbar).

**`extern "C" header "…"`** macht das zur Compilezeit automatisch — man nennt nur
den Header, alle Funktionen sind da:
```vire
extern "C" header "geo.h"
print(geo_hypot(3.0, 4.0))     // 5.0 — keine Signatur getippt
```
`vire run --obj geo.c c_header_auto.vr` (oder eine vorkompilierte Lib linken).

## Ergonomie-Stufen (Zusammenfassung)
| Ziel | Aufwand |
|---|---|
| C-Funktion aufrufen | `extern "C" { fn f(...) }` + `link` — oder `header "h.h"` (auto) |
| Eingebetteter C/C++/Python-Shim | `native "abi" """…"""` (auto-kompiliert/gelinkt) |
| Python-Lib nutzen | `py_import("mod")` aus reinem Vire — kein extern, kein cstr, kein C |
| String an C übergeben | Vire-Str direkt (bei `header`/Deklaration `Ptr`), oder `cstr(s)` |

## Sicherheit: `Ptr` und Python-Objekte sind UNSAFE (bewusst)
Vire ist per Konstruktion speichersicher — **außer an der FFI-Grenze**. Das gilt
scharf für:
- **`Ptr`** (opaker Roh-Zeiger): der RC/Kollektor kennt ihn NICHT. Ein `Ptr` ist
  ein nackter C-Zeiger; Lebenszeit/Gültigkeit liegen beim Nutzer.
- **Python-Objekte** (`py_import`/`py_getattr`/`py_call_*` liefern `Ptr`): sie
  tragen einen **CPython-Refcount**, den Vire NICHT verwaltet. Ein `py_getattr`-
  Ergebnis, das in einer Vire-Variable landet und aus dem Scope fällt, wird
  **nicht** `Py_DECREF`'t → es **leckt** (und ein manuelles frühes DECREF wäre
  use-after-free auf der Python-Seite).

Das ist erwartbares unsafe-FFI-Territorium — dieselbe Grenze, an der jede
speichersichere Sprache (Rust `unsafe`, …) endet. Behandle `Ptr`/Python-Handles
wie C-Zeiger: kurzlebig, klar besessen, nicht über Scopes hinweg gehortet. Ein
sicherer, RC-integrierter `Py[T]`-Wrapper-Typ (mit Drop → `Py_DECREF`) ist die
saubere Lösung und noch offen.
