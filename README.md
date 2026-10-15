# Vire

> **Goal (binding, see `AGENTS.md` §0): at least as fast as Rust, lighter than
> Python, fully typed, memory-safe without `unsafe` — including hardware and
> registers. Gabbro is the target. Java-IR, GPU/Vulkan, Runtime/GC, LLVM are out.**

**Vire** is a programming language: *as light as Python, as fast as C/Rust,
memory-safe — without lifetimes, without ownership syntax, without manual memory
management, without loop/cost/contract annotations.* It compiles to
**Gabbro source** (`.gab`). The Vire compiler itself is **unverified**;
correctness flows through the Gabbro toolchain (check + certificate +
validating ARM emission) once it lands.

> Name from the Latin *vīrēs* ("forces, strength") — light, yet powerful.
> File extension `.vr`. Current state: **not yet fully functional on the target
> path** — the Gabbro emitter covers the bounded subset (integers, counted loops,
> strings, const arrays, int-payload sums, `region(max)` arenas, static object
> rows, scalar `extern`); everything else is a loud `E5xx` rejection, never a
> silent miscompile. Execution truth until Gabbro's validating compiler lands:
> `vire run` (paused LLVM oracle, no new work).

```vire
fn add(a: Int, b: Int) -> Int {
    a + b
}
fn main() {
    mut s = 0
    for i in 0..10 {
        s = s + i
    }
    print(s)      // 45
}
```

Reads like Python — compiles to checked `.gab` (`gabbro check`, 0 errors) or is
rejected with a code.

## The idea in one paragraph

