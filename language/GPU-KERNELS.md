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
[`crates/cuda-oxide/NOTICE.md`](../crates/cuda-oxide/NOTICE.md).

## Example

A `@gpu` kernel reads just like a [`parallel_for`](../examples/vire/threads_parallel_for.vr)
worker: its **first parameter is the thread index**, supplied by the launcher —
you don't pass it. Callers pass only the parameters after it.

```vire
@gpu
fn saxpy(i: Int, n: Int, a: Int, x: array, y: array) {
    if i < n {                 // guard: you own the bounds on the device
        y[i] = a * x[i] + y[i]
    }
}

fn main() {
    mut x = array(1000)
    mut y = array(1000)
    mut i = 0
    while i < 1000 { x[i] = i  y[i] = 2 * i  i = i + 1 }
    saxpy(1000, 3, x, y)       // ← runs on the GPU; `i` is injected, n=1000
    print(y[7])                // 35
}
```

`saxpy(1000, 3, x, y)` supplies `n, a, x, y` (params 1..); the index `i` is
injected per thread. The first supplied argument (`n = 1000`) is the launch size.

Build/run needs an NVIDIA GPU, the CUDA toolkit (`libcuda`, headers under
`/opt/cuda/include`), and an LLVM with the NVPTX target (`llc`).

## Thread index & intrinsics

The kernel's **parameter 0 is the global 1-D thread index** (an `Int`,
`blockIdx.x*blockDim.x + threadIdx.x`), injected by the launcher — the usual way
to get your index. For advanced kernels (grid-stride loops, 1-D block/grid
queries) these nullary intrinsics are also available inside a `@gpu` function
(they lower to `@llvm.nvvm.read.ptx.sreg.*`):

| intrinsic     | meaning                                            |
|---------------|----------------------------------------------------|
| `gpu_gid()`   | same as parameter 0: global 1-D index              |
| `gpu_gsize()` | total thread count = `gridDim*blockDim` (grid-stride loops) |
| `gpu_tid()`   | `threadIdx.x`                                      |
| `gpu_bid()`   | `blockIdx.x`                                       |
| `gpu_bdim()`  | `blockDim.x`                                       |
| `gpu_gdim()`  | `gridDim.x`                                        |

Grid-stride example (`gpu_gsize()` still works with the injected index):

```vire
@gpu
fn fill(i: Int, n: Int, out: array) {
    mut j = i
    while j < n { out[j] = j  j = j + gpu_gsize() }
}
```

## Calling convention & launch

- Parameter 0 (the thread index) is injected; callers pass params 1.. , exactly
  like a `parallel_for` worker `(i, …)`. A kernel needs `(index: Int, count:
  Int, …)`: the **first caller-supplied `Int` is the launch size N**, and the
  runtime launches N threads (`block = 256`, `grid = ceil(N/256)`). Your kernel
  guards `if i < n`.
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
