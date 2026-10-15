# AGENTS.md — Vire Neubau (gilt ab 2026-10-12; Target-Wechsel 2026-10-13)

## 0. Ziel (bindend, auch für README)

> **Vire: mindestens so schnell wie Rust, leichter als Python, vollständig getypt,**
> **speichersicher ohne `unsafe` — auch für Hardware und Register.**
> **Gabbro ist das Target. Java-IR, GPU/Vulkan, Runtime/GC, LLVM sind raus.**

* **Performance:** `≥ Rust` auf CPU (Benchmark-Suite gegen `rustc -O`, `clang++ -O2`,
  bit-identisch, RSS daneben — erst über den validierenden Gabbro-Compiler, nicht über
  C11: der C-Emitter ist **deprecated** und wird nicht genutzt). Kein Pessimismus-Pfad
  im Compiler.
* **Ergonomie:** leichter als Python lesen/schreiben (keine Lifetimes, kein Borrow-Syntax,
  keine manuellen `free`/`retain`, **keine Schleifen-/Kosten-/Vertrags-Annotationen**),
  aber **vollständig statisch getypt** — kein `Any`,
  kein stiller `Int`-Default, jeder Typ zur Compilezeit bekannt oder Compilefehler.
  Sichtbar ist nur die temporäre Heap-Grenze (`region(max)` u. ä.); Schleifenschranken,
  Kosten und Verträge beweist der Solver oder lehnt mit Code ab — nie der Mensch.
* **Sicherheit ohne `unsafe`:** Es gibt kein `unsafe`-Keyword, keinen blinden
  `native`-Escape, kein `int -> ptr`. Auch Hardware/Register/Syscall sind
  **getypte Sprachformen** (Vorbild: Gabbro — `reg`/`mmio`/`port`/`syscall` als
  deklarierte Items mit Vertrag, Region statt Zahl; der hiesige Compiler wird
  **nicht formal verifiziert**, trägt die Garantie aber per Checker + Tests).
* **Kein Java:** `crates/classfile`, `crates/frontend`, `fastjavac`, Java-Semantik-Ballast
  (Slots, Boxing, `int`-nur-Index) sind entfernt. IR ist Vire-only SSA.
* **Kein GPU/Vulkan (jetzt):** `@gpu`, `@vulkan`, NVPTX/SPIR-V-Emitter,
  `gpu_runtime.c`, `vk_runtime.c`, `shader.rs`, `cuda-oxide`-Vendoring, GPU/Vulkan-Tests,
  -Benchmarks und -Docs sind entfernt. Reines CPU-Projekt.
