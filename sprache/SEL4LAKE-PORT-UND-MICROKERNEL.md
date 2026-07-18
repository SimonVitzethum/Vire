# Vire → SEL4Lake: Port-Plan + Microkernel-Performance-Analyse

*Nutzerfragen: den seL4-Port planen; ist seL4 der beste Microkernel für Performance
oder ginge ein besserer? Kontext: das Ziel ist NICHT stock-seL4, sondern
**SEL4Lake** — der eigene capability-Microkernel in Rust (Single-Address-Space,
aarch64; x86-Port auf eigenem Branch, bald). Phasen 0–7 fertig.*

## 1. Ist seL4 der performance-beste Microkernel? — Nein, und SEL4Lake ist der Beweis
seL4 ist der beste **formal verifizierte** Microkernel — nicht der schnellste. Sein
Preis für die Sicherheit ist die **per-Prozess-MMU-Isolation**: jede IPC über eine
Isolationsgrenze kostet einen **Adressraum-Wechsel** (TTBR-Reload + TLB-Verwaltung),
und Datenübergabe braucht **Kopieren** oder Page-Remapping.

Der performance-optimale Entwurf ist **Single-Address-Space (SAS)** — genau
SEL4Lakes Modell (ADR 0002, inspiriert von Theseus):
- **Identity-Map + Caches an → volle HW-Performance** (kein uncached-RAM-Problem).
- **Keine TLB-Shootdowns zwischen Komponenten**, kein TTBR-Wechsel beim
  Context-Switch → Kontextwechsel = **SP-Tausch** (SEL4Lake Phase 4).
- **Zero-Copy-IPC**: Referenzen queren die Isolationsgrenze cap-gewährt, ohne Kopie
  (SEL4Lake P3 RegionSource/Zero-Copy).
Das ist messbar schneller als seL4s Fastpath, WEIL der Adressraum-Wechsel entfällt.

**Der Trade** (ehrlich, ADR 0002): SAS bietet KEINE Hardware-Isolation zwischen
Komponenten. Die Trennung kommt aus **Rust (intralingual) + Capabilities (Autorität)**.
Das funktioniert nur, wenn **jede** Komponente speichersicher ist — eine einzige
unsichere native Komponente könnte den ganzen Space korrumpieren.

**→ Genau hier ist Vires Platz.** Ein SAS-Kernel braucht speichersichere
Userland-Komponenten. Heute heißt das „alles in Rust". Vire erweitert das um eine
**zweite** speichersichere Sprache (RC + Zyklen-Collector statt Borrow-Checker) —
ergonomischer für Anwendungslogik, weiter ohne use-after-free/Leak. **Der bessere
Microkernel (SAS) und Vire sind komplementär: der Kernel gibt die Performance,
die Sprache liefert die Sicherheit, die der Kernel voraussetzt.**

Könnte man noch besser? Die verbleibenden Perf-Hebel über SAS hinaus sind
Mikroarchitektur, nicht Architektur: **Register-IPC-Fastpath** (Argumente in
Registern statt Speicher — seL4 macht das, SEL4Lakes Fastpath baut es aus),
**IPI-freies Cross-Core** (Same-Core-Fastpath, SEL4Lake P4 `switch_to`), und
**statisches Systemlayout** (Microkit-Modell → keine dynamische Cap-Suche im
Hot-Path). SEL4Lake adressiert diese bereits. Ein „noch besserer" Kernel wäre kein
anderes Modell, sondern SAS + diese Fastpaths ausgereizt — was SEL4Lakes Roadmap ist.

## 2. Vire → SEL4Lake Port-Plan (konkret, auf ihre Architektur abgebildet)
Vire kompiliert über clang zu nativem Code → das passt zu SEL4Lakes **generischem
Binary-Loader** (ADR 0011: extern gebaute, cap-gegatete Prozesse). Der Port ist
primär ein **Runtime-Backend** (`FASTLLVM_FREESTANDING` existiert als Skelett).

**Phase A — aarch64-Komponente, die bootet:**
1. **Target:** `vire build --target aarch64-unknown-none` (das `--target`-Flag ist
   jetzt da), no_std/freestanding-Runtime, kein libc. (x86 folgt, wenn der
   SEL4Lake-x86-Branch landet — dann `x86_64-unknown-none`.)
