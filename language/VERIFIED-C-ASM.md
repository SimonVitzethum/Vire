# Verified inline C / asm — the sound replacement for `unsafe`

Vire is *safe by construction*: there is no `unsafe` keyword. Hardware access
(MMIO registers, syscalls, SIMD intrinsics, device drivers) still needs raw memory
and instruction access the safe subset cannot express. Rust's answer is `unsafe` — a
block the compiler trusts **blindly and wholesale**. Vire's answer: **inline C/asm
that must pass a memory-safety *proof*.** A block that cannot be proven safe is a
**compile error**, not a runtime hazard.

The prover is [CSolver](../../CSolver) — a formal memory-safety verifier whose native
input is exactly LLVM-IR and x86/ARM assembly, which is exactly what Vire emits. The
pipeline is therefore lossless:

```
native "c" """ … """   ──clang──▶  LLVM-IR   ──CSolver.verify──▶   PASS ? accept : compile error
   (or asm { … })                                                    │
                                                       FAIL: counterexample
                                                       UNKNOWN: residual obligations
```

## Status: working prototype

Implemented (opt-in, `--verify-c <solver-bin>`): every `native "c"` block is compiled
to LLVM (`clang -O0 -emit-llvm -g`) and run through `solver verify`. The block is
accepted only on a **proven-safe** verdict (exit 0 = PASS).

```
$ vire build --verify-c solver safe.vr   # native "c": bounded a[0..3] on int[4]
verify: native "c" block 0: PASS (proven memory-safe)     → binary produced

$ vire build --verify-c solver oob.vr    # native "c": a[i]=7 on int[4], i unbounded
error: native "c" block 0 is not provably memory-safe (rejected instead of trusted
       like `unsafe`):
    FAIL PO5 [in_bounds] @ llvm:oob#7
    UNKNOWN PO1 [valid_pointer_arith] @ llvm:oob#5     → NO binary
```

This is the core mechanism: unsafe C is refused; safe C is admitted with a proof.

## The precise guarantee (honest)

`PASS` means **proven safe under the explicitly reported assumptions** — never a
false PASS. Full memory safety of arbitrary machine code is undecidable, so a block
lands in one of three states:

- **PASS** — proven free of OOB, use-after-free, double-free, null-deref, dangling
  deref, integer UB (div-by-zero, shift past width, signed over/underflow), bad
  pointer arithmetic. Accepted.
- **FAIL** — a concrete counterexample exists. Rejected, counterexample shown.
- **UNKNOWN** — the prover cannot decide (a theory limit, or a fact the code does not
  establish). Rejected by default, with the residual obligation named.

So the claim is **not** "inline C/asm cannot produce errors" — that is impossible in
principle. The accurate claim is:

> **Every inline block is machine-verified memory-safe, except for a small set of
> explicitly named device/hardware axioms at the true trust boundary.**

That is strictly stronger than Rust's `unsafe`, which trusts the *entire* block with
no proof and no named boundary.

## Why Vire wins where raw C / Rust `unsafe` cannot

CSolver on **raw C** must *assume* many preconditions, because C guarantees nothing
about them — a bare `int *a` could point anywhere. Those become `UNKNOWN`:

```
UNKNOWN PO5 [valid_write] … residual: uncontracted pointer parameter
```

