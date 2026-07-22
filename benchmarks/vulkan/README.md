# Vire `@vulkan` vs hand-written Vulkan (C++ / Rust)

**Question:** does Vire's compiler-integrated Vulkan cost anything at runtime versus
writing Vulkan by hand?

**Answer:** no. Vire's `@vulkan` lowers to **direct `libvulkan` calls** in the
generated C runtime — the same calls a hand-written program makes. The GPU work is
byte-identical (same triangle, same driver), so the per-frame time matches within
noise. The difference is the **source size**: the Vire program is a fraction of the
hand-written baselines because the runtime, pipeline, render pass, and synchronization
are generated.

## What it measures

All three programs run the **same workload**: initialise Vulkan once, then render a
mesh-shader triangle (`VK_EXT_mesh_shader`) to a 256×256 headless image `N` times —
one `vkQueueSubmit` + fence wait per frame — and report the **per-frame nanoseconds**.
This is steady-state (no re-init in the loop), so it isolates the CPU-side submission
cost. The three baselines load the **same SPIR-V** (`glslc`-compiled) so the GPU side
is identical.

- **Vire** — `bench.vr` calls `vk_bench(frames)` (runtime: `crates/driver/src/vk_runtime.c`).
- **C++** — `bench.cpp`, `vulkan.h`, linked against `-lvulkan`.
- **Rust** — `rust-ash/`, the [`ash`](https://crates.io/crates/ash) bindings.

## Run it

```sh
sh benchmarks/vulkan/run.sh
```

Needs a Vulkan device with `VK_EXT_mesh_shader` (both an Intel iGPU and an RTX 5070
qualify here), plus `glslc`, `g++`, and `cargo` (the `ash` crate, offline). Skips
cleanly if the device or tools are absent.

## Result (this machine: RTX 5070 Laptop, 5000 frames/run, median of 5)

| Baseline | Per-frame | Source (non-comment lines) |
|----------|-----------|----------------------------|
| **Vire `@vulkan`** | **≈ 21.1 µs** | **9** |
| C++ (`vulkan.h`)   | ≈ 21.5 µs | 85 |
| Rust (`ash`)       | ≈ 20.5 µs | 132 |

The per-frame times are equal within run-to-run noise (~±5%): **Vire adds no runtime
overhead** — it executes the same driver calls. The per-frame figure is dominated by
submit + GPU dispatch + fence-wait latency for a trivial triangle, which is what a
CPU-overhead comparison should stress.

The headline is the **9 vs 85 vs 132 lines**: the same GPU-driven capability, at the
same speed, from a fraction of the code — the boilerplate (device selection, pipeline,
render pass, command recording, synchronization) is compiler-generated. For the full
GPU-driven meshlet renderer (build → cull → draw → shade), that ratio only widens —
see [`../../language/GPU-VULKAN.md`](../../language/GPU-VULKAN.md) and the
`examples/vire/vulkan_*.vr` programs.

Numbers are machine-specific; re-run `run.sh` for your hardware.
