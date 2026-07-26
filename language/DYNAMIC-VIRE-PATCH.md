# Design Plan: Binary patch-point subsystem (Option B) — general native self-modification

**Date:** 2026-07-26
**Status:** PLAN, **B0 landed** (entry patch points + `@jrt_patchtab` + the anti-leak
RC'd module lifetime, §8); B1+ (the runtime patcher) unstarted. Companion to [DYNAMIC-VIRE-PLAN.md](DYNAMIC-VIRE-PLAN.md);
this is the heavier **Option B** (literal self-modifying machine code) referenced there in
§4.3, planned here as a **general facility**, not a mixin one-off. Mixin/around-advice on a
direct call is only its first client.

Relationship to Option A (the slot mechanism, already shipped): a `dynamic fn` seam
dispatches through the mutable `@jrt_dynslot` — pure data, no code rewriting (P2 step 1).
Option A is the default and needs no W^X. **Option B rewrites instruction bytes**, so it is
reserved for what A cannot do: patching **direct-call** sites, collapsing a hot slot
dispatch back to a direct call, live-updating a running function, and inserting probes. A
and B compose — the JIT uses B to remove A's indirection once a seam settles.

---

## 0. Why general, not mixin-specific (the requirement)

The goal names "later also useful for things other than mixins." So the design is a
**patch-point substrate** with a small, audited runtime patcher, of which each of these is a
*client*, not a special case:

| Client | What it patches | Analogue |
|---|---|---|
| **Mixin / around-advice on a direct call** | function entry → wrapper; `prev` = original body | AOP around |
| **Hot-update / live patch** | function entry → new implementation (no restart) | Linux `kpatch`/`ftrace` livepatch |
| **JIT dispatch-collapse (P3, §5.1)** | a slot-load *call site* → a guarded direct call | JIT patch point / inline cache |
| **Constant specialization (§5.2)** | a reserved `mov $imm` → a runtime-known constant | JIT constant patching |
| **Runtime instrumentation / probes** | function entry → `call hook; <body>` | `ftrace`/`__fentry__`, eBPF trampoline |
| **Guard installation** | a NOP region → a type-version guard | speculative-opt guard |
| **Feature-flag / A-B swap** | function entry → variant B | dark-launch toggle |

They share **one substrate** (compiler-reserved patch points + one runtime patcher). Build
the substrate once; the clients are thin.

---

## 1. Mechanism choice: reserved NOP sleds, not instruction relocation

Two ways to hijack a native function:

- **Detours (MS Detours style):** overwrite the real prologue bytes with a `jmp`, and
  *relocate* the overwritten instructions into a trampoline so the original can still run.
  The hard, fragile part is relocating arbitrary instructions (RIP-relative operands, jump
  targets, variable length). **Rejected** — arch-fragile and unsound to automate.
- **Reserved patch slots (ftrace / `-fpatchable-function-entry` style):** the compiler
  emits **dedicated NOP bytes** the patcher may overwrite. No real instruction is ever
  destroyed, so **no relocation is needed** and `prev`/original is always intact.
  **Chosen.** This is exactly how the Linux kernel does live patching.

LLVM/clang provides this directly: the function attribute
`"patchable-function-entry"="N"` (and the `-fpatchable-function-entry=N,M` flag) emits N
NOP bytes at the entry, with the symbol M bytes in. A 5-byte sled holds an `E9 rel32` (jmp)
or `E8 rel32` (call). **The backend sets this attribute only on opted-in functions**, so
sealed hot code keeps a bare entry and pays nothing.

---

## 2. Patch points (compiler-emitted)

Two kinds; both are just reserved bytes with a known address.

- **Entry patch point** — an N-byte NOP sled at a function's entry (N≥5 on x86-64; align to
  keep the sled within one cache line so a patch is a single aligned store where possible).
  Enables redirect / before-probe / around for that whole function.
  - Emitted for a function only when it is patchable: a `dynamic fn`/`open fn` (so B can
    later collapse A's slot to a direct-but-patchable call), or a function the user/JIT
    marks. Never for ordinary sealed functions.
  - The real body starts at `entry + N`; that address (`entry_after_sled`) is what `prev`
    and "restore" use.
- **Call-site patch point** (later, for the JIT) — a reserved region at a specific call
  site (e.g. the slot-load + indirect call of a seam), sized to hold `cmp; je; call` (the
  guarded-direct form). Lets the JIT (P3) rewrite an indirect slot dispatch into a guarded
  direct call in place.

**Locating a patch point at runtime:** reuse the symbol-table pattern already built for the
freestanding backtrace (`emit_symtab`). Emit a `@jrt_patchtab` = array of `{i64 addr, i64
sled_len, ptr name}` for every patchable function, so the runtime resolves *name → (addr,
sled_len)*. (Entry address alone suffices for entry points; call-site points get their own
id table.)

---

## 3. The runtime patcher (the whole API)

A small, audited module in `runtime.c` (hosted first; freestanding needs an RWX scratch
mapping). Every op is O(bytes) and only ever writes into a **compiler-reserved sled** — it
can never scribble over real instructions (that is the soundness anchor, §5).

```c
/* Redirect: all calls to `target` jump to `repl` (hot-update, feature swap). */
int  jrt_patch_redirect(void *target, void *repl);
/* Before-probe: run `hook` on entry, then fall through to the original body. */
int  jrt_patch_probe(void *target, void *hook);
/* Around: redirect to `wrapper`; the wrapper calls `jrt_patch_original(target)` for prev. */
void *jrt_patch_original(void *target);      /* = target + sled_len (the intact body) */
/* Patch a reserved immediate slot to a runtime constant (specialization). */
int  jrt_patch_imm(void *site, int64_t value);
/* Undo any patch on `target` — restore the NOP sled. Idempotent. */
int  jrt_patch_restore(void *target);
```

`prev`/around for the direct-call mixin then needs **no per-override cell** (unlike Option
A): the wrapper computes the original as `jrt_patch_original(target)` = `target + sled_len`,
the body untouched behind the sled. Chaining stacks wrappers; each wrapper's "original" is
whatever the entry pointed at when it was installed (save-and-restore per install, same
discipline as A's prev-cell but resolved through the patch table).

**Encoding (x86-64, first target):** redirect = write `E9 <rel32(repl-(target+5))>` over
the 5-byte sled; probe = `E8 <rel32(hook-(target+5))>`; restore = write back the 5-byte NOP
`0F 1F 44 00 00`. `rel32` covers ±2 GiB; if `repl` is farther, route through a nearby
allocated thunk (`mmap` in range) holding an absolute `jmp`.

---

## 4. W^X, atomicity, and cross-thread safety

The three real hazards, staged:

1. **W^X (always).** Code pages are RX. To patch: `mprotect(page, RW)` → write → `mprotect(page, RX)` → `__builtin___clear_cache(start,end)` (i-cache flush). A dedicated
   patch lock serialises patchers. Freestanding/seL4: no `mprotect` libc — map a private
   RWX scratch region or use the platform's memory-protection syscall; documented per port.
2. **Single-threaded (phase 1).** No other thread runs the target during a patch → the
   mprotect+write+flush above is sufficient and simplest. This already covers hot-update,
   probes, mixins, and the JIT collapse for the single-threaded programs Vire runs by
   default.
3. **Cross-thread live patching (phase N).** Another thread may be *executing* the target
   while we patch its first bytes — a non-atomic 5-byte store is unsafe. Use the
   **kernel/ftrace two-phase `int3` protocol**: (a) write a 1-byte `0xCC` (int3) over the
   first byte atomically; (b) install a SIGTRAP handler that, seeing a trap at a known
   patch point, redirects RIP to the replacement; (c) write bytes 2..5 of the `jmp`; (d)
   atomically replace the `int3` with the final first byte. A thread that hits the
   half-patched site traps and is steered correctly. Alternative: a **safepoint** protocol
   (park all threads at RC-safe points, patch, resume) — reuse the threads runtime's
   existing stop points. Pick per measurement; single-threaded ships first.

---

## 5. Soundness (the memory-safety contract)

Binary patching is the most dangerous facility in the system, so it is fenced hard —
consistent with Vire's paramount memory-safety invariant:

- **Only reserved sleds are ever written.** The patcher writes exclusively into
  compiler-emitted NOP sleds / reserved sites listed in `@jrt_patchtab`. It never overwrites
  a real instruction, so it cannot corrupt control flow or straddle an instruction boundary.
  A patch target not in the table is **rejected**.
- **ABI-checked targets.** A redirect/around target is a native function with a **verified
  compatible signature** (same as A's scalar-only rule at first; widened with the
  object-boundary verifier later). The load-time verifier (DYNAMIC-VIRE-PLAN.md §6) vets a
  module's patch payloads exactly as it vets its overrides.
- **W^X invariant.** Pages are writable only for the microscopic patch window under the
  patch lock; RX otherwise. No page is ever simultaneously W and X.
- **Restore-ability = determinism + debuggability.** Every patch is undoable to the exact
  original bytes (the sled is fixed content), so the running image is always reducible to
  its AOT form — no divergence a debugger can't reverse.
- **Still no VM / no synthesized logic.** The patcher only writes `jmp`/`call`/`nop`/an
  immediate — it *selects between existing native code*, it does not generate new logic.
  New logic is still AOT (M1/M2). This keeps Option B inside the same "no VM" boundary as
  the rest of the plan.
- **Bounds.** Only functions the compiler marked patchable have a sled → the patchable
  surface is explicit and minimal, like the sealing model's seams. Sealed code has no sled
  and cannot be patched at all.

Gating (each phase): the 0-live heap oracle, **Java 67/67 unaffected** (patch points are
emitted only for opted-in Vire functions; the Java backend path emits none), a
**patch→run→restore→run round-trip** test asserting the restored image is byte-identical and
behaviourally identical to unpatched, and a **cross-thread stress** once phase N lands.

---

## 6. Determinism

Consistent with the JIT's G3 (DYNAMIC-VIRE-PLAN.md §5.3): a patch is a pure function of
(target, replacement) — same inputs ⇒ same bytes written. Patches are triggered by explicit
API calls or the deterministic JIT counter, never a timer. A `FASTLLVM_PATCH_TRACE`
(target, op, bytes-before/after) must be identical across repeated runs of a fixed input —
a test-suite gate, like the JIT trace.

---

## 7. What changes where

| Piece | Change |
|---|---|
| `crates/backend` | Set `"patchable-function-entry"="N"` on patchable functions; emit `@jrt_patchtab` (addr, sled_len, name) — reuse the `emit_symtab` pattern; later, call-site patch-point reservation for the JIT. |
| `crates/vire` (frontend/lower) | Mark which functions are patchable (`dynamic`/`open`, or a `@patchable` attribute); `prev`/around lowering for the direct-call mixin client resolves through `jrt_patch_original`. |
| `crates/driver` → `runtime.c` | The patcher (`jrt_patch_redirect`/`probe`/`original`/`imm`/`restore`), W^X + i-cache handling, the patch lock, later the `int3`/safepoint cross-thread protocol; `@jrt_patchtab` lookup. |
| `crates/ir` | A small `patchable: bool` (or reuse the `dyn_fns`/attr channel) so the backend knows which functions get a sled. |

No new IR *operand*; the sled is a backend codegen attribute, and the tables mirror the
existing symtab/dynslot globals.

---

## 8. Phased roadmap (each independently shippable & gated)

- **B0 — entry patch points + `@jrt_patchtab` — DONE (2026-07-26).** The backend sets
  `"patchable-function-entry"="5"` on the patchable functions (currently the `dynamic fn`/
  `open fn` seams) → a 5-byte NOP sled at entry (verified in the binary: `0f 1f 44 00 08`
  before the real prologue), and emits `@jrt_patchtab` = `{ i64 addr, i64 sled_len, ptr
  name }` per patchable fn. Static, read-only (cannot leak); the sled is inert NOPs until a
  patch is applied. Running unchanged (seam still 210000, override still works); **Java 67/67
  + shape_soundness 3/3 unaffected** (no sled/table on the Java path — `dyn_fns` empty).
  *Also delivered here (the anti-leak requirement): reference-counted module lifetime.* A
  loaded module is a 1-based id into an RC'd table; a load holds 1 ref, each dyn-slot that
  points into it holds 1; `unload_module(handle)` drops the load ref; installing/replacing an
  override retains the new module and **releases the displaced one**; a module is `dlclose`d
  the instant its count hits 0 — so a stale module never lingers (**no retained-stale-
  function leak**) and is never closed while a slot still uses it (**no UAF**). `tests/
  vire_modlife.sh` (3/3): 1000× load/unload and 500× install→replace→unload both keep the
  256-entry table from exhausting (a leak would), RSS flat ~2 MB, and a slot-kept module
  stays callable after its load ref is released.
- **B1 — redirect + restore (single-threaded).** `jrt_patch_redirect` / `jrt_patch_restore`
  with W^X + i-cache. *Test:* redirect a fn to a replacement, call → new behaviour; restore
  → original; byte-identical after restore; 0-live. **First client: hot-update / feature
  swap — already useful beyond mixins.**
- **B2 — around-advice + `prev` on direct calls (the mixin client).** wrapper install +
  `jrt_patch_original`; chaining. *Test:* a wrapper logs before/after and `prev` runs the
  original; a 2-wrapper chain composes; 0-live.
- **B3 — probe / instrumentation.** `jrt_patch_probe` (before-hook). *Test:* a probe counts
  entries with no semantic change; restore removes it.
- **B4 — JIT clients: call-site patch points + immediate patching.** reserve call-site
  sleds; `jrt_patch_imm`; the JIT (P3) collapses a hot slot dispatch to a guarded direct
  call and specializes a stabilized constant. *Test:* a hot seam reaches near-direct-call
  speed then deopts on a new override; determinism trace stable.
- **B5 — cross-thread live patching.** the `int3` two-phase protocol or safepoints. *Test:*
  a running multi-threaded program redirects one of its own hot functions with no thread
  observing a torn instruction; heap balances.

Ordering rationale: B0–B1 stand up the substrate + its safest client (redirect/restore),
useful on their own (live update). Mixins (B2) and instrumentation (B3) are thin on top.
The JIT clients (B4) and cross-thread (B5) are the advanced, higher-risk tail — built only
after the substrate is proven and (for B4) the indirect-dispatch tax is measured to matter
(DYNAMIC-VIRE-PLAN.md risk #6).

---

## 9. Risks / open decisions

1. **Architecture-specificity.** The sled encoding is per-ISA (x86-64 `E9 rel32` first;
   aarch64 uses a `B`/`BL` with ±128 MiB range + a literal-pool thunk for farther targets).
   Abstract the encoder behind a tiny per-arch patch backend; ship x86-64, add aarch64.
2. **Inlining vs. patchability.** A patchable function must not be inlined at the sites you
   intend to patch — otherwise there is no single entry to redirect. Same rule as
   `dynamic fn` (already excluded from inlining); a `@patchable` fn is likewise pinned.
   Document that patching redirects *future* calls through the (un-inlined) entry only.
3. **`int3` handler signal-safety / debugger interaction.** The cross-thread protocol's
   SIGTRAP handler must be async-signal-safe and must coexist with a debugger's own
   breakpoints. Reason: prefer the safepoint variant if the threads runtime already has
   convenient stop points.
4. **PIC / rel32 range.** Shared modules are PIC; a redirect target > ±2 GiB away needs an
   in-range thunk (`mmap` near the target). Handle in the encoder.
5. **Is B needed at all vs. A?** For seam overrides, A (slots) already suffices and is
   safer. B earns its complexity only for direct-call sites, live update, the JIT collapse,
   and instrumentation. Build B0–B1 (substrate + hot-update) first as the standalone win;
   grow the other clients only as each is demanded and measured.

## 10. Non-goals
Relocating arbitrary overwritten instructions (Detours-style) · patching sealed
(non-`@patchable`) functions · synthesizing new logic (still M1/M2 AOT) · a W-and-X page ·
cross-thread patching before the atomic protocol lands (single-threaded only until B5).
