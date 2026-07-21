# `@gpu` — single-source GPU kernels

Vire can run data-parallel functions on an NVIDIA GPU. You mark a function
`@gpu`, write it in ordinary Vire, and call it like any other function — the
compiler lowers it to PTX, embeds it in the binary, and launches it on the GPU
at runtime via the CUDA Driver API. No separate `.cu` file, no host/device
glue.

The design is adapted from [NVlabs/cuda-oxide](https://github.com/NVlabs/cuda-oxide)
(Apache-2.0; single-source Rust→PTX). Vire already owns the pipeline it needs
(Vire → shared IR → LLVM IR → codegen), so `@gpu` reuses it: a kernel becomes an
`nvptx64` LLVM module compiled with `llc -march=nvptx64`. See
[`third_party/cuda-oxide/NOTICE.md`](../third_party/cuda-oxide/NOTICE.md).

## Example

```vire
@gpu
fn saxpy(n: Int, a: Int, x: array, y: array) {
    mut i = gpu_gid()          // global thread index
    if i < n {                 // guard: you own the bounds on the device
        y[i] = a * x[i] + y[i]
    }
}

fn main() {
    mut x = array(1000)
    mut y = array(1000)
    mut i = 0
    while i < 1000 { x[i] = i  y[i] = 2 * i  i = i + 1 }
    saxpy(1000, 3, x, y)       // ← runs on the GPU
    print(y[7])                // 35
}
```

Build/run needs an NVIDIA GPU, the CUDA toolkit (`libcuda`, headers under
`/opt/cuda/include`), and an LLVM with the NVPTX target (`llc`).

## Thread-index intrinsics

Nullary, return `Int`, valid only inside a `@gpu` function (they lower to
`@llvm.nvvm.read.ptx.sreg.*`):

| intrinsic     | meaning                                            |
|---------------|----------------------------------------------------|
| `gpu_gid()`   | global 1-D thread index = `blockIdx*blockDim + threadIdx` |
| `gpu_gsize()` | total thread count = `gridDim*blockDim` (grid-stride loops) |
| `gpu_tid()`   | `threadIdx.x`                                      |
| `gpu_bid()`   | `blockIdx.x`                                       |
| `gpu_bdim()`  | `blockDim.x`                                       |
| `gpu_gdim()`  | `gridDim.x`                                        |

## Calling convention & launch

- The kernel is called with ordinary Vire syntax. The **first scalar-integer
  parameter is the launch size N**: the runtime launches N threads
  (`block = 256`, `grid = ceil(N/256)`). Your kernel guards `if gpu_gid() < N`.
- **Array parameters** (`array` = `Int`/i64, `farray` = `Float`/f64,
  `Array<T>`) are uploaded to device memory before the launch and copied back
  after (v1 treats every array as in/out). Scalars are passed by value.
- The device context + module are created lazily on the first launch and cached.

## What a kernel may contain

A kernel is a **restricted device subset** — the backend rejects anything else
with a clear error rather than miscompiling:

- integer/float arithmetic, comparisons, bitwise/shift ops
- `if`/`else`, `while`/`for`, `match` (branches, switches)
- array indexing `x[i]` / `x[i] = v` (raw, **unchecked** — like CUDA; the
  `if i < n` guard is yours), the `gpu_*` intrinsics
- kernels return `()` (no return value)

Not allowed on the device (→ compile error): `print`/other host runtime calls,
object allocation/fields, strings, growable `list`/`map`/`set`, calls to other
functions. Kernels are top-level `fn`s (not methods).

## Soundness & the benchmark oracle

GPU floating point is **not** bit-identical to the CPU (different FMA
contraction, rounding, and reduction order). So `@gpu` lives on a **separate GPU
track**: the existing CPU benchmark suite stays bit-identical and is not moved to
the GPU. Integer kernels *are* bit-exact GPU-vs-CPU (see `tests/vire_gpu.sh`,
which pins the integer saxpy against the CPU result). Device array access is
unchecked, so the GPU track is explicitly outside the memory-safe-vs-Rust CPU
oracle; the fair GPU comparison is Vire-GPU vs a Rust-GPU baseline
(cuda-oxide), not vs CPU code.

## Implementation

| piece | file |
|---|---|
| `@gpu` parse + `gpu_*` intrinsics + kernel collection | `crates/vire/src/{parser,lower}.rs` |
| `GpuKernel` (kept out of `Program::functions`) | `crates/ir/src/lib.rs` |
| NVPTX device-IR emitter + C launch-stub generator | `crates/backend/src/nvptx.rs` |
| `jrt_gpu_*` Driver-API runtime | `crates/driver/src/gpu_runtime.c` |
| build wiring (`llc` → PTX → embed → link `-lcuda`) | `crates/vire/src/main.rs` |

A kernel is deliberately **not** placed in `Program::functions`, so no host
solver pass, RTA, or inliner ever touches it — the device code carries no RC, no
arena, and no bounds checks. The generated C launch stub takes the kernel's
mangled symbol name, so a host `call @<name>` links straight to it.
