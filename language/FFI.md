# Foreign-Language Interop (C / C++ / Python)

Vire speaks the **C ABI** directly: an `extern "C"` declaration makes a C function
callable under its name (no mangling). The backend declares the called function,
`clang` links it. This is how Vire reaches C, and via the standard `extern "C"` bridge
route also C++ and Python — exactly how every serious language does cross-language
interop.

## C / libc / libm — direct
```vire
extern "C" {
    fn sqrt(x: F64) -> F64
    fn pow(base: F64, exp: F64) -> F64
    fn llabs(n: Int) -> Int
}
fn main() { print(sqrt(16.0)) }   // 4.0
```
`vire run c_math.vr` — `libm` is always linked. Further libraries: `-l NAME`.

## C++ — via an `extern "C"` facade
C++ has name mangling; the portable bridge is an `extern "C"` facade (standard). The
internals may be full C++ (STL etc.).
```cpp
// cpp_helper.cpp — std::vector/std::sort inside, C-ABI outside
extern "C" int64_t cpp_median_of_squares(int64_t n) { /* … STL … */ }
```
```vire
extern "C" { fn cpp_median_of_squares(n: Int) -> Int }
fn main() { print(cpp_median_of_squares(101)) }   // 2500
```
`vire build --obj cpp_helper.cpp -l stdc++ -o bin cpp_call.vr`

## Python — via the CPython C-API (shim)
Python libraries are reachable via the CPython C-API (even pure C). A small C shim
initializes the interpreter and calls the library:
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

## Self-contained: `link` in the source + embedded `native` blocks
So that a `.vr` file runs **without CLI flags and without extra files**:

**Link libs in the source** — `link "lib"` directly in the `extern` block:
```vire
extern "C" link "m" {
    fn cbrt(x: F64) -> F64
}
print(cbrt(27.0))            // 3.0 — `vire run` suffices, no -l needed
```

**Embedded foreign code** — `native "abi" [link "lib"]* """ …code… """`. The block is
automatically compiled and linked (extension by ABI). No separate file:
```vire
native "c++" """
#include <vector>
#include <algorithm>
extern "C" long median_sq(long n) { /* …STL… */ }
"""
extern "C" { fn median_sq(n: Int) -> Int }
print(median_sq(101))        // 2500 — C++ stdlib linked automatically
```

**Python fully automatic** — `native "python"` pulls the include path + `libpython`
itself (from `python3`/sysconfig):
```vire
native "python" """
#include <Python.h>
extern double pyval(double x) { /* math.sqrt(x) via CPython C-API */ }
"""
extern "C" { fn pyval(x: F64) -> F64 }
print(pyval(625.0))          // 25.0 — no -I, no -lpython, no extra file
```
`"""…"""` is a multi-line raw string (no escapes) — ideal for foreign code.

## Type mapping (scalar, clean)
`Int`→`int64_t`, `I32`→`int`, `F64`→`double`, `F32`→`float`, `Bool`→`int`.
Pointer/string interop (Vire `Str` is an object with a header, not a `char*`) needs a
shim that passes the bytes — still open (extension: `cstr(s)` builtin).

## Flags
- `-l NAME` — link library (e.g., `-l stdc++`, `-l python3.14`).
- `--obj FILE` — link object/source (`.o`, `.c`, `.cpp`, `.a`).
- `libm` is always linked.

## Python libs from PURE Vire — without your own C code (built-in bridge)
For Python there is a **bridge built into the compiler**: declare the `vire_py_*`
functions and call Python directly from Vire — `pybridge.c` is compiled in
automatically and libpython linked. **No user C, no shim, no flag.**
```vire
extern "C" {
    fn vire_py_eval_f(code: Str, x: F64) -> F64
    fn vire_py_eval_i(code: Str, x: Int) -> Int
}
print(vire_py_eval_f("__import__('math').sqrt(x)", 625.0))       // 25.0
print(vire_py_eval_i("__import__('math').factorial(x)", 6))      // 720
```
`x` is bound as an argument in the expression; `__import__('lib')` reaches ANY
installed Python library (numpy, …). The result comes back as a scalar.

**Summary "usable without your own foreign code?"**
- **C libs:** yes, directly — just declare signatures in `extern "C"` + `link`.
- **C++ libs:** only if the lib exports a C API; mangled C++ needs, by principle (no
  stable ABI), an `extern "C"` facade (`native` block).
- **Python libs:** yes, via the built-in bridge (`vire_py_*`) from pure Vire.

## Bind C headers automatically — no signatures by hand
Two levels so that you do NOT have to declare C functions one by one:

**`vire bindgen`** generates an `extern "C"` block from a C header:
```sh
vire bindgen geo.h -l geo -o geo_bind.vr    # → fn geo_hypot(a0: F64, a1: F64) -> F64 …
```
Covers scalar + pointer APIs; struct-by-value/function-pointers/varargs are skipped
(not cleanly mappable to the C ABI).

**`extern "C" header "…"`** does this automatically at compile time — you name only
the header, all functions are there:
```vire
extern "C" header "geo.h"
print(geo_hypot(3.0, 4.0))     // 5.0 — no signature typed
```
`vire run --obj geo.c c_header_auto.vr` (or link a precompiled lib).

## Ergonomics levels (summary)
| Goal | Effort |
|---|---|
| Call a C function | `extern "C" { fn f(...) }` + `link` — or `header "h.h"` (auto) |
| Embedded C/C++/Python shim | `native "abi" """…"""` (auto-compiled/linked) |
| Use a Python lib | `py_import("mod")` from pure Vire — no extern, no cstr, no C |
| Pass a string to C | Vire Str directly (at `header`/declaration `Ptr`), or `cstr(s)` |

## Safety: `Ptr` and Python objects are UNSAFE (deliberately)
Vire is memory-safe by construction — **except at the FFI boundary**. This holds
sharply for:
- **`Ptr`** (opaque raw pointer): the RC/collector does NOT know it. A `Ptr` is a bare
  C pointer; lifetime/validity are the user's responsibility.
- **Python objects** (`py_import`/`py_getattr`/`py_call_*` return `Ptr`): they carry a
  **CPython refcount** that Vire does NOT manage. A `py_getattr` result that lands in
  a Vire variable and falls out of scope is **not** `Py_DECREF`'d → it **leaks** (and
  a manual early DECREF would be use-after-free on the Python side).

This is expected unsafe-FFI territory — the same boundary at which every memory-safe
language (Rust `unsafe`, …) ends. Treat `Ptr`/Python handles like C pointers:
short-lived, clearly owned, not hoarded across scopes. A safe, RC-integrated `Py[T]`
wrapper type (with Drop → `Py_DECREF`) is the clean solution and still open.
