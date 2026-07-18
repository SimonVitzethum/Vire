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

## Typ-Abbildung (skalar, sauber)
`Int`→`int64_t`, `I32`→`int`, `F64`→`double`, `F32`→`float`, `Bool`→`int`.
Zeiger-/String-Interop (Vire-`Str` ist ein Objekt mit Header, kein `char*`) braucht
einen Shim, der die Bytes übergibt — noch offen (Erweiterung: `cstr(s)`-Builtin).

## Flags
- `-l NAME` — Bibliothek linken (z.B. `-l stdc++`, `-l python3.14`).
- `--obj FILE` — Objekt/Quelle mitlinken (`.o`, `.c`, `.cpp`, `.a`).
- `libm` wird immer gelinkt.