But Vire's caller is **fully typed and safe**: a Vire array *is* a proven `(ptr,
len)`; a Vire reference is proven non-null. Vire therefore **discharges** exactly the
contracts C leaves open, and passes them into the proof. A block that is UNKNOWN as
standalone C becomes PASS when its caller is Vire. This is the unique advantage: the
verified boundary is *tighter* than anything achievable in C or in Rust `unsafe`.

In the prototype this is modelled by `--assume-valid-params` (raw-pointer params are
sized-and-valid — the Vire contract). The full design synthesizes the *exact* per-block
contract from the Vire call site instead of a blanket flag (below).

## Design part 1 — the contract, synthesized from Vire types

A `c { … }` / `asm { … }` block has an **interface**: the Vire values that flow in and
out. Vire already knows their types with proof, so the contract is *derived*, not
written by hand:

| Vire value at the boundary | CSolver precondition handed to the proof |
|---|---|
| `a: [Int]` / `array(n)` | `a` is a valid buffer of exactly `n` `Int`s (`--pre` element count) |
| `r: &T` (reference) | `r` is non-null and points to a valid live `T` |
| `s: Str` | `s` is a valid `(ptr, len)` byte buffer; length known |
| `x: Int` scalar | value passed as-is; provenance N/A |
| return `-> T` | the block must establish `T`'s validity to return it into safe Vire |

These map onto CSolver's existing precondition surface (`--pre <file>` carries
bytes/elements/cstring facts; `param-buffer-len` recognises `(buf, len)` pairs;
`slice-abi` is the Rust analogue). The Vire→CSolver bridge emits a `.pre` contract per
block automatically from the call-site types. Result: the programmer writes **no**
safety annotations for the common case — the safe caller pays for the proof.

## Design part 2 — `@assume`: the only controlled trust boundary

Some obligations are **irreducible from software alone** — no proof, in any language,
can discharge them, because the fact lives in hardware:

- the extent of an `ioremap`'d MMIO mapping (the device's, known only at map time);
- a raw device address materialised by `inttoptr`;
- a loaded register field constrained by a datasheet invariant (`size ∈ {1,2,4,8}`).

For these — and *only* these — Vire offers `@assume`, the single, named, auditable
escape valve:

```vire
@assume(mmio_extent(regs, 0x1000))          // datasheet: this mapping is 4 KiB
@assume(field_range(desc.size, 1, 8))       // datasheet: size ∈ [1,8]
c {
    regs[offset] = value                     // offset proven < 0x1000 GIVEN the assume
}
```

Semantics:
1. `@assume(P)` injects `P` as a precondition into the block's proof (CSolver's
   `--assume-*` family / labelled provenance regions).
2. The proof still runs — everything *around* the assumed fact (the arithmetic on
   `offset`, the bounds *given* the extent, aliasing) is **proven**, not trusted. The
   assume only closes the one hardware fact.
3. Every `@assume` used is **recorded in the block's proof tree and its build log**, so
   a `PASS` is never silently bought. `vire audit` (planned) lists every assumption in
   a program and its justification.

This is the difference in kind from `unsafe`: `unsafe { *(0x1000 as *mut u32) = v; ptr[i] = w; }` trusts the address, the arithmetic, the bound, and the aliasing — all
of it, unnamed. `@assume(mmio_extent(...))` names the *one* hardware fact and proves
the rest.

**Sound by default.** With no `@assume`, an irreducible obligation stays UNKNOWN and
the block is *rejected*. You cannot accidentally get an unproven hardware access — you
must write the assumption down, and it is logged.

## Integration architecture

- **Where:** after lowering, before the `native "c"` block is compiled into the binary
  (the gate the prototype adds in the driver's native-block loop). For `asm { }`, the
  block lowers to an LLVM `call asm` and the same verify step runs (CSolver models
  register-only asm like `rdtsc`; memory-clobbering asm needs supplied semantics).
- **How:** CSolver exposes a library API (`crates/verifier/src/lib.rs`), so the mature
  form is a **crate dependency** — no subprocess, no text parsing, structured verdicts
  and obligations. The prototype shells out to the `solver` CLI to stay decoupled while
  the interface settles.
- **Caching:** verification is content-addressed per block (hash of the C/asm + the
  synthesized contract). An unchanged block is not re-verified — important because CDCL
  SAT + symbolic execution are compile-time-expensive. Hardware blocks are small, so
  per-block cost is bounded.
- **Failure surface:** FAIL/UNKNOWN verdicts render as ordinary Vire compile errors,
  carrying CSolver's counterexample or residual obligations and the `@assume` that
  would close each one — the same "here is the minimal missing assumption" CSolver
  already produces.

## Honest limits

- **Compile-time cost.** SAT + symbolic execution is expensive; tractable for small,
  well-typed interop blocks, not for embedding large C bodies. Keep blocks small.
- **UNKNOWN = false rejection.** Some genuinely-safe blocks the prover cannot decide are
  rejected. The programmer closes them with a contract/`@assume`, or restructures — the
  price of sound-by-default (never a false accept).
- **asm is harder than C.** Register-only asm is easy; memory-clobbering asm needs a
  supplied semantics before it can be proven.
- **CSolver maturity.** Decided rate is ~45 % on *kernel* code; Vire interop blocks are
  smaller and arrive with proven caller contracts, so the practical rate is far higher —
  but this is an evolving prover, not a finished oracle.

## Roadmap

1. **[done] prototype gate** — `native "c"` verified via CSolver, opt-in `--verify-c`.
2. First-class `c { … }` / `asm { … }` **expression blocks** with typed in/out bindings
   (not just top-level `native`).
3. **Auto-contract synthesis** — emit the per-block `.pre` from the call-site Vire types
   (replaces the blanket `--assume-valid-params`).
4. **`@assume` surface** + `vire audit` (list every assumption + justification).
5. **CSolver as a crate dependency** (structured verdicts; no subprocess).
6. Verification **cache** (content-addressed per block).
