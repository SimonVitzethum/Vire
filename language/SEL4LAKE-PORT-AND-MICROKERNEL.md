# Vire → SEL4Lake: Port Plan + Microkernel Performance Analysis

*User questions: plan the seL4 port; is seL4 the best microkernel for performance,
or would a better one be possible? Context: the goal is NOT stock seL4, but
**SEL4Lake** — the project's own capability microkernel in Rust (single-address-space,
aarch64; x86 port on its own branch, soon). Phases 0–7 done.*

## 1. Is seL4 the performance-best microkernel? — No, and SEL4Lake is the proof
seL4 is the best **formally verified** microkernel — not the fastest. Its
price for the safety is **per-process MMU isolation**: every IPC across an
isolation boundary costs an **address-space switch** (TTBR reload + TLB management),
and passing data requires **copying** or page remapping.

The performance-optimal design is **single-address-space (SAS)** — exactly
SEL4Lake's model (ADR 0002, inspired by Theseus):
- **Identity map + caches on → full HW performance** (no uncached-RAM problem).
- **No TLB shootdowns between components**, no TTBR switch on the
  context switch → context switch = **SP swap** (SEL4Lake Phase 4).
- **Zero-copy IPC**: references cross the isolation boundary cap-granted, without a copy
  (SEL4Lake P3 RegionSource/Zero-Copy).
This is measurably faster than seL4's fastpath, BECAUSE the address-space switch is eliminated.

**The trade** (honestly, ADR 0002): SAS provides NO hardware isolation between
components. The separation comes from **Rust (intralingual) + capabilities (authority)**.
This only works if **every** component is memory-safe — a single
unsafe native component could corrupt the entire space.

**→ This is exactly where Vire fits.** A SAS kernel needs memory-safe
userland components. Today that means "everything in Rust". Vire extends this with a
**second** memory-safe language (RC + cycle collector instead of borrow checker) —
more ergonomic for application logic, still without use-after-free/leak. **The better
microkernel (SAS) and Vire are complementary: the kernel provides the performance,
the language delivers the safety that the kernel presupposes.**

Could it be even better? The remaining perf levers beyond SAS are
microarchitecture, not architecture: **register IPC fastpath** (arguments in
registers instead of memory — seL4 does this, SEL4Lake's fastpath builds it out),
**IPI-free cross-core** (same-core fastpath, SEL4Lake P4 `switch_to`), and
**static system layout** (Microkit model → no dynamic cap lookup in the
hot path). SEL4Lake already addresses these. An "even better" kernel would not be a
different model, but SAS + these fastpaths pushed to the limit — which is SEL4Lake's roadmap.

## 2. Vire → SEL4Lake port plan (concrete, mapped onto their architecture)
Vire compiles via clang to native code → this fits SEL4Lake's **generic
binary loader** (ADR 0011: externally built, cap-gated processes). The port is
primarily a **runtime backend** (`FASTLLVM_FREESTANDING` exists as a skeleton).

**Phase A — aarch64 component that boots:**
1. **Target:** `vire build --target aarch64-unknown-none` (the `--target` flag is
   now there), no_std/freestanding runtime, no libc. (x86 follows once the
   SEL4Lake x86 branch lands — then `x86_64-unknown-none`.)
2. **Memory:** today a fixed 16 MB static heap → switch to **`sel4lake-region`**
   (ADR 0010): `plat_alloc`/the slab take memory from **cap-owned regions**
   (real physical addresses), not from an ambient heap. A Vire process holds a
   *set* of regions — the slab is instantiated per region. The compact 16-byte header +
   slab fit ideally into tight regions.
3. **IO:** `plat_write/puts` → SEL4Lake IPC (endpoint to a console/serial
   server), no stdio. A tiny `println`→IPC shim.
4. **Entry:** the loader calls the program entry; `main`→`java_main` stays, but
   without `atexit` (component runs permanently / terminated via supervisor).

**Phase B — concurrency + interop:**
5. **Threads/monitors:** pthreads → SEL4Lake TCBs + notifications/IPC (scheduler
   Phase 4/5). The `FASTLLVM_THREADS` path is reimplemented on SEL4Lake
   primitives.
6. **Capabilities = Vire's `Ptr`:** Vire's opaque `Ptr` type (no RC) maps
   SEL4Lake capabilities naturally — cap-granted handles to regions/endpoints
   cross the boundary zero-copy. This is the FFI boundary between Vire components.
7. **Zero-copy object passing (SAS bonus):** because all components share the same
   address space, a Vire reference can be cap-granted to another component
   **without a copy** — unlike seL4 (where a copy/remap is needed). This makes
   Vire component IPC cheap.

**Phase C — The GC model in the SAS (the interesting design question):**
- A **shared** collector across component boundaries would be possible (one
  address space), but couples components (one collection pauses all) → bad
  for determinism.
- **Better, fitting SEL4Lake's determinism goal:** **per-component isolation** —
  each Vire component has its own region(s) + its own slab/collector; across
  the boundary only cap-granted `Ptr` handles go (no shared RC). For hard
  real-time components: **`--no-cycles` + region inference + auto-arena** → pure RC
  or no GC at all, deterministic latency/memory. This is exactly the lever this
  session built (region inference at the ceiling, auto-arena).
- **Hot reload (Phase 7):** Vire components are AOT + deterministic → fit the
  v1→v2 swap over the same endpoint cap.

**Effort:** Phase A is the bulk (region allocator backend + IPC IO); the runtime
(RC/collector/slab) is pure C and already runs freestanding. No language-core rework.

## 3. Cross-platform (Linux/Windows/BSD/macOS) — status
- **Linux/BSD/macOS:** the POSIX runtime (stdio/stdlib/pthread) builds directly; the IR
  is triple-agnostic → `vire build --target <triple>` cross-compiles (toolchain/
  sysroot assumed). **Works in principle today.**
- **Windows:** the one non-portable point (C11 `aligned_alloc`) now has a
  `_WIN32` shim (`_aligned_malloc`); the threads path (pthreads) would still need
  Win32 threads for Windows (only relevant with `--threads`). Single-threaded runs.

## 4. Scaling (many 10 million lines) — status + plan
Built: **`--thin-lto`** (parallel, low-memory instead of full-LTO whole-program
bottleneck) + **string intern O(n²)→O(1)**. The runtime scales with data, not
LOC (see SCALING-SEL4.md). Open (design): **monomorphization instance cap** with
erasure fallback (hot type combos monomorphic, rare ones erased/`CallPoly`), and
**incremental compilation** (cache per module) — both are the honest path to
double-digit millions of lines, but each its own focused step.
