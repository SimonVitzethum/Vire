# Vire — hardware and registers without `unsafe` (Gabbro principle, no formal proof)

Status: design (2026-10-12, AGENTS.md §3). The compiler is **not** formally
verified (unlike Gabbro's Lean goal theorem); the guarantee is carried by the
checker (reject, don't trust) + the test suite + `forbid(unsafe_code)` on the
Rust side. Principle borrowed from Gabbro: *the user proves only their own
logic and named hardware assumptions; the language carries the rest.*

## 1. Rules

- **No `unsafe` keyword, no blind `native` escape, no `int -> ptr`.** A number
  is never an address. What Gabbro enforces by proof, Vire enforces by
  rejection: anything the checker cannot see through is error, not UB.
- **Rust side:** `#![forbid(unsafe_code)]` workspace-wide (no `unsafe {}` in
  `crates/*`). Handwritten C/asm in a shipped binary only as a **named entry
  stub** (before any stack exists), each with a written reason + test + count.
  The C the *emitter writes* from Vire is not covered by this rule — but every
  emitted form comes from a checked construct below, never from a user string.
- **Vire side:** hardware access only through typed items (see §2). `native
  "c"/"c++"/"asm"` blocks and `--noverify`/`--unsafe-c` are removed; the
  CSolver verifier gate stays as an internal pass for *emitter-generated* glue
  only (e.g. the `spawn` shim), never for user strings.

## 2. Language surface (`hw` module, std)

```vire
use hw::{Reg32, Mmio, Port, syscall, region}

// A memory-mapped register block DECLARED with its extent — not a number.
// `base` comes from the target profile (--target board profile), never from
// an integer literal in user code.
mut uart = Mmio::bind("uart0")            // profile names extent + contract
uart.write::<Reg32>(0x00, 0x60)           // checked offset < extent, volatile
let s = uart.read::<Reg32>(0x04)

// Raw port I/O (x86) — same declaration shape.
Port::outb(0x3F8, b)                      // Port::inb / outb only; no raw asm

// Syscalls are declared items with ABI + cost + contract, not libc wrappers.
syscall sys_write(fd: Int, buf: Bytes, n: Int) -> Int
```

- `Reg8/16/32/64`: volatile cell types; ordinary `=`/`as` never applies to
  them (no accidental non-volatile access); arithmetic on register *values*
  only after `read` (a plain `Int` copy).
- `Mmio::bind(name)`: the name must exist in the target profile (checked file,
  part of `--target`); unknown name → error E400 with the list of known names.
  Offsets are `comptime` bounds-checked against the declared extent.
- `syscall`: declaring one states registers/numbers/costs in the item; the
  Lean-style full proof is out of scope — the checker pins the *shape* (arity,
  types, clobbers) and the suite pins the *behavior* (QEMU test per profile).
- Interrupts/DMA (`intr`, `dma::Region`): same shape — declared extents with
  ceilings, refuse-on-full, never a bare pointer. DMA buffers are `region(max)`
  (see OWNERSHIP-STATIC.md §2.4): the device writes only inside the declared
  extent, the compiler emits the fence from the declaration.

## 3. FFI (narrowed)

- Only `extern "C" header "h.h"` with a checked mapping: scalars + `extern`
  structs with explicit layout; function pointers/varargs/struct-by-value that
  the checker cannot map are **rejected** (listed, with reason), not passed
  through.
- C++/Rust only via their C-ABI surface (same boundary as every non-C++/Rust
  language — see EVALUATION.md §1.C, unchanged).
- `Ptr` stops being `i64`: it becomes an opaque handle type that can only come
  from a declared item (`Mmio::bind`, `array_data` inside `kernel` modules,
  header bindings) and can only be consumed by one. `ptrtoint`/`inttoptr` in
  the backend outside those paths is an ICE.

## 4. Migration steps (each green-gated)

1. **H1 — `forbid(unsafe_code)`** workspace-wide + remove `--noverify`/
   `--unsafe-c` (default becomes the only mode). `native` blocks without a
   proof become error E410 (with hint). *Glue exemption:* emitter-generated
   shims (`c-glue`, e.g. spawn) stay, counted in one registry.
2. **H2 — `hw` surface:** `Reg*`/`Mmio`/`Port`/`syscall` items + target
   profiles with named regions; first QEMU-backed test per profile.
3. **H3 — `Ptr` opacity:** `Ptr`-as-`i64` removed from user paths; backend
   `ptrtoint`/`inttoptr` gated to declared items (else ICE).
4. **H4 — FFI narrowing:** header-binding checker rejects unmappable shapes;
   `pybridge.c` stays deleted (already removed Phase 1); Python only via
   explicit subprocess/FFI, never an implicit shim.

## 5. Diagnostics (named, counted)

Every rejection has a code + must-reject test (promote *and* decline):
E300 unboundable dynamic graph (ownership doc), E400 unknown hardware name /
out-of-extent offset, E410 unverified native block, E420 unmappable FFI shape,
E430 int↔ptr conversion outside declared items.