* **Keine Runtime/GC — am Ende keine `runtime.c`:** Weder RC/Collector/Slab noch
  sonst ein verlinktes C-Runtime-Modul. Die Datei `crates/runtime/runtime.c` wird
  **vollständig gelöscht** (nicht nur entkernt); im Build landet kein C-Runtime mehr —
  einzige Ausnahme war der benannte Eintritts-Stub (§3, mit LLVM entfallen).
  Auch unbeschränkte dynamische Strukturen laufen ohne C-Runtime: Der Compiler emittiert
  pro Programm **maximal eine** Speichereinheit (Stack/`free`/Pools/Drops, dedupliziert),
  alles zur Compilezeit bewiesen und memory-safe — **kein GC, kein RC, kein Collector,
  nirgends** (weder als C-Runtime noch emittiert). **`Rc[T]` nur im Kernel/**Treiber als
  expliziter Opt-in-Typ — nie implizit (s. §2.5).
* **Gabbro ist das Target (statt LLVM):** Vire emittiert Gabbro-Quelltext (`.gab`,
  konform zum Lean-modellierten Kern — SYNTAX.md + `Syntax.lean` sind die Quelle, wo sie
  schweigt, gewinnt das Modell). Der Vire-Compiler bleibt **unverifiziert**; Korrektheit
  fließt über die Gabbro-Toolchain, sobald deren Compiler tut (Prüfung + Zertifikat +
  validierende ARM-Emission — GabbroV/Logikbeweise, wenn fertig). Die Vire-Syntax wird
  dadurch **nicht schwerer**: Was Gabbro an Budgets/Verträgen/Decken verlangt, beweist
  der Solver oder lehnt mit Code ab — nie der Mensch per Annotation.
  Ehrlicher Scope: Dynamische Strukturen stehen nicht in Gabbros Zielsatz (OFFEN O29) —
  unbeschränkt-Unbekanntes und `Rc` werden auf dem Gabbro-Pfad **abgelehnt**, bis Gabbro
  sie trägt; die begrenzte Menge (konstante Decken überall) kompiliert vollständig.
* **`unsafe_code = "forbid"`** workspace-weit (Rust-Seite).

Wo README und diese Datei widersprechen, gewinnt diese Datei.

## 1. Was weg ist (nicht wieder einbauen ohne Entscheidung)

* `crates/classfile`, `crates/frontend`, Binär `fastjavac`, `tests/run.sh`-Java-Orakel
  als Gate (kann als externes Archiv leben, nicht als Build-Pfad).
* `crates/backend/src/nvptx.rs`, `spirv.rs`, `crates/driver/src/gpu_runtime.c`,
  `vk_runtime.c`, `crates/vire/src/shader.rs`, `crates/cuda-oxide/`,
  `@gpu`/`@vulkan`/`@vertex`/…-Attribute, `VkIface`/`GpuKernel`-IR, `spirv-as`/`llc`-NVPTX-Pfade.
* `crates/driver/src/runtime.c` (RC + Bacon-Rajan + Slab + Arenen als Runtime):
  ersetzt durch **emittierte statische Speicherverwaltung** (s. §2), danach wird die
  Datei **vollständig gelöscht** — am Ende existiert kein C-Runtime mehr (der
  Eintritts-Stub entfällt mit LLVM, Gabbro-Programme starten bei `main`).
* Blinde Escapes: `native "c"/"c++"/"asm"` ohne Beweis, `--noverify`/`--unsafe-c`,
  `extern` mit Zeiger-Arithmetik, `Ptr`-als-`i64`, `pybridge.c` als impliziter Shim.
  Ersatz: `hw`-Modul + Header-Bindings mit geprüften Verträgen (s. §4).
* LLVM-Backend: pausiert (Entscheidung 2026-10-13) — kein Ausbau, keine Fixes außer
  Bau-Fähigkeit. Text-IR-Emission, `clang`-Builds, `runtime.c` bleiben kompilierbar und
  ausführbar als **Differenzial-Orakel** für den Gabbro-Pfad (Bit-Parität), sonst
  unangetastet. Neue Compiler-Arbeit geht ausschließlich in den Gabbro-Pfad.

## 2. Speicher­modell (Userspace: Gabbro-Träger; Kernel: explizites Rc)

Vire-Objekte sind Gabbro-Träger (Tabellen + `ptr`-Handles, keine Records als Werte);
Regionen sind Gabbro-Arenen mit **konstanten** Decken (`capacity lo..hi`, `alloc`/`reset`,
`else` = `refuse`); Schleifen sind `traverse`/`retry bounded` mit statischer Schranke.
Was keine konstante Decke hat, lehnt der Gabbro-Pfad mit Code ab (O29-Scope).

1. **Owned values** (Standard): jeder Wert hat genau einen Besitzer, Move per Default,
   `free` zur Compilezeit am letzten Use (Solver beweist, Backend emittiert Gabbro-Formen
   direkt — kein Zähler).
2. **Borrows (inferiert, unsichtbar):** Feld-/Param-Lesezugriffe ohne Mutation sind
   Borrows, keine Owns. Keine Syntax dafür — der Checker entscheidet, Ablehnung bei
   Zweifel (lieber Compilefehler als Runtime).
3. **Regionen mit Decke:** `region(N)` / Schleifen-Arena nur mit statisch bekannter
   Schranke (`array(N)`, `region(max=…)`); Überlauf = geprüfter Fehlerpfad
   (`refuse`), nie UB, nie stilles Wachsen. Keine `mmap`-Zahl im Usercode.
4. **Verbotene Typen im Userspace:** zyklische Heap-Graphen mit dynamischer Lebensdauer
   ohne Decke → **Compilefehler** (mit Fix-Vorschlag: Decke nennen oder in Kernel-Modul
   auslagern), nicht RC-Fallback.
5. **`Rc[T]` nur Kernel:** explizit importiert (`use kernel::Rc`), nur in
   `kernel`-Modulen, mit Budget (inkrementell, gedeckelt). Userspace-Code mit `Rc`
   kompiliert nicht.
6. **Unbeschränkt dynamisch ohne C-Runtime:** Auch Strukturen ohne statische Schranke
   laufen ohne `runtime.c`. Unique Ownership trägt beliebige Größen (`free` am letzten
   Use braucht keinen Zähler, egal wie groß). Wo geteiltes Ownership unvermeidbar ist,
   steht nur der explizite Kernel-`Rc[T]` bereit — als **reiner Library-Typ in Vire**
   (Zählung als gewöhnlicher emittierter Code, kein Collector, Zyklen nur via `weak`/
   Decke). Pro Programm emittiert der Compiler **maximal eine** Speichereinheit
   (dedupliziert); ein zweites, drittes, verstecktes Allokations-Runtime gibt es nicht.

## 3. Hardware/Register ohne unsafe (Gabbro-Prinzip, ohne Formal­beweis)

* Rust-Seite: `#![forbid(unsafe_code)]` überall; kein handgeschriebenes C/Asm im
  ausgelieferten Binary außer **benanntem Eintritts-Stub** (vor Stack, mit Begründung
  + Test, gezählt).
* Vire-Seite: kein `unsafe`, kein `int->ptr`, kein Roh-`asm`. Stattdessen:
  `hw::Reg[T]`, `hw::Mmio[T]`, `hw::Port`, `syscall`-Items mit deklarierter
  Signatur + Kosten + Vertrag; `volatile`-/`fence`-Semantik aus der Deklaration,
  nicht aus C-Sequenzpunkten. Verstoß = Compilefehler mit Code (N-Nummer).
  Auf dem Gabbro-Pfad werden sie zu `device`/`reg`-Deklarationen und `syscall`-Gates
  (der Solver erzeugt ABI-Abbildung, Kosten und Contracts — nie der Mensch).
* FFI: nur `extern "C" header` mit geprüfter Abbildung (skalar + `extern`-Structs
  mit Layout); C++/Rust nur über C-ABI; was nicht abbildbar ist, wird **abgelehnt**,
  nicht durchgewunken.

## 4. Typisierung (vollständig)

* Kein Default-Typ. Uninferierbar → Fehler mit Spanne, kein `Int`-Fallback.
* `Result[T,E]` mit **zwei** Typparametern getypt (kein i64-Erase); `?`/`wrap`/`match`
  kennen die Mono-Instanz. `Option[T]` ebenso.
* Generische Traits + Kohärenz (überlappende Impls = Fehler), Value-Generics `[N]`
  monomorphisiert, Iterator-Invalidierung (Mutation während Iteration = Fehler,
  `snapshot()` als expliziter Ausweg), `Str.split -> List[Str]` über typisierte Listen.

## 5. Arbeitsregeln (autonom)

* Kleine Schritte, nach jedem Schritt: `cargo build -p vire` + `tests/vire_heap.sh`
  (künftig ohne 0-live-Orakel: Ownership-Gate) grün. Kein Schritt, der beides bricht.
* Jede Ablehnung bekommt Code + Test (promote **und** decline). Keine stillen Fallbacks.
* Gabbro-Pfad: Jede emittierte `.gab`-Datei muss `gabbro check` (unverändertes
  Orakel-Binary, Black-Box) mit 0 Errors bestehen; der C-Emitter (`gabbro build`
  über C11) ist **deprecated** und wird nicht genutzt — Ausführungswahrheit bis zum
  validierenden Compiler: `vire run` (LLVM-Orakel). Was `gabbro check` ablehnt oder
  Gabbro-Scope sprengt (O29-Dynamik, `Rc`), lehnt der Pfad mit Code ab statt zu
  schummeln.
* Benchmarks ehrlich: best-of-N, bit-identisch, RSS daneben — erst über den
  validierenden Gabbro-Compiler, kein C11-Scheinmaß; Constant-Fold-Artefakte
  (fixer `fib(38)`) markieren, nicht in Geomean schummeln.
* Deutsch im Gespräch, Englisch in Docs/Commits/Diagnostik-Codes.
