# AGENTS.md — Vire Neubau (gilt ab 2026-10-12)

## 0. Ziel (bindend, auch für README)

> **Vire: mindestens so schnell wie Rust, leichter als Python, vollständig getypt,**
> **speichersicher ohne `unsafe` — auch für Hardware und Register.**
> **LLVM bleibt. Java-IR, GPU/Vulkan, Runtime/GC sind raus.**

* **Performance:** `≥ Rust` auf CPU (Benchmark-Suite gegen `rustc -O`, `clang++ -O2`,
  bit-identisch, RSS daneben). Kein Pessimismus-Pfad im Compiler.
* **Ergonomie:** leichter als Python lesen/schreiben (keine Lifetimes, kein Borrow-Syntax,
  keine manuellen `free`/`retain`), aber **vollständig statisch getypt** — kein `Any`,
  kein stiller `Int`-Default, jeder Typ zur Compilezeit bekannt oder Compilefehler.
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
* **Keine Runtime/GC:** kein `runtime.c` mit RC/Collector/Slab, kein versteckter
  Allocator. Userspace: **statisches Ownership** (Stack + Regionen mit deklarierter
  Decke, `refuse-on-full`), alles zur Compilezeit bewiesen. **`Rc[T]` nur im Kernel/**
  Treiber als expliziter Opt-in-Typ — nie implizit.
* **LLVM bleibt:** einziges Backend (`clang -O2 -flto -march=native`, Text-IR).
  `unsafe_code = "forbid"` workspace-weit (Rust-Seite).

Wo README und diese Datei widersprechen, gewinnt diese Datei.

## 1. Was weg ist (nicht wieder einbauen ohne Entscheidung)

* `crates/classfile`, `crates/frontend`, Binär `fastjavac`, `tests/run.sh`-Java-Orakel
  als Gate (kann als externes Archiv leben, nicht als Build-Pfad).
* `crates/backend/src/nvptx.rs`, `spirv.rs`, `crates/driver/src/gpu_runtime.c`,
  `vk_runtime.c`, `crates/vire/src/shader.rs`, `crates/cuda-oxide/`,
  `@gpu`/`@vulkan`/`@vertex`/…-Attribute, `VkIface`/`GpuKernel`-IR, `spirv-as`/`llc`-NVPTX-Pfade.
* `crates/driver/src/runtime.c` (RC + Bacon-Rajan + Slab + Arenen als Runtime):
  ersetzt durch **emittierte statische Speicherverwaltung** (s. §3).
* Blinde Escapes: `native "c"/"c++"/"asm"` ohne Beweis, `--noverify`/`--unsafe-c`,
  `extern` mit Zeiger-Arithmetik, `Ptr`-als-`i64`, `pybridge.c` als impliziter Shim.
  Ersatz: `hw`-Modul + Header-Bindings mit geprüften Verträgen (s. §4).

## 2. Speicher­modell (Userspace: null Runtime; Kernel: explizites Rc)

1. **Owned values** (Standard): jeder Wert hat genau einen Besitzer, Move per Default,
   `free` zur Compilezeit am letzten Use (Solver beweist, Backend emittiert `free`/`alloca`
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

## 3. Hardware/Register ohne unsafe (Gabbro-Prinzip, ohne Formal­beweis)

* Rust-Seite: `#![forbid(unsafe_code)]` überall; kein handgeschriebenes C/Asm im
  ausgelieferten Binary außer **benanntem Eintritts-Stub** (vor Stack, mit Begründung
  + Test, gezählt).
* Vire-Seite: kein `unsafe`, kein `int->ptr`, kein Roh-`asm`. Stattdessen:
  `hw::Reg[T]`, `hw::Mmio[T]`, `hw::Port`, `syscall`-Items mit deklarierter
  Signatur + Kosten + Vertrag; `volatile`-/`fence`-Semantik aus der Deklaration,
  nicht aus C-Sequenzpunkten. Verstoß = Compilefehler mit Code (N-Nummer).
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
* Benchmarks ehrlich: best-of-N, bit-identisch, RSS daneben; Constant-Fold-Artefakte
  (fixer `fib(38)`) markieren, nicht in Geomean schummeln.
* Deutsch im Gespräch, Englisch in Docs/Commits/Diagnostik-Codes.
