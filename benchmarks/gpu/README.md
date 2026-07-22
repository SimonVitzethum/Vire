# GPU track — `@gpu` kernels

This is a **separate benchmark track** from the CPU suite. GPU floating point is
not bit-identical to the CPU (different FMA/rounding/reduction order), so the
main suite stays on the CPU and bit-identical; GPU kernels are measured here on
their own. See [`../../language/GPU-KERNELS.md`](../../language/GPU-KERNELS.md).

## `heavy` — arithmetic-intensity sweep (integer, bit-exact)

Per array element, run *K* LCG steps (`x = (x*1103515245 + 12345) & 0x7fffffff`),
`N = 2_000_000` elements. Integer math → the GPU result is **bit-identical** to
the CPU (`heavy_gpu.vr` and `heavy_cpu.vr` print the same checksum at every *K*).

Measured on an **NVIDIA GeForce RTX 5070 Laptop GPU** (CUDA 13.3, wall-clock incl.
H2D/D2H transfer and the host-side reduction; GPU warmed once for context/JIT):

| inner steps K | GPU | CPU (1 core, AVX-vectorized) | speedup |
|--------------:|-----:|-----:|-----:|
| 2 000   | 0.235 s | 0.051 s | 0.22× (overhead-bound) |
| 20 000  | 0.239 s | 0.344 s | 1.44× |
| 100 000 | 0.290 s | 1.74 s  | 6.0× |
| 400 000 | 0.428 s | 6.98 s  | **16.3×** |

The GPU pays a fixed ~0.2 s overhead (context creation + PTX JIT + a 16 MB
round-trip), after which compute is nearly flat while the CPU grows linearly.
So the GPU wins once arithmetic intensity is high enough to amortize the
transfer — and loses at low intensity, which the table shows honestly rather
than cherry-picking. The CPU baseline is genuinely fast here (LLVM vectorizes
the independent per-element LCG chains 8-wide); its time scales with K, so it is
doing the work, not eliding it.

## Running

```sh
cargo build --release -p vire
sh benchmarks/gpu/run.sh            # sweep K, print GPU vs CPU + checksum match
```

Requires an NVIDIA GPU, the CUDA toolkit (`libcuda`, `/opt/cuda/include`), and an
LLVM with the NVPTX target (`llc`). `run.sh` skips cleanly if none is present.

## Fairness note

Vire-GPU vs Vire-CPU shows the offload speedup, but the *fair* GPU comparison is
Vire-GPU vs a **Rust-GPU** baseline (NVlabs/cuda-oxide — the design this feature
adapts). cuda-oxide is a custom `rustc` codegen backend requiring its own
toolchain (`rust-src`, a pinned nightly), so it is not built in this repo; the
Rust-GPU column is left open. Device array access is unchecked (CUDA-like), so
the GPU track is explicitly outside the memory-safe-vs-Rust CPU oracle.

See [`VS-CUDA-OXIDE.md`](VS-CUDA-OXIDE.md) for the architectural analysis of where
GPU performance is decided (both lower to PTX through the same LLVM NVPTX backend),
and the two improvements it motivated: an `opt -O3` device-module middle-end and a
read-only-array D2H-skip analysis — both adapted from cuda-oxide's design.
