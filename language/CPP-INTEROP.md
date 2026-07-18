# C++ Library Interop — Assessment + Plan

*User request: "look for ways to obtain good C++ library interop."*

## What works today (verified)
`native "c++" """ … """` blocks with an `extern "C"` facade: STL/templates inside,
C-ABI outside; auto-compiled (`want_cpp` → clang++), auto-linked (`-lstdc++`).
```
native "c++" """
#include <vector>
#include <algorithm>
#include <numeric>
extern "C" long sum_sorted(long n) {
    std::vector<long> v;
    for (long i=n;i>0;i--) v.push_back(i);
    std::sort(v.begin(), v.end());
    return std::accumulate(v.begin(), v.end(), 0L);
}
"""
fn main() { print(sum_sorted(100)) }   // → 5050, std::vector+sort+accumulate
```
This **works** and is powerful (full C++ inside). Limitation: for each function used,
you must write the `extern "C"` facade **by hand**.

## Why "direct" C++ interop is impossible in principle
C++ has **no stable ABI**: name mangling (compiler-/version-specific), templates (no
symbol until instantiated), exceptions, RTTI, non-trivial object layouts/inheritance,
inline functions without a symbol. **No** language calls arbitrary C++ directly —
Rust (`cxx`/`autocxx`), Swift, Python (pybind11), etc. ALL go through a generated
C-ABI bridge. The question is therefore not "direct vs facade", but **"facade by hand
vs generated".**

## Options for *more ergonomic* interop (assessed)

**(A) Status quo — hand-written `extern "C"` facade.** Maximum control, zero magic,
works for everything. Cost: manual work per function. *Fine for small surfaces.*

**(B) Bridge generator from a small IDL (recommended — the `cxx` route).** The user
declares the desired C++ surface concisely; Vire generates the `extern "C"`
trampolines (C++ side, compiled via the existing `native` path) AND the Vire `extern`
declarations. Example target syntax:
```
cxx "mylib.h" link "mylib" {
    fn make_widget(w: Int, h: Int) -> Ptr        // → new Widget(w,h), pointer out
    fn Widget.area(self: Ptr) -> Int             // → ((Widget*)self)->area()
    fn Widget.free(self: Ptr)                     // → delete (Widget*)self
}
```
Generates C++:
```cpp
#include "mylib.h"
extern "C" void* make_widget(long w,long h){ return new Widget(w,h); }
extern "C" long Widget_area(void* s){ return ((Widget*)s)->area(); }
extern "C" void  Widget_free(void* s){ delete (Widget*)s; }
```
+ Vire `extern` sigs (`Ptr`=opaque pointer, already present). Covers the **90%**:
free functions, constructors/destructors, methods with scalar/pointer args. No
libclang needed (heuristic generator like the existing C `bindgen`).
*Medium effort, largest ergonomics gain.*

**(C) libclang-based autocxx.** Parse headers with the real Clang AST → complete,
type-accurate bindings (overloads, namespaces, instantiated templates). Most robust
route (= Rust's `autocxx`), but **heavy dependency** (libclang) and large effort.
*Later, when (B) reaches its limits.*

**(D) C-only wrapper libraries.** Many large C++ libs already offer an official C API
(e.g., `llvm-c`, `libclang`). These run TODAY via the normal `extern "C"`/`bindgen`
path, without C++ specifics. *Free where available.*

## Recommendation (order, gate-faithful)
1. **Now:** document the status quo (A) + capture the opaque-`Ptr` convention (object
   handles over C-ABI) as a pattern — it already covers real cases.
2. **Next focused step:** build the **bridge generator (B)** — the small `cxx {}` IDL
   → C++ trampolines + Vire externs. Reuse: the `native` compile/link path is in
   place, the C `bindgen` heuristic parser is the template. This is the best
   effort/ergonomics point and needs NO new dependency.
3. **Only when needed:** libclang-autocxx (C) for overloads/templates/namespaces.

## Honest scoping
- Objects across the boundary are **opaque `Ptr`** (no Vire RC — lifetime manual via
  `Widget.free`, as documented for `Ptr`/PyObject). Automatic RC across the C++
  boundary would only work with generated deleter hooks (later step).
- Exceptions across the boundary: `extern "C"` trampolines must catch C++ exceptions
  (`try/catch` → error code), otherwise UB. The generator (B) should wrap this
  automatically.
- Templates: bridgeable only concretely instantiated (the trampoline instantiates
  them).
