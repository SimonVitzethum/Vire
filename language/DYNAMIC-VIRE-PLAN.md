# Design Plan: Dynamic Loading, Reflection & Self-Modification for Vire — with a JIT, no VM

**Date:** 2026-07-26
**Status:** PLAN, first slices landed — **P0 step 1** (static reflection API:
`type_name`/`field_count`/`field_name`/`field_type`/`abi_version`) and **P1 step 1** (the
scalar-only M1 module round-trip) and the **P1 step 2a** sealing-model foundation
(`dynamic fn`/`open` parse + the arena declining at a seam), §9. The shared-type object
boundary + load-time verifier + the runtime override + the JIT are unstarted. FastJavaC's dynamic runtime (Phases 0–5, `../FastJavaC/
DYNAMIC-RUNTIME-PLAN.md`) is the **existence proof** that native, no-VM/no-JIT dynamism
works: native module ABI, `dlopen` loader, redefinition by code-pointer swap,
compile-on-load cache, binary trampoline patching — all implemented and 0-live-heap tested
there. This plan brings that capability to Vire and adds one deliberate step beyond it: a
**real in-process JIT** that patches/specializes the running binary to *reclaim* the speed
dynamism costs — permitted because it is held to four non-negotiable guarantees:
**(G1)** it only makes the program *faster*, never changes results (semantics-preserving,
so correctness never depends on it); **(G2)** it is **memory-safe by construction** (it
only transforms already-verified IR with safety-preserving passes + guards + native
deopt); **(G3)** it is **deterministic** (same input ⇒ same decisions, same emitted bytes,
same output — no wall clock, no sampling thread); **(G4)** it has a **near-zero idle
footprint** (per-seam counters + one reused scratch buffer, no background thread, no
growing code cache). And the whole open world stays **RC-only — no garbage collector** —
running as native code directly on the CPU, never a VM.

**Concrete surface syntax** for every construct below (open seams, effect contracts,
plugin impls/overrides, module headers, loading, reflection) is proposed in the companion
[DYNAMIC-VIRE-SYNTAX.md](DYNAMIC-VIRE-SYNTAX.md).

**What is borrowed and what is not.** FastJavaC contributes only the **systems
mechanisms** — code-pointer indirection, trampoline patching, native module linking, the
load-then-link discipline — which are language-agnostic. **No Java compatibility is a
goal, and no Java language concept is adopted 1:1.** Vire has no classes, no interface
hierarchy, no bytecode; it has **structs (product types), sum types, and traits**, plus
value generics, comptime, and the RC+arena memory model. The dynamic surface below is
designed **from those** — `open type`/`open trait`/`dynamic fn`, a descriptor over Vire's
own type graph, runtime trait-impl loading — not as a port of a class/vtable/mixin model.
Where a FastJavaC term appears (e.g. "method table"), it names a *mechanism*, not a Java
semantic.

---

## 0. Why this is genuinely hard for Vire (read this first)

A managed runtime with a tracing GC can bolt dynamism on comparatively cheaply: the memory
model stays **safe regardless of what loads** — new code appearing never threatens heap
integrity, only speed. Vire is the opposite. Its speed comes from static proofs that are
**memory-safety-critical, not merely performance hints**:

- **RC elision** (`crates/backend` `immortal_only_locals`): drops `retain`/`release` on
  references it proves stay within a known lifetime. A *wrong* verdict is a
  use-after-free, not a slowdown.
- **Automatic arena inference** (`crates/vire/src/lower.rs` `while_arena_safe`): frees a
  whole per-iteration object graph *en bloc*, having proven — transitively over **every
  callee** — that nothing escapes the iteration. A callee that suddenly *does* escape (a
  runtime override) turns the en-bloc free into a dangling pointer.
- **Whole-program devirtualization / bounds elision / monomorphization**: all assume the
  set of types and call targets is *complete* at compile time.

So the central design question is not "how do we load code" (FastJavaC answered that).
It is:

> **How can new code appear at runtime without silently invalidating the closed-world
> soundness proofs that the already-running, already-optimized code baked in — while
> keeping the 99% that never interacts with dynamism at full, unguarded speed?**

The whole plan is the answer to that one question. Everything else is reuse.

---

## 1. The honest boundary: JIT yes, VM no — where the line is

The line is not "codegen at runtime" (the JIT does that); it is **synthesizing *new
logic*** at runtime. New *logic* can arrive three ways; only the first two are possible
without a VM:

| Mode | New logic arrives as | Needs | Verdict |
|---|---|---|---|
| **M1 — prebuilt native module** | already `vire`-compiled `.so`/PIC object | linking only | ✅ primary |
| **M2 — compile-on-load** | `.vr` source / IR, compiled by a `vire` **subprocess**, cached | an AOT compiler (separate process) | ✅ |
| **M3 — new logic executed from a description in-process** | bytecode/source the running engine turns into behavior with no prior compile | an interpreter or a source-compiling JIT | ❌ = a VM |

**A CPU cannot execute Vire source; turning *new* source into behavior in-process is
exactly what an interpreter/VM does.** So genuinely new logic is always AOT-compiled —
prebuilt (M1) or by a separate `vire` process (M2), like FastJavaC.

**Where does the JIT sit?** *Not* at M3. The JIT never turns a description into new logic;
it only **re-emits code that already exists in verified form** — collapsing an indirect
call to a direct one, filling a constant, or specializing an already-AOT-compiled function
to facts now known. Its input is always **verified IR or a pre-authorized patch site**,
never new source. That is the precise boundary that makes "a real JIT, self-modifying the
running binary, but no VM" true rather than a contradiction: **M3 (new logic without a
compiler) stays forbidden; specializing existing verified logic (§5) is the JIT's whole
job.** The four guarantees (G1–G4, above) fence it in.

---

## 2. The sealing model — the soundness foundation

**Sealed by default; open only where declared.** This is the single most important
decision and the thing that keeps Vire fast.

- Everything is **sealed** (closed-world, fully optimized, direct calls, RC elision,
  arena, monomorphization) unless it sits on an explicitly declared **open seam**.
- A seam is introduced by annotation on the *provider* side, never inferred:
  - `open type T` (participating in an `open trait`) — T may gain new `impl`s at
    runtime; trait calls on it dispatch through a **mutable slot**, never unconditionally
    devirtualized.
  - `dynamic fn m(…)` — m may be redefined/overridden at runtime.
  - `open module …` / a `@extensible` boundary — a unit that exposes symbols for modules
    to call and be called by, across the **frozen ABI** (real RC, no cross-seam inlining).
- **The optimizer treats a seam as a hard black-box boundary** (§4, §6): no devirt across
  it, no RC elision assuming a fixed callee, **no arena promotion across it**, no
  monomorphization of a seam-typed value. Sealed code that never *reaches* a seam is
  compiled exactly as today — zero tax.

This inverts the FastJavaC `--dynamic` **global** switch (whole program pays the
indirect-dispatch tax). Vire pays **only at seams**, because for Vire the tax is not just
slowness — an un-annotated global open world would mean turning off RC elision and arena
*everywhere*, erasing the entire performance story. Sealing keeps the proofs valid where
they run and disables them precisely where they'd be unsound.

*Ergonomic default:* a program with no `open`/`dynamic`/`@extensible` anywhere compiles
and runs **exactly** as it does today — same speed, same 0-live oracle. Dynamism is
opt-in, visible in the source, and locally reasoned about.

### 2.1 Knowledge is asymmetric — and that is the main performance lever
The host and its plugins do **not** know each other symmetrically, and the design exploits
the asymmetry rather than treating the boundary as an opaque wall:

- **Only the host, at *its* compile time, is blind to the plugins** — they don't exist yet.
  So the host must treat its declared seams as open (mutable slot, no cross-seam
  optimization). This is the unavoidable direction.
- **A plugin/mod, at *its* compile time, sees the host in full** — its type descriptors,
  layouts, and the **IR of the host's sealed surface** (shipped in the host's module SDK /
  passed to the M2 subprocess as `--link-against <host-manifest>`, incl. already-loaded
  deps). Therefore the plugin is compiled **closed-world relative to (host-sealed-surface +
  this plugin + resolved deps)** and may fully optimize *into* the host:

  > **A plugin may inline, devirtualize, monomorphize and const-fold against the host's
  > SEALED surface** — sealed host code is immutable (that is what `sealed` *means*), so an
  > inlined copy can never go stale; the plugin's cache key includes the host version, so a
  > changed host just recompiles the plugin. **Only the host's OPEN seams** must still be
  > reached through the frozen ABI (they can change under it).

This is strictly stronger than FastJavaC's "never inline across the module boundary": the
boundary is porous **in the plugin→host-sealed direction**, sealed only in the
host→plugin and the across-an-open-seam directions. A mod calling hundreds of host
utility functions pays no ABI tax on the sealed ones — it inlined them, exactly as if it
had been part of the original closed-world compile. The runtime JIT then adds only the
*last* increment of specialization that was unknowable even at plugin-compile time — which
of several loaded impls actually wins a seam, and runtime constants (§5).

### 2.2 Seam granularity — the decision (resolves risk #1)
How coarse is a seam? The spectrum, coarsest → finest:

| Granularity | Unit that becomes open | Cost |
|---|---|---|
| Whole-program (`--dynamic` global) | everything | the entire perf story — **rejected** (§2) |
| Module (`open module M`) | every trait + exported fn in M | pessimizes all of M, even parts no mod touches |
| **Trait / method (`open trait`, `dynamic fn`)** | one trait's dispatch, or one fn | **minimal — only the exact extension points** |
| — with `open module` as opt-in **sugar** | marks all of a module's traits/exports open at once | coarse *when you want it*, at the host's choice |

**Decision: the finest practical unit — per-`open trait` and per-`dynamic fn`, sealed by
default, explicit, with `open module` as coarsening sugar.** Rationale:

1. **Perf & safety are the same argument here.** Vire's optimizations are memory-safety-
   critical, so the seam surface is not just where speed is lost — it is where the load
   verifier must do its work. Minimizing it minimizes *both* the perf tax and the trust
   surface. Finest-grained ⇒ smallest surface.
2. **It synergizes with asymmetric knowledge (§2.1).** The more of the host stays *sealed*,
   the more a plugin can inline into it. Fine granularity keeps the sealed surface large,
   so plugins are faster, not just the host.
3. **Under-annotation fails *safe*.** Forget to mark something `open` that a mod needs, and
   the mod is **rejected at load** with a clear "host did not declare X extensible" — never
   a silent miscompile. So the closed default is safe to iterate against: the failure mode
   of "too sealed" is a loud rejection, whereas "too open" would be a silent perf/safety
   tax. Always fail toward the safe side.
4. **Traits are sealed unless `open trait`.** Vire leans on traits heavily; an implicitly-
   open trait would route *every* trait call through a mutable slot — a broad, invisible
   regression. Explicit `open` (like Kotlin/Swift `open`/`final`, closed by default) keeps
   the host author in deliberate control of their **stable extension API** — which is also
   what makes the ABI versionable.
5. **`open type` means "may receive new trait impls," not "layout is extensible."** A
   struct's fields are always the frozen ABI (you cannot add a field at runtime without
   breaking every compiled offset). So `open type T` only enables *new `impl`s* for T at a
   runtime seam; field layout is never open. (Runtime *state* extension, if ever wanted, is
   a separate opt-in side-table, not a layout change — deferred, not in this plan.)

Net: the default is "closed and fast"; opening is a deliberate, local, greppable act on
exactly the trait or function meant to be an extension point; and a host that wants a
broadly moddable surface writes `open module` once rather than annotating each item.

---

## 3. Reflection = compile-time metadata, not runtime type synthesis

Vire's stated non-goal is "runtime `eval`/reflection" (dynamic typing). This plan honors
that. "Reflection" here means a **statically emitted, typed descriptor over Vire's own
type graph** — queried at runtime through typed intrinsics. No dynamic types are ever
synthesized; the descriptor is a frozen view of what the compiler already knew.

The descriptor mirrors Vire's three kinds of type, not a class hierarchy. The backend
already computes the underlying layout/dispatch data (`local_arr_class`, trait method
sets, sum-variant tags) and **discards** it after codegen; the change is to serialize it:

- **product types (structs):** field table — offset, ref-or-scalar, and for a ref field
  its element/object class (incl. the element class of a typed object array, which Vire
  has and a class-based model does not) — this drives GC tracing of instances that were
  loaded at runtime;
- **sum types:** variant tags + per-variant payload layout, so `match` and the tracer walk
  a runtime-loaded value correctly;
- **traits:** the trait's method set + slot assignment — the unit of *open* dispatch (a
  runtime module supplies a new `impl Trait for T`, §4);
- per type: header format, allocation size, `VIRE_ABI_VERSION`, and the RC/arena **effect
  summary** at the boundary (does a method retain its args? can a returned ref outlive the
  callee? — see §6).

This subsumes the existing `@typeinfo`/`@derive` roadmap item: `@typeinfo(T)` becomes a
comptime-iterable view over the same descriptor. Two registries — **static** (the sealed
core, baked in) and **dynamic** (populated by the loader) — keyed by type identity;
construction, trait dispatch, `is`/`match`, and reflective queries resolve through them.

---

## 4. The dispatch substrate

The unit of open dispatch is a **trait method on an `open type`/`open trait`** — a
runtime module supplies a new `impl Trait for T` (or a new `dynamic fn` body). Three
layered mechanisms carry it, all native, none a VM; (1)+(3) are the systems primitives
FastJavaC proved out, reused here under Vire's own dispatch.

1. **Indirect trait slots (pointer swap) — the default.** A `dynamic fn`, or a trait
   method on an `open` type, is called through a code pointer in a **mutable dispatch
   table** (the backend's trait/dispatch-slot machinery extended to be writable — Vire
   already resolves trait calls through slots when it cannot devirtualize). *Loading a new
   impl / redefinition = store a new already-native pointer.* Zero codegen. Sealed calls
   (the common case, resolved to one impl at compile time) stay **direct**.
2. **Guarded devirtualization — recovers the speed a seam costs.** At a hot seam call
   site, emit a **type-version guard** → fast **direct** call to the currently-resolved
   impl, with the indirect slot as the fallback. When a load invalidates the guard, the
   site simply starts taking the (already-native) slow path. No deopt-to-interpreter —
   there is none; the fallback is another native call.
3. **Binary trampoline patch points — opt-in, for hot direct sites.** A padded entry
   (reserved NOP bytes) the runtime can rewrite into a `jmp` to another native
   implementation. Literal self-modifying code, but it only **switches between
   pre-existing native code**. Needs W^X (`mprotect` RW↔RX), i-cache flush, and
   cross-thread safepoints.

Mechanisms 1 and 3 are proven native primitives (FastJavaC); for Vire they are the
*carrier*. The new work is the **soundness contract they must respect** (§6) and the
**JIT that drives (2) and (3)** (§5).

### 4.5 The three requested capabilities, delivered Vire-native (no VM, no GC, on the CPU)
The Java-ecosystem features the goal names — **overrides, mixins, dynamic loading** — are
delivered as *capabilities*, each mapped onto a native mechanism above, none importing a
Java semantic and none needing a VM or a GC:

- **Override** = redefine a `dynamic fn` / supply a new `impl` for an `open` type at runtime
  → **pointer swap** into the mutable slot (§4.1). The next call runs the new native body.
- **Mixin-style injection** (wrap/inject at a method's head or tail, augment behavior
  without owning the original) = an override that **calls through to the previous pointer**
  it replaced (a native "super"/around-advice), installed via §4.1, or an in-place
  head/tail redirect via the **trampoline patch point** (§4.3). The *capability* (intercept
  and extend an existing method) is preserved; the *model* is a native pointer chain, not
  bytecode weaving into a class.
- **Dynamic loading** = M1 (prebuilt native module) or M2 (compile-on-load via a `vire`
  subprocess), linked through the frozen ABI after the **load verifier** (§6) accepts it.

All three are ordinary native machine code on the CPU at all times; the only runtime
"engine" is the loader + the code-pointer/branch patcher (+ the optional JIT for speed).

---

## 5. The JIT — a pure, deterministic, safety-preserving performance layer

A real in-process JIT **is permitted**, subject to four non-negotiable guarantees. It
exists only to make dynamism cheap; it is never a correctness dependency. The guarantees
are the whole contract:

> **(G1) Speed-only.** Every transform is **semantics-preserving**: the JIT may make a
> program faster, never change what it computes. Correctness therefore does not depend on
> it at all — delete the JIT and every program produces the identical result, only slower.
> **(G2) Memory-safe by construction.** The JIT never invents logic. It transforms
> **already-verified typed IR** with a **fixed, audited set of safety-preserving passes**
> (const-prop, guarded devirt, inline across a *resolved* seam, dead-branch removal), each
> guarded so its runtime assumption holds or it **deopts to the always-present native slow
> path**. It inherits the AOT memory-safety proof; it cannot weaken it.
> **(G3) Deterministic.** Same program + same inputs ⇒ **the same JIT decisions, the same
> emitted bytes, the same result** — every run, on every machine (see §5.3). No wall clock,
> no sampling thread, no scheduler dependency.
> **(G4) Near-zero idle footprint.** Off the hot path it costs almost nothing: a few
> bytes of per-seam counter, one small reused emit scratch buffer, **no background thread,
> no growing code cache** (it emits small specializations of existing code, not whole
> recompiled programs). Idle RAM is dominated by the (static) template library — kilobytes.

### 5.1 Tier A — patch/template emitter (the cheap, always-available layer)
A finite, audited library of fixed byte patterns with typed holes (a pointer, an aligned
immediate, a relative offset) — instruction *parameterization*, never *selection*:
- **Dispatch collapse / re-sealing.** After an `open` site has resolved to a single impl
  and stayed there past a **deterministic** threshold (§5.3), patch the indirect call into
  `cmp type-version; je fast; jmp slow; fast: call target`. Near-sealed speed; repatched to
  the slow path the instant a new impl/override lands. **This is "modify the running binary
  to reclaim performance."**
- **Constant hole fill.** Splice a stabilized runtime constant into a `mov $imm, reg` slot
  the AOT compiler reserved. No new control flow — just fill a pre-authorized hole.
- **Guard/trampoline stubs** for §4.3.

### 5.2 Tier B — bounded specializing recompiler (optional, opt-in, measured)
When Tier A's guard+direct-call is not enough, the JIT may **recompile one hot function
from its embedded verified IR**, specialized to the now-known facts (a resolved seam
target, a stabilized constant), running the **same backend passes in-process but only the
subset proven safety-preserving**. Because the input IR was already type-checked and the
pass set is safety-preserving, the output inherits the safety proof (G2); because it is
semantics-preserving, results are unchanged (G1). The specialized body is entered through a
guard and abandoned (deopt → slow path) if its assumption breaks. This is a genuine JIT —
but a *specializer of verified code*, never a compiler of new source, and still no VM: the
slow path is always native, never an interpreter.

*Boundedness (protects G4):* specialize only seams proven hot by the deterministic counter;
cap the total specialized-code budget; evict least-recently-entered specializations under
pressure. No unbounded code cache.

### 5.3 Determinism (G3) — how it is actually guaranteed
The JIT's *decisions* are a pure function of the program's *observable execution*, not of
timing:
- **Trigger = a deterministic counter, not a timer.** "Hot" means an **invocation/resolution
  counter** (incremented inline per call, compared to a fixed threshold) reached its bound —
  identical on every run for the same input. **No wall-clock sampling, no profiler thread**
  (which also serves G4).
- **Emission is a pure function.** Given (IR, resolved facts, target triple) the emitted
  bytes are fully determined — the backend is already deterministic (fixed pass order, no
  randomness; the project forbids `Math.random`/wall-clock in codegen). Same facts ⇒ byte-
  identical patch.
- **Order independence.** A specialization only fires when its guarded assumption *already
  holds*, so *whether* it has fired yet changes speed, never the value computed (G1) — the
  result is timing-independent by construction. Two runs that reach "hot" at the same
  deterministic count patch at the same logical point.
- **Testable.** A `FASTLLVM_JIT_TRACE` dump (seam, count, chosen target, emitted bytes hash)
  must be **identical across repeated runs** of a fixed input — a gate in the test suite,
  exactly like the 0-live oracle.

Net effect: modification-time cost is a few patched bytes + i-cache flush + safepoint;
**steady-state after re-sealing is one predictable branch**, often folded away when a seam
can gain no further impls; and every run of a fixed input is bit-for-bit reproducible.

---

## 6. Memory management across the open world — RC stays, no GC

**Constraint (from the design goals): no garbage collector.** The open world must keep
Vire's **deterministic reference counting** — objects freed at a defined point, no tracing
pauses, no scheduler-dependent reclamation. Dynamism must not smuggle in a GC as its safety
net. The rule: **an RC/arena proof may only be applied on paths that cannot reach an open
seam; across a seam the frozen-ABI RC contract is mandatory — and the load verifier keeps
the graph RC-collectable so no tracing collector is ever needed.**

- **Cycles are handled at the boundary, not by a background collector.** RC alone cannot
  reclaim a reference cycle; a tracing collector would violate "no GC" and determinism. So
  instead of linking a collector in dynamic builds, the **load verifier enforces
  acyclicity-or-explicit-weak** (§6, gate): a module whose types could close an ownership
  cycle with the host graph must declare the back-edge `weak` (a non-owning ref, like a
  sealed Vire program already can) — else it is **rejected**. This pushes Vire's existing
  static ownership discipline to the module boundary, keeping the whole live graph
  RC-reclaimable and deterministically freed. *(The cycle collector remains available only
  for programs that already opt into it sealed; dynamism never forces it on.)*
- **RC elision / stack-alloc / immortal analysis are scoped to a single AOT module** and
  to intra-module, non-seam paths. Any value that flows *through* a seam call gets **real
  `jrt_retain`/`jrt_release`** at the boundary — the callee is a black box that may retain
  it, so the +1 must be real. Determinism is preserved: RC frees at the same point every
  run, whether or not the JIT has specialized the surrounding code (G1).
- **Arena promotion is forbidden across a *plain* seam.** `while_arena_safe` already
  declines when it cannot prove non-escape through a callee; a plain `dynamic`/`open`-typed
  callee is **opaque** to it (an override could store the arena object somewhere that
  outlives the pop). This is a *conservative extension of an existing check* — the arena
  simply never fires on a loop whose body touches a plain seam. Sealed loops are untouched.
  **Exception (the recovery lever):** a seam declared `noescape` carries a contract the load
  verifier enforces on **every** impl (§11.1), so no override *can* escape — and then the
  arena **is allowed to fire across it**. This is how the open world gets its arena back
  where it matters.
- **Loaded instances carry a field-ref map** (from their descriptor, §3) so the RC
  **drop/release** path walks them and decrements their referents without static layout
  knowledge — the map records **element classes of ref arrays** (Vire has typed object
  arrays) so array drops decrement every element exactly once.
- **Redefinition preserves old instances** under their old, versioned layout; a new layout
  version coexists with instances of the old one, each freed by its own versioned drop.

**The load-time soundness gate (the single most important safety mechanism):** a module is
**verified before it is linked**, and rejected — not linked — if it would break memory
safety. The verifier checks, from the candidate module's manifest against the host
registry:

- **Layout/ABI compatibility** — every shared type has the frozen field layout, header
  format, and dispatch-slot assignment; `VIRE_ABI_VERSION` matches.
- **RC-contract conformance** — the module's boundary methods use real RC (no smuggled
  arena/immortal pointers escaping across the ABI); their effect summaries (§3) are
  consistent with how the host devirtualized/elided around the seam.
- **Thread discipline** — any type crossing to another thread is `Send`/`Sync` per Vire's
  existing checks; a module that would let a non-`Sync` value be shared is rejected.
- **Acyclicity-or-weak (the no-GC gate)** — if linking the module's types to the host graph
  could close an ownership cycle, the closing edge must be declared `weak`; otherwise the
  module is rejected. This is what lets the open world stay **RC-only, collector-free, and
  deterministic** (§6).

A module that fails verification does not load. This turns "dynamic loading is unsafe" into
"dynamic loading is a checked link step" — the same philosophy as Vire's compile-time
soundness, moved to load time.

---

## 7. Performance model — pay only for the seam, reclaim even that

| Code | Cost |
|---|---|
| Sealed program (no `open`/`dynamic`) | **Identical to today.** Same benchmarks, same 0-live. |
| Sealed code that never reaches a seam, in a dynamic build | ~Same; no collector linked (RC-only, §6) — just the loader/registry, idle-cheap. |
| A cold `open` call | one indirect call (pointer slot). |
| A hot `open` call, post-JIT re-seal | guard + **direct** call; guard folds away when the seam can't change again. |
| Value crossing a seam | real retain/release at the boundary (unavoidable and correct). |
| A modification event | a few patched bytes + i-cache flush + safepoint — *transient*, not steady-state. |

The design goal the user asked for — "modify while running, minimal overhead, keep
performance" — is met by making the *steady state after a modification* collapse back
toward direct-call speed via §5.1, so you pay the indirection **around** a change, not
forever after it.

---

## 8. Reuse map (what changes where)

| Piece | Change |
|---|---|
| `crates/ir` | Unchanged — the AOT lowering target for every module. |
| `crates/backend` | **Emit** the type descriptors + method tables (§3) it currently discards; mutable-slot + patch-point + immediate-hole emission (§4, §5); `-fPIC`/`-shared` module output; per-site "collapsible/hole" marks + embedded verified IR for the JIT. |
| `crates/solver` | Seam-aware: scope devirt/RC-elision/arena to non-seam paths (§6); guarded devirt instead of unconditional at seams. |
| `crates/vire` (frontend) | Parse `open`/`dynamic`/`@extensible`; seam-typed values opaque to `while_arena_safe`; `@typeinfo` over the descriptor. |
| `crates/driver` (runtime.c → `virt`) | Module loader (`dlopen`/custom for freestanding), static+dynamic registries, method-table + trampoline patching, W^X + safepoints, the **JIT (Tier A patcher + Tier B specializer)**, the **load-time verifier** (§6), M2 compile-cache. **No interpreter.** |

The JIT and the load verifier are the only genuinely new subsystems; everything else
extends existing machinery (or is a direct port of FastJavaC's proven code).

---

## 9. Phased roadmap (each phase independently shippable & 0-live-heap gated)

- **P0 — reflection API + ABI freeze & runtime metadata.**
  - *Step 1 — static reflection API — DONE (2026-07-26).* `type_name(x)` / `field_count(x)`
    / `abi_version()` (`VIRE_ABI_VERSION = 1`), resolved at compile time from the type graph
    (`crates/vire/src/lower.rs`). In a sealed build a value's runtime type equals its static
    class, so these are exact today; no runtime metadata table is emitted yet (deferred to
    step 2 / P1, when a *loaded* type's descriptor must be read at runtime — avoids shipping
    a dead table). 0-live, Vire-only (Java oracle 67/67 unaffected). `tests/vire_reflect.sh`,
    `examples/vire/reflect.vr`.
  - *Step 2 — runtime descriptor table + registries* (folds into P1, where the loader
    consumes it): serialize per-type descriptors (§3), version the ABI in the binary, and
    upgrade `type_name`/`describe` to read a loaded value's descriptor. *Test:* the sealed
    core introspects its own types via the table; oracle unchanged.
- **P1 — the sealing model + native module ABI (M1) + the no-GC verifier.**
  - *Step 1 — scalar-only M1 round-trip — DONE (2026-07-26).* `vire --emit-module` builds a
    PIC `.so` exporting a scalar C-ABI entry `vire_module_main(i64)->i64` (from `fn
    module_main(x: Int) -> Int`) + a `vire_module_abi` version constant; the host loads it
    with `load_module(path) -> handle` (`dlopen` + **ABI-version check** — the first
    verifier gate) and calls it with `module_call(handle, arg)`. Scalar-in/-out only, so no
    object crosses the boundary and there is no cross-module RC/arena question (the scalar-
    capsule discipline). A missing / non-module / wrong-ABI `.so` → handle 0, never a crash
    or a wrong call. `tests/vire_module.sh` (4/4, 0-live); Java oracle 67/67 unaffected
    (the loader is section-stripped when unused; freestanding excludes it).
  - *Step 2a — sealing model: parsing + optimizer scoping — DONE (2026-07-26).* `dynamic fn`
    / `open fn` / `open trait` / `open type` parse (keywords `dynamic`/`open`, sealed stays
    the keyword-free default). The memory-safety-critical part: a call to a `dynamic`/`open`
    **fn** is a hard black box to region inference (`crates/vire/src/lower.rs`) — a future
    override could escape/retain its args — so the **loop-arena declines at the seam** while
    an identical *sealed* builder still gets the arena, both 0-live, same value.
    `tests/vire_sealing.sh` (4/4). Vire-frontend-only (Java oracle unaffected). *(Trait-
    method seam dispatch is 2b; `open trait`/`open type` currently parse but don't yet route
    trait calls through a mutable slot — that's the override step.)*
  - *Step 2b — shared-type ABI + object-boundary verifier.* objects (not just scalars) cross
    the module boundary with frozen layouts + real boundary RC; the **load-time verifier**
    incl. the **acyclicity-or-weak gate** (§6) so the open world stays RC-only. *Test:* a
    two-module 0-live program; a layout-incompatible and a cycle-closing module are both
    **rejected**, not linked. **The soundness contract is proven here; do not compress it.**
- **P2 — redefinition by pointer swap + guarded devirt (override + mixin capability).**
  Mutable slot; a module overrides a `dynamic fn`, and an injection override calls through
  to the previous pointer (native "around"). *Test:* new behavior on next call; the mixin
  override still invokes the original; heap balances; arena still declines on seam loops.
- **P3 — JIT Tier A: dispatch collapse & re-sealing (§5.1) + the determinism gate.** Emit
  guarded-direct patches for seams proven hot by the **deterministic counter**; repatch to
  slow path on a new override. *Test:* a hot redefined method returns to near-direct-call
  speed; a subsequent override deopts correctly; heap balances; **`FASTLLVM_JIT_TRACE` is
  bit-identical across repeated runs of a fixed input** (determinism gate, §5.3). **First
  delivery of the "reclaim performance" ask.**
- **P4 — compile-on-load cache (M2).** Loader compiles `.vr`/IR modules via a `vire`
  subprocess, keyed by content+ABI+deps, cached. *Test:* load source at runtime → runs
  native first and subsequent times; second load hits cache.
- **P5 — JIT Tier B (specializing recompiler, §5.2) + trampoline patching + safepoints.**
  Recompile a hot function from verified IR specialized to resolved facts (semantics- and
  safety-preserving, deterministic, bounded budget); W^X in-place redirect; immediate-hole
  filling; cross-thread safepoints. *Test:* a specialized hot method is faster with
  identical output; determinism trace stable; specialization-budget eviction bounded; heap
  balances. **Gated on P1–P2 first showing the indirect-dispatch tax actually matters
  (risk #6) — Tier B is built only if measured.**

Ordering rationale: P1 (sealing + verifier) is the soundness spine and must precede any
codegen-at-runtime. The JIT (P3, P5) is layered on a *correct* dynamic dispatch, so
even if it were removed the program stays correct — the JIT is a pure performance layer
over a sound base, never a correctness dependency.

---

## 10. Principal risks & open decisions

1. **Seam granularity — DECIDED (§2.2).** Finest practical unit: per-`open trait` /
   per-`dynamic fn`, sealed by default, explicit, with `open module` as opt-in coarsening
   sugar. Under-annotation fails safe (a needed-but-unopened seam ⇒ the mod is rejected at
   load, never a silent miscompile). Remaining sub-question: whether `open module` should
   also be inferrable from a manifest for a pure plugin-host boundary crate.
2. **W^X + safepoints under threads.** In-place patching a method another thread is
   executing needs a safepoint protocol (park at RC-safe points). Single-threaded first
   (P3), cross-thread deferred (P5) — same staging FastJavaC used.
3. **The arena across seams — verify the conservative story is *actually* conservative.**
   The claim "seam-typed callee ⇒ arena never fires" must be pinned with must-decline
   tests in `vire_interproc_arena.sh` *before* any dynamic build ships, exactly as the
   existing arena work was gated.
4. **Freestanding/seL4.** No `dlopen`, no subprocess ⇒ **M1-only + a custom PIC loader**;
   the JIT needs a W^X story on bare metal (a mapped RWX scratch region). M2 is
   host-only. Detect toolchain absence and report, don't fake.
5. **Trust boundary.** M2 compiles + `dlopen`s third-party code = executes native
   third-party code in-process (trusted-input model). Sandboxing untrusted modules is
   process isolation, a separate concern — documented, not silently assumed.
6. **Do the JIT's gains justify it over pointer-swap alone?** *Measure first:*
   ship P1–P2 (no JIT) and quantify the indirect-dispatch tax on a realistic
   plugin-heavy workload. Build JIT Tier B (P5) only if that tax is shown to matter —
   the same "measure before shipping soundness-critical complexity" discipline the rest of
   the project follows.

## 11. Additional concepts worth adopting (beyond the core), ranked

Ordered by value-to-risk. The first two are the headline additions — the rest are natural
extensions once the spine (P0–P3) exists.

1. **Effect-contract seams — the biggest performance recovery (do this).** The sealing model
   gives up the arena and RC-elision at *every* seam because an unknown override *might*
   escape/retain. Let a seam carry a **contract the load verifier enforces on every impl**:
   `dynamic noescape fn` (args/`self`/result provably don't escape the call),
   `dynamic pure fn` (no observable effect, no retain), `noalloc`, `nopanic`. If a seam is
   `noescape`, the host **keeps the arena firing across it** — no impl can break the escape
   assumption because the verifier *rejects* any that would. This turns "seams are slow"
   into "seams you constrain are fast," recovering exactly the optimization the open world
   costs, with the guarantee moved to load time. **Highest value; directly attacks §7's
   only real tax.** (Extends the existing RC/arena effect summary in §3.)
2. **Composition warm-image (ahead-of-load whole-program re-optimization).** When a *stable*
   set of modules is loaded (a game + its known mod list, a server + its plugins), optionally
   produce a cached, **re-sealed, fused native image of the whole composition** — M2 applied
   to host+plugins *together*, so cross-module calls that were ABI edges get inlined/devirt'd
   as if closed-world. Keyed by the set's content hashes (deterministic, item 6). Gives
   *near-closed-world speed for a known plugin set* while keeping full dynamism for the
   unknown case. The JIT handles the transient/unknown; the warm-image handles the settled.
3. **Polymorphic inline caches at seams.** Beyond the monomorphic guard (§4.2): a small N-way
   PIC for seams that see a handful of impls — still counter-driven and deterministic (§5.3),
   still deopts to the slow path on the (N+1)th type. Cheap, and covers the common "2–3 mods
   implement this hook" case without falling to the indirect slot.
4. **Capability/effect-scoped modules (sound, not process isolation).** A module *declares*
   which host capabilities it may use — FFI, threads, specific host APIs, `unsafe`/`native`
   — and the verifier **enforces** it at load (a module that reaches beyond its declared set
   is rejected). Type-/effect-level containment reusing the descriptor metadata; not a
   sandbox for actively-malicious native code (that's process isolation, still out of scope),
   but real, checked least-privilege for the trusted-input model.
5. **Deterministic hot-swap & unload (RC makes this clean).** Because reclamation is RC, not
   GC, a module can be **atomically hot-swapped or unloaded at a safepoint**: drain its live
   instances under their versioned drop, revert its patches, free — at a *defined,
   reproducible* point, not whenever a collector runs. Live code update with a deterministic
   reclamation semantics a GC'd runtime cannot offer.
6. **Content-addressed modules + reproducible composition.** Identify each module by a
   content hash; load resolves deterministically (fixed order, hash-pinned deps). The entire
   running composition — which modules, which versions, which seams resolved to which impl —
   is then **reproducible from the input set**, extending the JIT's determinism (G3) to the
   whole system: same module set + same inputs ⇒ same run, enabling record/replay debugging.
7. **Comptime-generated ABI glue & manifests.** Use Vire's existing comptime layer to
   generate the module manifest, descriptor, and boundary glue from the seam declarations —
   comptime-checked, so an ABI mismatch is a *compile* error on the module side, before it
   ever reaches the load verifier.
8. **Speculative whole-program sealing (research, not default).** Treat even unannotated code
   as sealed *speculatively*, guard it, deopt on a load that violates — recovering speed
   without explicit `open`. **Risk:** the arena is memory-safety-critical, so a mis-speculation
   is a UAF, not a slowdown; would need deopt *before* any arena free commits. Parked as a
   research direction, explicitly behind the safe sealed-by-default model.

---

## 12. Non-goals (deliberate)
An **interpreter** or a *bytecode/source-executing* engine (M3) — the JIT is allowed but
only as a **semantics-preserving, memory-safe, deterministic** specializer of already-
verified native code (§5), never a runtime compiler of new source · a **garbage collector**
as dynamism's safety net — the open world stays **RC-only + deterministic**, cycles handled
by the load verifier (§6) · runtime *type* synthesis / dynamic typing · unsealing the whole
program by default · a JIT whose output depends on wall-clock/scheduling (non-determinism) ·
sandboxing untrusted modules within the process · inlining **across an open seam** or in
the **host→plugin** direction (the host never sees plugins; those edges stay real calls +
real RC). *(Note: plugin→host-**sealed** inlining is explicitly allowed and encouraged,
§2.1 — it is not cross-seam and cannot go stale.)*