Classically, memory safety costs one of three things: a garbage collector
(pauses), ownership + lifetimes (Rust's annotation burden), or reference
counting (a small runtime). Vire resolves this **per program site** with a
whole-program solver: owned values are freed at last use, pure reads are
inferred borrows (invisible, no syntax), bounded pools are explicit regions
with ceilings (`region(max)`), and anything without a static ceiling is a
**compile error with a code** — never an implicit runtime. Unbounded-shared
graphs exist only as explicit kernel `Rc[T]` (opt-in, never implicit). The
compiler emits **at most one** memory unit per program (stack/`free`/pools/
drops, deduplicated) — **no GC, no RC, no collector, nowhere**, and in the end
no `runtime.c` at all. This is feasible because Vire is **closed-world** (all
sources at compile time) and Gabbro carries tables + `ptr` handles, arenas with
constant ceilings, and statically bounded loops.

Division of labor: **the solver infers** (so the user never writes bounds,
costs, or contracts — ergonomics), **Gabbro verifies** (every emitted bound,
cost promise, and effect is held against its checker).

## Status & architecture

| Layer | Status |
|---|---|
| **Vire front-end** (`crates/vire`) — lexer, parser, inference (no silent `Int` default, `E100`), lowering to SSA IR | working; fully typed or compile error |
| **Mid-level IR** (`crates/ir`) — Vire-only SSA | working |
| **Solver** (`crates/solver`) — ownership/borrow (`own.rs`), bounds, escape, direct-free, explicit pools + `E300` | working; bound proofs feed the Gabbro path |
| **Gabbro backend** (`crates/backend/src/gab.rs`) — `.gab` emission, `E500/E501/E502` rejects | **the only target under construction** — `tests/vire_gab.sh` |
| **LLVM backend** (`crates/backend/src/lib.rs`) — textual IR + `clang` | **paused**: builds and runs as **differential oracle** (`vire run`, bit parity), no new work |
| **Runtime** (`crates/runtime/runtime.c`) | **to be fully deleted**; kept only for the LLVM oracle until Gabbro validates |
| **Hardware** (`hw::Reg/Mmio/Port`, `syscall` items, `fence`) | typed language forms, no `unsafe` |

Rust side: `#![forbid(unsafe_code)]` workspace-wide.

## Memory model (userspace: Gabbro carriers; kernel: explicit `Rc`)

1. **Owned values** (default): exactly one owner, move by default, `free` at
   last use — proven by the solver, no counter.
2. **Borrows (inferred, invisible):** field/param reads without mutation are
   borrows. No syntax; rejection on doubt.
3. **Regions with ceilings:** `region(N)` / loop arenas only with statically
   known bounds; overflow is a checked `refuse` path, never UB, never silent growth.
4. **Forbidden in userspace:** cyclic heap graphs with dynamic lifetime and no
   ceiling → compile error with fix suggestion, never RC fallback.
5. **`Rc[T]` kernel-only:** explicit `use kernel::Rc`, kernel modules only,
   budgeted. Userspace `Rc` does not compile.
6. **Unbounded-dynamic without C runtime:** unique ownership carries any size;
   shared ownership only via kernel `Rc[T]` as a plain Vire library type.

## Hardware / registers without `unsafe`

No `unsafe` keyword, no blind `native "c"/"asm"` (removed, `E410`), no
`int -> ptr`. Instead: `hw::Reg[T]`, `hw::Mmio[T]`, `hw::Port`, `syscall` items
with declared signature + cost + contract; `volatile`/`fence` semantics from the
declaration. FFI only as `extern "C"` declarations with a checked mapping
(scalars; `Ptr` never crosses to Gabbro); C++/Rust via the C ABI behind
`extern "C"` wrappers linked with `--obj`/`-l`. What is not mappable is
rejected, not waved through. (Gabbro has no `extern "C"` form: scalar decls map
to `extern fn` heads, held at `gabbro link`.)

## Typing (complete)

No default type — uninferable is a spanned error, never an `Int` fallback
(`E100`). `Result[T,E]` with **two** type parameters (no i64 erasure),
`Option[T]` likewise; `?`/`wrap`/`match` know the mono instance. Generic traits
+ coherence (overlapping impls = error), value generics `[N]` monomorphized,
iterator invalidation (mutation during iteration = error, `snapshot()` as the
explicit way out).

## Building & checking programs

```console
$ vire build hello.vr --emit=gab -o hello.gab   # the target: Gabbro source
$ gabbro check hello.gab                         # unchanged oracle binary, must report 0 errors
$ vire run hello.vr                              # execution truth until Gabbro's compiler lands (LLVM oracle)
```

Every emitted `.gab` file must pass `gabbro check` with 0 errors. The C emitter
(`gabbro build` via C11) is **deprecated and unused**. What `gabbro check`
rejects or what exceeds Gabbro scope (O29 dynamics, `Rc`) is rejected on the
Vire side with a code (`E500` not emittable / `E501` static bound needed /
`E502` shared ownership) instead of cheating. After every step:
`cargo build -p vire` + `tests/vire_heap.sh` green; the full gate is
`tests/gate.sh`.

Common flags: `-o FILE`, `--emit=gab|llvm|ir|obj|asm|staticlib`, `--deps FILE`,
`--target TRIPLE`, `-g` / `--backtrace`, `--log-level …`. The LLVM-family flags
exist only for the oracle; new compiler work goes exclusively into the Gabbro path.

## Benchmarks (honest)

No valid performance numbers exist yet: performance counts **only** through the
validating Gabbro compiler (best-of-N, bit-identical, RSS alongside) — never
through C11. There is no pessimism path in the compiler.

## What is gone (not rebuilt without a decision)

- Java path (`classfile`, `frontend`, `fastjavac`, slots/boxing/`int`-only index).
  IR is Vire-only SSA.
- GPU/Vulkan (`nvptx.rs`/`spirv.rs`, `gpu_runtime.c`/`vk_runtime.c`, `shader.rs`,
  `cuda-oxide`, `@gpu`/`@vulkan` attributes, `VkIface`/`GpuKernel` IR). Pure CPU.
- `runtime.c` as a product (RC + Bacon–Rajan + slab + arenas): replaced by emitted
  static memory management, then **fully deleted** (kept only for the oracle).
- Blind escapes: bare `native "c"/"c++"/"asm"`, `--noverify`/`--unsafe-c`,
  `extern` with pointer arithmetic, `Ptr`-as-`i64`, implicit pybridge shims.
- LLVM backend expansion: paused, oracle-only (see above).
- Vendored CSolver, `vire-wasm`, VS Code extension, benchmarks, stdlib stubs,
  seL4 bring-up, Java examples. (All recoverable from git history.)

## Documents

- **[AGENTS.md](AGENTS.md)** — binding goals, memory model, work rules. Wins over
  this file on conflict.
- **[TODO.md](TODO.md)** — open work only.
- **[LICENSING.md](LICENSING.md)** — license: everything under GPL-3.0-or-later.
- **[tests/](tests/)** — `vire_gab.sh` (target gate), `vire_heap.sh` (oracle
  heap gate), `gate.sh` (master gate).
- **[examples/vire/](examples/vire/)** — compiling examples (bounded subset).

## License

Everything in this repository is under the **GNU GPL v3.0 or later**
([`LICENSE`](LICENSE)). See [LICENSING.md](LICENSING.md).

## Core language ideas (in brief)

- **Inference over annotation** — hardly any type written, every type known at
  compile time or a compile error.
- **No `null`, no exceptions** — `Option[T]`; errors are values (Go spirit) with
  `?` propagation; `Result[T,E]` fully typed.
- **`type`** for product and sum types, **traits** + monomorphized **generics**
  incl. value generics `[N]`.
- **`comptime`** — code running in the compiler: reflection, derivation,
  conditional compilation — zero-cost.
- **Invisible memory** — ownership/borrow/region decided by the solver; only the
  temporary heap ceiling (`region(max)`) is visible.
- **Concurrency safe by construction** — `spawn`/`Channel`/`Mutex`/`Atomic`;
  shared bare mutable state rejected.
- **`capsule` / `region`** — fault-containment arenas with ceilings, freed en bloc.
- **C native via headers** — `extern "C"` declarations with checked mapping;
  C++/Rust via the C ABI. Meson first-class.

The name and details are provisional and easy to change; the design is the core.
