# Investigation: a Vulkan/SPIR-V backend for `@gpu`

**Question:** would integrating a simple-but-powerful Vulkan compute path directly
into the Vire compiler make sense — as a second `@gpu` target beside CUDA/PTX?

**Verdict: yes, high value and technically de-risked.** It is the cleanest way to
make `@gpu` **vendor-neutral** (NVIDIA + AMD + Intel + Apple), which is exactly the
kind of thing cuda-oxide (a NVIDIA-only `rustc` backend) structurally cannot do —
so it directly serves the "beat cuda-oxide" goal. Below: the feasibility evidence,
the design, the honest trade-offs, and the recommendation.

## Why it's de-risked (measured on this machine)

- **LLVM already has the SPIR-V target built in.** `llc --version` lists
  `spirv` / `spirv32` / `spirv64` (LLVM 22.1.8). Our NVPTX emitter already produces
  LLVM IR *text*; the same IR can go to `llc -march=spirv64` instead of
  `-march=nvptx64`. The device-IR structure is identical — the backend is ~90%
  reusable.
- **The Vulkan stack is present and universal.** `libvulkan.so.1` (loader),
  `vulkan.h`, and `glslc`/`spirv-as`/`glslangValidator` are installed.
- **Both GPUs enumerate under Vulkan** on this laptop: the **Intel iGPU** *and* the
  **RTX 5070** (`vulkaninfo`). CUDA sees only the NVIDIA card; Vulkan runs on both —
  a concrete demonstration of the portability win on the very same hardware.

## The design (reuse the emitter, swap the dialect + runtime)

`@gpu fn` → shared IR → **SPIR-V dialect** of the device emitter → `llc
-march=spirv64` → SPIR-V module → embedded in the binary → dispatched via a minimal
Vulkan compute runtime. Concretely, three things change vs the PTX path; everything
else (the whole kernel-body IR walk) is shared:

| concern | CUDA/PTX (today) | Vulkan/SPIR-V |
|---|---|---|
| array pointers | `ptr addrspace(1)` (global) | `StorageBuffer` storage class (structured buffer) |
| thread index | `@llvm.nvvm.read.ptx.sreg.*` | SPIR-V builtins `GlobalInvocationId` etc. |
| barrier / warp / atomics | `nvvm.barrier0`, `shfl.sync`, `atomicrmw` | `OpControlBarrier`, `VK_KHR_shader_subgroup` ops, SPIR-V atomics |
| shared memory (future) | `.shared` / `addrspace(3)` | `Workgroup` storage class |
| host runtime | `jrt_gpu_*` over libcuda (84 lines) | `jrt_gpu_*` over libvulkan (~250–400 lines, one-time boilerplate) |

The **G1 intrinsics just landed map directly**: `gpu_sync` → `OpControlBarrier`,
`gpu_warp_reduce_add`/`gpu_shfl_down` → subgroup ops (`OpGroupNonUniform*`),
`gpu_atomic_add` → `OpAtomicIAdd`, IEEE math → SPIR-V `ExtInst` (`GLSL.std.450`).
So the primitive surface is already the right shape for both targets.

The host ABI (`jrt_gpu_ensure/upload/download/free/launch`) stays identical, so the
generated launch stubs and the read-only-array D2H-skip analysis are **backend-
agnostic** — only the runtime `.c` behind the ABI differs. Selection: `--gpu=cuda|
vulkan` (auto-default to CUDA if libcuda present, else Vulkan).

## Honest trade-offs

**Wins (Vulkan):**
- **Vendor-neutral**: NVIDIA + AMD + Intel + Apple (via MoltenVK) + Android. One
  binary, any GPU. cuda-oxide cannot match this.
- **No CUDA toolkit / libcuda dependency** — the Vulkan loader is more universally
  present.
- Subgroup, shared-memory, and atomic primitives all exist in core Vulkan / KHR
  extensions, so G1/G2 features port.

**Costs / risks (Vulkan):**
- **More host boilerplate**: instance → physical device → queue → descriptor set
  layout → pipeline → command buffer → explicit memory barriers → dispatch. It's
  templatable (write once), but it's ~3–5× the CUDA runtime's line count.
- **SPIR-V "Logical" addressing is restrictive**: no arbitrary pointer arithmetic;
  buffer access must be structured (index into a typed buffer). Our array GEPs
  already *are* structured indexing, so this mostly fits — but pointer-heavy kernels
  would need care. LLVM's SPIR-V backend is also newer/less battle-tested than
  NVPTX; expect some IR constructs to need adjustment.
- **Lower ceiling on cutting-edge NVIDIA features**: Vulkan has
  `VK_KHR_cooperative_matrix` but it is less capable than CUDA's `wgmma`/`tcgen05`.
  So the G3 tensor-core peak stays a CUDA-only story; Vulkan targets the broad 80%.
- **Perf**: for standard compute, Vulkan ≈ CUDA on the same hardware (same SPIR-V →
  native JIT). NVIDIA's CUDA path is sometimes marginally better on NV silicon;
  Vulkan is the *only* option on non-NV.

## Recommendation

Build it as a **second target, not a replacement** — the CUDA path stays for peak
NVIDIA performance (and the G3 tensor-core roadmap), and Vulkan adds portability.
"Simple but powerful" = reuse the emitter (dialect flag), reuse the `jrt_gpu_*` ABI
+ launch stubs + read-only analysis, and add one templated Vulkan compute runtime.
Estimated effort: a few weeks for a working saxpy/reduction on both Intel + NVIDIA
via the same `@gpu` source, because the hard parts (IR emission, the intrinsic
surface, the host ABI) already exist. Staged plan and risks tracked in
[../TODO.md](../TODO.md) (GPU section, "Vulkan/SPIR-V vendor-neutral backend").
