# Design investigation: `@vulkan` — safe, easy, full-performance Vulkan in Vire

**Goal (the user's vision):** make Vulkan usable in Vire *as easily as OpenGL* —
you write a few lines to draw — while keeping **full Vulkan performance**,
**memory safety**, and **Vire's whole-program optimizations**. Not a thin FFI
binding, and not a compute-only offload: a **compiler-integrated, safe Vulkan
framework** (graphics + compute) where the notorious boilerplate (swapchain,
render passes, pipelines, descriptor sets, command buffers, synchronization,
memory) is generated — correctly — by the compiler and a thin runtime.

Think "a compiler-integrated `wgpu`/`sokol_gfx`", but with two things those can't
do: **whole-program pipeline/barrier baking** and **language-level safety**.

## Feasibility — everything needed is present (measured on this machine)

- **SPIR-V codegen**: LLVM 22 ships the `spirv64` target, and our `@gpu` device-IR
  emitter already produces LLVM IR — so Vire shaders compile to SPIR-V through the
  same path. (This is the compute foundation, already validated for `@gpu`.)
- **Vulkan stack**: `libvulkan.so.1` (1.4), `vulkan.h`, `glslc`/`spirv-as` present.
- **Windowing**: GLFW 3.4 and SDL2 (+headers) present; both Wayland (`wayland-0`)
  and X11 (`:1`) sessions available.
- **WSI**: both the Intel iGPU and the RTX 5070 expose
  `VK_KHR_{wayland,xcb,xlib}_surface` + `VK_KHR_swapchain`.
- **Vendor-neutral**: two Vulkan devices enumerate here (Intel + NVIDIA) — the same
  `@vulkan` program runs on both, and on AMD/Apple(MoltenVK)/Android elsewhere.

So the full graphics path is buildable; nothing external is missing.

## Architecture — three layers, most of the hard work at compile time

### 1. Shaders in Vire (single-source, no GLSL files)
`@vertex` / `@fragment` / `@compute` functions, compiled to SPIR-V via the emitter
(reusing the `@gpu` path). Vertex/fragment I/O is typed by Vire structs, so the
**vertex layout and descriptor bindings are known at compile time** from the shader
signatures — no runtime reflection.

```vire
@vertex fn vs(pos: Vec3, uv: Vec2) -> (clip: Vec4, out_uv: Vec2) { ... }
@fragment fn fs(uv: Vec2, tex: Sampler2D) -> Vec4 { ... }
```

### 2. Declarative resources + pipelines → baked by the compiler
Typed handles (`Buffer`, `Image`, `Texture`, `Mesh`, `Pipeline`, `Uniforms`).
Because Vire is **whole-program**, the compiler:
- derives **descriptor-set layouts** from shader resource usage (no hand-written
  `VkDescriptorSetLayout`);
- **bakes the pipeline state object** (`VkGraphicsPipelineCreateInfo`: vertex input
  from the `@vertex` input struct, blend/depth/topology from the declared config)
  at compile time — no runtime pipeline introspection or stalls;
- builds a **render graph** from the per-frame draw/dispatch sequence and inserts
  **minimal, correct image-layout transitions + barriers automatically** — the
  single most error-prone part of hand-written Vulkan, done statically.

### 3. A thin safe runtime over libvulkan (the OpenGL-like surface)
Auto instance/device/queue selection, swapchain (re)creation, frames-in-flight
sync, and a sub-allocating memory allocator — behind an immediate, OpenGL-simple
API. The target ergonomics:

```vire
fn main() {
    let win  = window(1280, 720, "hello vire")
    let pipe = pipeline(vs, fs)              // baked at compile time
    let mesh = mesh(vertices, indices)
    let tex  = texture("wood.png")
    while !win.closing() {
        frame(win) {                          // records the command buffer + barriers
            clear(0.1, 0.1, 0.12)
            draw(pipe, mesh, uniforms(mvp, tex))
        }                                     // present, advance frame-in-flight
    }
}
```

That is OpenGL-level line count for a textured, depth-tested draw — but every
Vulkan object under it is explicit, baked, and correct.

## What makes it *Vire* (why not just bind an existing framework)

- **Compile-time pipeline/layout baking.** A whole-program compiler sees every
  shader, resource, and draw call, so pipeline objects and descriptor layouts are
  constants in the binary — no first-use hitches, no runtime reflection. wgpu/sokol
  build these at runtime.
- **Static render-graph → minimal barriers.** The compiler computes resource
  read/write per pass and emits the tightest correct barrier set — hand-expert
  quality, without the hand-expert bug surface.
- **Language-level memory safety.** Handles are Vire objects with RC/region
  lifetimes → no use-after-free of GPU resources, teardown ordered (device-idle
  before destroy). Buffer writes go through typed, bounds-checked mapped regions;
  no raw pointers in the safe surface.
- **Zero-cost validation.** Usage rules (create-usage matches use, bind-before-draw,
  shader I/O matches pipeline) checked by the solver where provable; Vulkan
  validation layers on under `--debug`, compiled out in release (same pattern as
  the logger / `--backtrace`) → safety in dev, full speed in ship.
- **Whole-program optimization of the frame.** const-fold pipeline config,
  monomorphize shader variants per material, dead-resource elimination, and
  `@gpu`-style specialization all apply to the render path.
- **Single-source.** Shaders, resources, and app logic in one language/IR — no
  GLSL/SPIR-V toolchain juggling.
- **No ceiling.** Raw `Vk*` handles remain reachable through verified `native "c"`
  blocks for anything the safe layer doesn't cover yet — advanced use never blocks.

## Memory-safety model

| hazard (raw Vulkan) | how `@vulkan` prevents it |
|---|---|
| use-after-free of a buffer/image/pipeline | handles are RC/region-owned; destroy ordered after device-idle |
| missing/incorrect image-layout transition | render graph inserts them; not user-writable |
| descriptor/layout mismatch vs shader | layouts derived from typed shader signatures at compile time |
| out-of-bounds buffer write | typed, bounds-checked mapped write regions |
| use-before-bind, wrong usage flags | solver checks statically; validation layers in `--debug` |
| forgotten sync (races/tearing) | frames-in-flight + semaphores/fences owned by the runtime |

## Honest scope

This is **large — multi-quarter**, not a two-month item. A safe windowed triangle
already needs windowing + instance/device/swapchain + one baked pipeline + command
buffer + sync + present. The full framework (textures, uniforms/descriptors, depth,
render-graph, multi-pass, compute integration) is a real project. But it is
*de-risked* (all deps present) and *incremental* (each stage is runnable), and it
reuses the `@gpu` SPIR-V path and Vire's ownership/whole-program machinery rather
than starting from zero.

## Staged roadmap

- **V1 — safe compute (foundation).** `@compute` → SPIR-V → dispatch over a minimal
  safe Vulkan runtime; the `jrt_gpu_*` ABI + read-only analysis carry over. Delivers
  vendor-neutral compute (Intel + NVIDIA here) with no windowing. *Smallest real
  step; validates the SPIR-V emitter + runtime.*
- **V2 — hello triangle.** *Foundation shipped:* a Vire program renders a real,
  self-verifying Vulkan triangle **headless** (`vk_triangle()` → instance/device/
  render-pass/baked-pipeline/draw/readback, pixel-verified; `crates/driver/src/
  vk_runtime.c`, `tests/vire_vulkan.sh`, runs on the Intel iGPU here). *Remaining:*
  windowing (GLFW/SDL) + swapchain + present, and the `frame { clear; draw }`
  surface — then the same pipeline shows in a window. Shaders are bootstrap SPIR-V
  (glslc) until the emitter below lands.
- **V3 — resources.** Buffers/meshes, uniforms, textures/samplers, auto descriptor
  layouts from shader signatures; a `draw(pipe, mesh, uniforms)` API.
- **V4 — render graph.** Per-frame graph → automatic layout transitions + minimal
  barriers; depth buffer, multiple passes, MSAA, swapchain-resize handling.
- **V5 — Vire optimizations.** Compile-time pipeline/descriptor baking, shader
  monomorphization per material, whole-program resource-lifetime + dead-resource
  elimination, zero-cost validation gating.

## Recommendation

Yes — build it, staged, as a headline Vire capability: it turns Vire into a
language where you get **OpenGL-simple, memory-safe, full-speed Vulkan** with
optimizations no runtime framework can do. Start at **V1** (reuses the validated
compute path, no windowing, smallest surface) to stand up the SPIR-V emitter + safe
runtime, then **V2** for the triangle that proves the ergonomics. The compute
backend previously discussed is exactly V1 — the foundation of this larger vision,
not a separate track.