2. **Speicher:** heute fixer 16-MB-Static-Heap → auf **`sel4lake-region`** umstellen
   (ADR 0010): `plat_alloc`/der Slab nehmen Speicher aus **cap-besessenen Regionen**
   (echte Physadressen), nicht aus einem Ambient-Heap. Ein Vire-Prozess hält eine
   *Menge* Regionen — der Slab wird pro Region instanziiert. Der packte 16-B-Header +
   Slab passen ideal zu knappen Regionen.
3. **IO:** `plat_write/puts` → SEL4Lake-IPC (Endpoint an einen Konsolen-/Serial-
   Server), kein stdio. Ein winziger `println`→IPC-Shim.
4. **Einstieg:** der Loader ruft den Programm-Entry; `main`→`java_main` bleibt, aber
   ohne `atexit` (Komponente läuft dauerhaft / via Supervisor beendet).

**Phase B — Nebenläufigkeit + Interop:**
5. **Threads/Monitore:** pthreads → SEL4Lake-TCBs + Notifications/IPC (Scheduler
   Phase 4/5). Der `FASTLLVM_THREADS`-Pfad wird auf SEL4Lake-Primitiven neu
   implementiert.
6. **Capabilities = Vires `Ptr`:** Vires opaker `Ptr`-Typ (kein RC) bildet
   SEL4Lake-Capabilities natürlich ab — cap-gewährte Handles auf Regionen/Endpoints
   queren die Grenze zero-copy. Das ist die FFI-Grenze zwischen Vire-Komponenten.
7. **Zero-Copy-Objektübergabe (SAS-Bonus):** weil alle Komponenten denselben
   Adressraum teilen, kann eine Vire-Referenz cap-gewährt an eine andere Komponente
   gehen **ohne Kopie** — anders als bei seL4 (dort Kopie/Remap nötig). Das macht
   Vire-Komponenten-IPC billig.

**Phase C — Das GC-Modell im SAS (die interessante Design-Frage):**
- Ein **gemeinsamer** Collector über Komponentengrenzen wäre möglich (ein
  Adressraum), koppelt aber Komponenten (eine Collection pausiert alle) → schlecht
  für Determinismus.
- **Besser, passend zu SEL4Lakes Determinismus-Ziel:** **per-Komponente Isolation** —
  jede Vire-Komponente hat ihre eigene(n) Region(en) + eigenen Slab/Collector; über
  die Grenze gehen nur cap-gewährte `Ptr`-Handles (kein geteiltes RC). Für harte
  Real-Time-Komponenten: **`--no-cycles` + Region-Inferenz + Auto-Arena** → reine RC
  oder gar keine GC, deterministische Latenz/Speicher. Das ist genau der Hebel, den
  diese Session gebaut hat (Region-Inferenz an der Decke, Auto-Arena).
- **Hot-Reload (Phase 7):** Vire-Komponenten sind AOT + deterministisch → passen zum
  v1→v2-Austausch über dieselbe Endpoint-Cap.

**Aufwand:** Phase A ist das Gros (Region-Allokator-Backend + IPC-IO); die Runtime
(RC/Collector/Slab) ist reines C und läuft freestanding schon. Kein Sprachkern-Umbau.

## 3. Cross-Platform (Linux/Windows/BSD/macOS) — Status
- **Linux/BSD/macOS:** die POSIX-Runtime (stdio/stdlib/pthread) baut direkt; das IR
  ist triple-agnostisch → `vire build --target <triple>` cross-kompiliert (Toolchain/
  Sysroot vorausgesetzt). **Läuft im Prinzip heute.**
- **Windows:** der eine nicht-portable Punkt (C11 `aligned_alloc`) hat jetzt einen
  `_WIN32`-Shim (`_aligned_malloc`); der Threads-Pfad (pthreads) bräuchte für Windows
  noch Win32-Threads (nur bei `--threads` relevant). Single-threaded läuft.

## 4. Skalierung (viele 10 Mio Zeilen) — Status + Plan
Gebaut: **`--thin-lto`** (parallel, speicherarm statt Full-LTO-Whole-Program-
Flaschenhals) + **String-Intern O(n²)→O(1)**. Die Runtime skaliert mit Daten, nicht
LOC (s. SKALIERUNG-SEL4.md). Offen (Design): **Monomorphisierungs-Instanz-Cap** mit
Erasure-Fallback (heiße Typkombis monomorph, seltene erased/`CallPoly`), und
**inkrementelle Übersetzung** (pro Modul cachen) — beides ist der ehrliche Weg zu
zweistelligen Millionen Zeilen, aber je ein eigener fokussierter Schritt.
