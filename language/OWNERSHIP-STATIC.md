# Vire — static ownership (no runtime, no GC; `Rc` only in kernel)

Status: design (2026-10-12, AGENTS.md §2). The current build still links
`crates/runtime/runtime.c` (RC + Bacon–Rajan + slab + arenas) and emits
`jrt_retain`/`jrt_release`. This doc is the migration target; every step below
is gated by `cargo build -p vire` + `tests/vire_heap.sh` (re-normalized, see §5).

## 1. Rule

*Userspace Vire has no reference counting, no collector, no hidden allocator.*
Every value has exactly one owner (inferred, no syntax). The backend emits the
`free` at the proven last use — directly (`free(p)` / scope-pop), never a counter.
What cannot be proven is a **compile error**, never a runtime fallback.

`Rc[T]` exists only as an explicit opt-in inside `kernel` modules (drivers,
long-lived graphs with dynamic lifetime). Userspace code naming `Rc` does not
compile. The kernel `Rc` ships its own small, budgeted, incremental runtime
(the only place retain/release may appear in emitted code).

## 2. The five tiers (highest provable wins; else error, not RC)

1. **Stack** — non-escaping locals/objects/arrays → `alloca` (exists: `StackNew`,
   escape analysis). Unchanged, only renamed (no "immortal" magic bit).
2. **Move** — unique refs freed at last use → backend emits `free` at the
   consuming use/`return` (generalizes today's move-on-last-use, which only
   *skips* RC; now it must *emit* the free, since there is no collector).
3. **Borrow (invisible)** — reads from a stable base the function and its
   transitive callees never mutate → no ownership transfer (exists:
   borrow-slot + field-write analysis). Unchanged in spirit; a doubtful case is
   rejected instead of retained.
4. **Region with ceiling** — cyclic or multi-owner graphs whose size is
   statically bounded (`array(N)`, `region(max=N)`, loop arenas with proven trip
   bound) → one bump pool, freed en bloc at scope end. Overflow is a *checked*
   error path (`refuse`: trap/panic-path, never UB, never silent growth).
   Replaces today's runtime arenas (`jrt_arena_push/pop`) with *emitted*
   frame pools (no global `arena_top`, no 64 KiB chunks from `malloc`).
5. **Kernel `Rc[T]`** — only in `kernel` modules, explicit import, budgeted
   incremental reclamation. The userspace compiler never emits it.

Explicitly **not** a tier: "dynamic graph without ceiling in userspace".
Today that silently falls back to RC+collector; after the migration it is
error E300 with a fix hint ("name a ceiling → region, or move to a kernel
module → Rc"). No silent fallback — a fallback that fires is a soundness-shaped
lie about performance (§7.1 EVALUATION.md: the RC path never competed).

## 3. What disappears from emitted code

- Object header shrinks to `{ vtable }` (drop fn for static free; no refcount,
  no color/buffer bits, no `rcflags`). Immortal bit gone (nothing is immortal;
  everything is owned or borrowed or region-pooled).
- No `jrt_alloc` (→ `malloc` for owned heap, `alloca` for stack, pool-bump for
  regions), no `jrt_retain`/`jrt_release`, no `jrt_collect_*`, no slab, no
  `FASTLLVM_NO_RC`/`NO_CYCLES`/`GUARD_FREE` knobs (nothing to switch off).
- `runtime.c` shrinks to: entry (`main`), print/write helpers, math helpers
  (`sqrt` intrinsic already), abort/uncaught-message. Everything else is
  *emitted* per program (frees, pool pops) or gone.
- `drop` functions stay (per-type field frees, generated) but run *exactly
  once* at the static free point — no queue, no budget, no deferral.

## 4. Migration steps (each green-gated)

1. **M1 — `Rc` gate:** `Rc` name rejected in userspace (today: no `Rc` type at
   all → vacuous; add the type *first* as kernel-only so the gate is real).
2. **M2 — free emission:** backend emits `free` at move-consumed last uses;
   heap suite asserts frees balance (replaces 0-live oracle, see §5).
3. **M3 — region ceilings:** `region(max)` surface + emitted pools; loop arenas
   become pools with proven bounds; unbounded cases → E300.
   **SHIPPED 2026-10-13 (explicit surface):** `region(max) { … }` mit
   Byte-Ceiling (`jrt_region_push/pop`, Refuse per Abort `vire.region.Overflow`),
   skalarem Ergebnis, E300 bei Pool-Unsicherheit (Freshness-Beweis aus den
   Loop-Arenen wiederverwendet); inferierte Loop-Arenen feuern innen nicht
   (unbounded Push würde den deklarierten Pool überschatten), Stack-Promotion
   gilt weiter; Backend-Arena-Tiefe + Immortal-Pfad decken `region_push` ab
   (kein Direct-Free auf Pool-Speicher). Tests: `tests/vire_region.sh` (6),
   Beispiel `examples/vire/region_pool.vr`. Offen: E300-Enforcement als Default
   (heute Warn-Vorschau), `region(max)`-Migration der 8 Graph-Files.
4. **M4 — runtime shrink:** delete collector/slab/RC from `runtime.c`; header
   without counter; remove `FASTLLVM_*` knobs.
5. **M5 — delete the fallback:** any remaining retain/release emission is a
   compiler ICE (not a fallback).

## 4b. E300 blast radius — MEASURED (2026-10-13)

`FASTLLVM_WARN_DYNAMIC=1` previews enforcement (warn-only): 47 Shared heap
sites across 43 corpus files, concentrated in 8 graph-builder programs
(object_graph/btree/compiler 14 each, sharedgraph 3, list 3, ring 2,
array_objects/arraygraph 1) — everything else is ZERO. Enforcement would only
touch programs that genuinely build dynamic graphs, which is exactly the
intended scope (AGENTS.md §2.4).

Known refinement before enforcement (M3): loop-arena-proven allocations
(btree/sharedgraph/compiler run on the loop arena today — bounded lifetime
already proven by `while_arena_safe`) must not count. That needs the arena
verdict threaded into the report; until then the count is conservative
(sound direction: more Shared, never less).

## 5. Testing (replaces the 0-live oracle)
- **Free-balance:** every test program frees exactly what it allocates
  (ASan/LSan gate in `tests/vire_mem.sh`: `clang -fsanitize=address,undefined`,
  0 errors, matching output).
- **Temporal:** debug mode guards freed pools (`mprotect`, like today's
  GUARD_FREE) — a premature free segfaults deterministically.
- **Decline pins:** unbounded-dynamic-graph programs stay in the suite as
  must-reject cases (E300), promote+decline like today's arena pins.
