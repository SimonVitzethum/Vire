# Vire `@gpu` vs cuda-oxide — performance analysis

**Status: no measured head-to-head exists.** cuda-oxide is a custom `rustc`
codegen backend that needs its own pinned nightly toolchain (`rust-toolchain.toml`
→ `nightly-2026-04-03` + `rustc-dev`/`rust-src`), not installed here, so a fair
Vire-GPU-vs-Rust-GPU benchmark is not built in this repo. The only hard data is
Vire-GPU **vs CPU** (up to **16.3×** on an RTX 5070, [`README.md`](README.md)).
This file is the **architectural** analysis of where GPU performance is actually
decided, plus the two improvements it motivated (both shipped, see end).

## The frame: both lower to PTX through the *same* LLVM NVPTX backend

Vire (`Vire → IR → LLVM IR → llc -march=nvptx64 → PTX`) and cuda-oxide
(`Rust → MIR → LLVM IR → PTX`) share the **same** LLVM NVPTX code generator. So
the *frontend language* does not determine GPU throughput. For a simple,
compute-bound kernel the two converge to the same PTX quality. The real
differences live in **(a)** the IR fed to NVPTX and **(b)** the launch/memory
model — not in Vire-vs-Rust.

## Where Vire could lose (ranked by leverage), and the status

### (a) Middle-end before PTX — **FIXED**
The NVPTX emitter ([`crates/backend/src/nvptx.rs`](../../crates/backend/src/nvptx.rs))
deliberately produces naive **alloca-per-local** IR (no phis). `llc` runs codegen
passes but **not** the target-independent middle-end (mem2reg/SROA/LICM/inline/
unroll/vectorize) — so loop-carried scalars could spill to slow `.local` device
memory. cuda-oxide gets that middle-end for free from rustc's LLVM pipeline.
**Shipped:** the build now runs `opt -O3` on the device module before `llc`
([`crates/vire/src/main.rs`](../../crates/vire/src/main.rs), the `want_gpu` branch).
Measured on the saxpy kernel: **13 device allocas → 0** after `opt` (all promoted
to PTX registers). Best-effort: if `opt` is absent, the build falls back to the
raw module (llc-only), so it never regresses on a toolchain without it.

### (b) Memory / launch model — **partly fixed**
v1 uploaded *and* downloaded every array (in/out), synced every launch, kept no
persistent buffers, and had no async path. cuda-oxide has typed in/out
`DeviceBuffer`s + async.
**Shipped:** a **read-only array analysis** (`read_only_params` in `nvptx.rs`,
adapted from cuda-oxide's typed in/out buffer distinction — idea, not code) proves
which array parameters the kernel never stores into and **skips their D2H
copyback**. Sound-conservative: an array pointer that cannot be traced back to a
parameter forces every array to in/out, so a needed copyback is never dropped.
Verified: saxpy's `x` skips D2H, `y` is still downloaded, result bit-exact vs CPU.
**Still open:** write-only H2D elision (upload nothing for output-only buffers),
persistent device buffers across launches, and an async (non-syncing) launch path.

### (c) Expressiveness ceiling — **open (feature gap, not codegen)**
Vire emits only: global memory, fixed `block=256`, 1-D grid, **no** shared memory,
no warp/block barriers, no warp reductions, no tensor cores. cuda-oxide's examples
cover shared memory, barriers, `wgmma`/`tcgen05`, tiled GEMM, cluster, and TMA.
For those kernel *classes* (GEMM, reductions, stencils) cuda-oxide can be 10×+
because the fast algorithm *requires* shared memory / tensor cores that Vire cannot
express yet. This is an expressiveness gap, not a code-generation gap.

### (d) Launch-config tuning — **open**
Fixed `block=256, grid=ceil(N/256)` vs a tunable block/2-D/3-D grid + shared-memory
size → occupancy left on the table for some kernels. Tracked in TODO Tier 4.

## Honest expectation table

| Kernel class | Vire vs cuda-oxide |
|---|---|
| Simple, compute-bound, global-mem, 1-D (`heavy`, saxpy) | ~parity (same NVPTX backend; now with the same middle-end) |
| Register-pressure / nested loops / device inlining | **was** behind (no middle-end) → now closed by `opt -O3` |
| Repeated launches / memory-bound | improved (read-only skips D2H); still behind on async + persistent buffers |
| GEMM / reduction / stencil (shared-mem / tensor-core) | cuda-oxide clearly ahead — Vire cannot express the fast algorithm |

## Where Vire "wins" — integration, not raw throughput
Single-source in the same language/IR, no separate toolchain or pinned nightly,
and a **bit-exact CPU fallback** for integer kernels as a correctness oracle. An
ergonomics/soundness win, not a throughput claim.

## What a fair measurement would require
Build the cuda-oxide toolchain, run **identical** kernels on both, and compare
**kernel-compute time only** (exclude H2D/D2H, warm the context/JIT once) — else
you measure the transfer model, not codegen quality.
