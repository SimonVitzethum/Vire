# C++-Bibliotheks-Interop — Bewertung + Plan

*Nutzerwunsch: „suche nach Möglichkeiten eine gute C++-Lib-Interop zu erhalten."*

## Was heute geht (verifiziert)
`native "c++" """ … """`-Blöcke mit einer `extern "C"`-Fassade: STL/Templates innen,
C-ABI außen; auto-kompiliert (`want_cpp` → clang++), auto-gelinkt (`-lstdc++`).
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
Das **läuft** und ist mächtig (voll C++ innen). Grenze: pro genutzter Funktion muss
man die `extern "C"`-Fassade **von Hand** schreiben.

## Warum „direkte" C++-Interop prinzipiell nicht geht
C++ hat **keine stabile ABI**: Name-Mangling (compiler-/versionsspezifisch),
Templates (kein Symbol bis instanziiert), Ausnahmen, RTTI, nicht-triviale
Objekt-Layouts/Vererbung, Inline-Funktionen ohne Symbol. **Keine** Sprache ruft
beliebiges C++ direkt — Rust (`cxx`/`autocxx`), Swift, Python (pybind11) etc. gehen
ALLE über eine generierte C-ABI-Brücke. Die Frage ist also nicht „direkt vs
Fassade", sondern **„Fassade von Hand vs generiert".**

## Optionen für *ergonomischere* Interop (bewertet)

**(A) Status quo — handgeschriebene `extern "C"`-Fassade.** Maximale Kontrolle,
null Magie, funktioniert für alles. Kosten: Handarbeit pro Funktion. *Für kleine
Oberflächen ok.*

**(B) Bridge-Generator aus einer kleinen IDL (empfohlen — der `cxx`-Weg).** Der
Nutzer deklariert die gewünschte C++-Oberfläche knapp; Vire generiert die
`extern "C"`-Trampoline (C++-Seite, über den bestehenden `native`-Pfad kompiliert)
UND die Vire-`extern`-Deklarationen. Beispiel-Zielsyntax:
```
cxx "mylib.h" link "mylib" {
    fn make_widget(w: Int, h: Int) -> Ptr        // → new Widget(w,h), Zeiger raus
    fn Widget.area(self: Ptr) -> Int             // → ((Widget*)self)->area()
    fn Widget.free(self: Ptr)                     // → delete (Widget*)self
}
```
Generiert C++:
```cpp
#include "mylib.h"
extern "C" void* make_widget(long w,long h){ return new Widget(w,h); }
extern "C" long Widget_area(void* s){ return ((Widget*)s)->area(); }
extern "C" void  Widget_free(void* s){ delete (Widget*)s; }
```
+ Vire-`extern`-Sigs (`Ptr`=opaker Zeiger, schon vorhanden). Deckt die **90 %**:
freie Funktionen, Konstruktoren/Destruktoren, Methoden mit skalaren/Zeiger-Args.
Kein libclang nötig (heuristischer Generator wie der bestehende C-`bindgen`).
*Mittlerer Aufwand, größter Ergonomie-Gewinn.*

**(C) libclang-basiertes autocxx.** Header mit dem echten Clang-AST parsen →
vollständige, typgenaue Bindings (Overloads, Namespaces, Templates instanziiert).
Robustester Weg (= Rusts `autocxx`), aber **schwere Abhängigkeit** (libclang) und
großer Aufwand. *Später, wenn (B) an Grenzen stößt.*

**(D) Nur-C-Wrapper-Bibliotheken.** Viele große C++-Libs bieten bereits eine
offizielle C-API (z.B. `llvm-c`, `libclang`). Diese laufen HEUTE über den normalen
`extern "C"`/`bindgen`-Pfad, ohne C++-Spezifik. *Kostenlos, wo verfügbar.*

## Empfehlung (Reihenfolge, gate-getreu)
1. **Jetzt:** Status quo (A) dokumentieren + die opake-`Ptr`-Konvention (Objekt-
   Handles über C-ABI) als Muster festhalten — deckt reale Fälle schon ab.
2. **Nächster fokussierter Schritt:** den **Bridge-Generator (B)** bauen — die kleine
   `cxx {}`-IDL → C++-Trampoline + Vire-externs. Reuse: der `native`-Kompilier-/
   Link-Pfad steht, der C-`bindgen`-Heuristik-Parser ist die Vorlage. Das ist der
   beste Aufwand/Ergonomie-Punkt und braucht KEINE neue Abhängigkeit.
3. **Erst wenn nötig:** libclang-autocxx (C) für Overloads/Templates/Namespaces.

## Ehrliche Abgrenzung
- Objekte über die Grenze sind **opake `Ptr`** (kein Vire-RC — Lebenszeit manuell
  via `Widget.free`, wie `Ptr`/PyObject dokumentiert). Automatisches RC über die
  C++-Grenze ginge nur mit generierten Deleter-Hooks (späterer Schritt).
- Exceptions über die Grenze: `extern "C"`-Trampoline müssen C++-Exceptions fangen
  (`try/catch` → Fehlercode), sonst UB. Der Generator (B) sollte das automatisch
  umschließen.
- Templates: nur konkret instanziiert brückbar (der Trampolin instanziiert sie).
