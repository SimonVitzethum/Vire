# Vire — Roadmap (open work)

Only **open** and **partial** items. Completed work lives in [DONE.md](DONE.md).
Legend: `[ ]` open · `[~]` partial. Design basis: [language/](language/).

## Current state (2026-07)

The whole pipeline is functional and green (lexer → parser → macro/comptime →
inline → inference → SSA IR → whole-program solver → LLVM `-O2 -flto -march=native`).
Performance sits at **geomean ~1.00× Rust** across 12 Vire benchmarks — compute-bound
at parity or faster, virtual dispatch 2.4× faster than clang. What shipped is in
[DONE.md](DONE.md); the remaining headroom is captured in the Performance Push below.

Soundness floor (never waived): Java heap-balance oracle **65/65** +
`tests/vire_heap.sh` 0-live + all `tests/vire_*.sh` green after every change.

---

# ⚡ PERFORMANCE PUSH — TOP PRIORITY (2-month plan)

**Goal: maximum performance without losing memory safety.** Every item is gated by
the 65/65 heap oracle + 0-live. **Execution order: Tier 4 → Tier 1 → Tier 2 →
Tier 3.** (Tier 4 first per decision 2026-07-22.)

Baseline is already Rust-parity, so the achievable delta is: (1) capture the one
structural ~2× lever (auto-arena for alloc-bound graphs), (2) mop up the last few
>1.1× residuals to parity, (3) plant vectorization enablers — plus (Tier 4, first)
the GPU-track wins.

**Do NOT re-attempt (measured dead — see [DONE.md](DONE.md)):** RC-inline-as-IR
(costs `-flto` inlining of other hot helpers), per-access `noalias` for
latency-bound benches (graph/sort), node-pool/SoA rewrite (slower), hand
regalloc/scheduling tuning for raytracer (low ROI, no single pass).

## Tier 4 — GPU track (DO FIRST) — separate track, CPU suite untouched

- [x] **Device-module middle-end (`opt -O3` before `llc`)** — DONE. The NVPTX
  emitter produces naive alloca-per-local IR; `llc` alone skips the
  target-independent mid-end, so loop scalars could hit slow `.local` memory. The
  build now runs `opt -O3` on the device module first (saxpy: 13 allocas → 0,
  register-promoted). Best-effort fallback if `opt` absent. See
  [benchmarks/gpu/VS-CUDA-OXIDE.md](benchmarks/gpu/VS-CUDA-OXIDE.md).
- [x] **Read-only array analysis** — DONE. `read_only_params` proves which array
  params a kernel never stores into and skips their D2H copyback (sound: an
  untraceable base forces in/out). Verified bit-exact (saxpy `x` skips, `y`
  downloads). *Still open below: write-only H2D elision.*
- [ ] **Write-only H2D elision + persistent context / async** — skip the *upload*
  for output-only buffers; reuse device buffers across launches; a non-synchronous
  launch path (v1 syncs every launch). *Removes per-launch malloc/free + sync
  overhead across repeated kernels.*
- [ ] **Explicit launch config** — let a kernel/call choose block size / 2-D & 3-D
  grids + shared memory, instead of fixed `block=256, grid=ceil(N/256)`.
- [ ] **Sub-word + Ref arrays on device**, `Array<F32>` scalars, device-side math
  intrinsics (sqrt/exp via `@llvm.nvvm.*`).
- [ ] **Fair Rust-GPU baseline** — build cuda-oxide (needs its rustc backend
  toolchain) to fill the Vire-GPU vs Rust-GPU column in benchmarks/gpu.

## Tier 1 — the structural ~2× lever (highest ceiling)

- [ ] **Automatic interprocedural region/arena inference for short-lived heap
  graphs.** binary-trees is at 0.91× Rust; the `--no-rc` oracle is **0.46× Rust but
  leaks** — that gap is the allocator (per-node malloc/free cascade), not RC
  (move-on-last-use already zeroed construction retains). Capture it *soundly*:
  auto-fire the `capsule`-arena mechanism where escape analysis proves a whole
  subgraph dies at a known point (build→consume→drop), **without** the user writing
  `capsule`, and free the subgraph en bloc.
  - Extends a proven mechanism: thread-local `arena_top`, `while_arena_safe`
    interprocedural escape check, `tests/vire_interproc_arena.sh`, 0-live oracle all
    already exist — this generalizes the trigger from explicit `capsule` to inferred.
  - **Arena fixed costs — chunk recycling DONE** (`jrt_arena_pop`/`arena_alloc`):
    standard 64 KiB chunks are recycled through a capped per-thread free-list instead
    of `free()`d at each pop — removes the O(chunks) free burst (a latency spike) and
    the per-capsule chunk malloc. (Larger-chunk tuning still open.)
  - **Soundness-critical** (a wrong escape verdict = use-after-free): pin promote
    *and* decline in both directions with new cases in
    `tests/vire_interproc_arena.sh` before enabling by default.
  - *Effort ~4–6 wk. This is the one place with real structural headroom.*

## Tier 2 — targeted, safe, medium ROI

- [ ] **NBody SoA `noalias`/`restrict` on disjoint static arrays.** NBody is the
  single remaining >1.1× compute case (**1.16× Rust / 1.31× C++**): seven same-typed
  `double[]` globals LLVM can't prove disjoint → reloads. Unlike the ruled-out
  per-access case (latency-bound), these are *statically distinct allocations* →
  provably safe to mark. Target parity. (Note: inlining `advance` makes it *worse*,
  7.5× — do not.)
- [ ] **(M0.3-iv) Field-/interprocedural bounds elision** for `out[k]` (length of a
  field array). Extends the mature `crates/solver/src/bounds.rs`. **Soundness risk
  ~zero** — elision only removes a check when provably safe; a real OOB still throws.
  Closes residual toward ~1.1×.
- [ ] **PGO on graph (Dijkstra).** Infra (`--pgo-gen`/`--pgo-use`) is already built
  but never applied to the data-dependent heap-sift branches. **Zero correctness
  risk**, cheap experiment (regular branches saw ~0%; branchy pointer-chasing may
  differ). graph is 1.64× Rust / 55 vs 30 MB RAM — also find which arrays are fully
  touched (cache pressure).

## Tier 3 — enablers with broad latent effect

- [ ] **(M0.3-v) Overflow default + `+%` culture.** Checked-overflow currently blocks
  vectorization of integer reductions; an explicit wrapping `+%` operator lets hot
  loops vectorize — **opt-in, checked stays the default** (safe). Broad latent gain
  for integer-array code. (Pairs with the overflow-in-release item under
  Cross-cutting.)
- [ ] **Explicit SIMD intrinsic path** for reductions LLVM won't auto-vectorize
  (e.g. vectorized argmin — kmeans nearest-centroid is 0.55× Rust / **1.28× C++**;
  no compiler emits SIMD for the branchy argmin). Emit `@llvm.vector.reduce.*` /
  explicit `<N x i64>` ops, or a comptime SIMD library. Opens a general capability,
  not just one bench.

## Perf — residual / parked (low ROI, keep for context)

- [ ] **Codegen scheduling / register allocation** on the FP losers (raytracer 1.9×,
  regex/pquicksort/pipeline 1.1–1.25×). Verified *not* IR quality (same program
  through `opt -O2` matches clang) — it's the LLVM **backend** reacting to subtle IR
  structure (~2× the stack spills of clang on the raytracer inner loop). Deep-codegen
  tuning, not a single fixable pass. **Parked — low ROI vs the wins already banked.**
- [ ] **sort 1.05× / pquicksort 1.23×** residual — the explicit-stack structure (a
  recursive `Array`-param version measured *slower*). Marginal.
- [ ] **Expand the differential fuzzer** (`tests/fuzz_gen.py`) — floats
  (fp-contract-matched), nested control-flow, break/continue, strings. (Correctness
  insurance, not perf, but belongs with the perf work.)
- [ ] **Analysis caching / incremental compile** — compile time measured super-linear
  ~O(n^1.4); orthogonal to runtime perf but the main compile-*speed* lever left.
- [ ] **Runtime GC latency — incremental collector (REOPENED, unsound attempt
  reverted).** The synchronous Bacon–Rajan pass runs one big stop-the-world scan at
  the adaptive threshold — a latency spike ∝ accumulated garbage. A naive
  **tail-batched** incremental version (`jrt_collect_step`: process the last N
  candidate roots + their component, compact the buffer) was tried but **leaks**
  (`tests/run.sh listdrop`: a prepend-built list that buffers each displaced node,
  then dropped, left ~610 live — head-of-buffer live roots never reclaimed), so it
  was reverted. A correct incremental Bacon–Rajan needs a **resumable traversal +
  write barrier** (mutation during a spread-out collection must be intercepted) or a
  fundamentally different buffer-processing invariant — bigger + correctness-critical.
  Related: the release **free-cascade** on a large dead subgraph is still one burst
  (`jrt_release` drop loop) — a budgeted/deferred-free (LIFO queue, drain a bounded
  amount per op + pump on alloc) is more tractable and can be attempted independently.
  Also open: chunk-recycle bound tuning, larger arena chunks.

---

## Compile-time programming layer (macros + comptime + reflection, one typed AST)

**Framing:** a **compile-time programming layer**, not text substitution. Macros,
`comptime`, and reflection all operate on the same typed AST / type graph, run
*after* parse+inference, re-checked after expansion.

- [~] **Phase 2 — move passes after inference.** comptime folding now lives in a
  post-inference pass ([comptime.rs](crates/vire/src/comptime.rs) `eval_comptime`):
  collects module `const`s, inlines refs to literals (respecting shadowing), folds
  `comptime`/`comptime if`. `const` now works (value/comptime/array size).
  `tests/vire_comptime.sh`. **Still open:** move **macro expansion** after inference
  too (still runs before — the untyped anti-pattern), and have the pass consult the
  type graph (type-aware `comptime if`).
- [ ] **Phase 3+ — features on the foundation** (sequence below).
- [~] **(b) typed reflection over the type graph** — `@derive(Eq, Show, Ord, Hash, Json)`
  works for product AND sum types ([derive.rs](crates/vire/src/derive.rs)).
  `tests/vire_derive.sh`. **Open:** generic types (needs generic-method
  monomorphization in lower.rs), nested-user-type fields (recursive derive), JSON
  string escaping, and `@typeinfo(T)` as a comptime-iterable typed value (needs
  aggregate comptime values — the interpreter is scalar-only today).
- [~] **(c) hygienic item macros** — `macro name(P: type, n: ident, e: expr){ <items> }`
  → declarations ([itemmacro.rs](crates/vire/src/itemmacro.rs)); AST-level,
  kind-checked, hygienic, type-checked after expansion; nested invocations expand to
  a fixpoint; generic type args work. `tests/vire_itemmacro.sh`. **Open:** token
  **pasting** (identifier interpolation), multi-argument generics (`Map[K, V]`),
  `block`/`pat` parameter kinds.
- [ ] **`comptime for`** (loop unrolling to runtime statements) / **`emit`** surface
  syntax. Also open: comptime over reference/aggregate values (scalars only today),
  `return`/`break` in a comptime body.

## Front-end completeness

- [ ] **`vire fmt`** (roundtrip AST→source) as parser-fuzz insurance.
- [~] **Error messages** — panic-mode recovery collects multiple diagnostics; still
  open: fix suggestions and pointing near the true cause.
- [~] **Trait resolution + coherence.** Duplicate/overlapping method defs per type
  rejected; bounded generics `[T: Trait]` resolve + enforced. Open: overlapping
  **generic** impls, coherence across impls.
- [~] **Monomorphization** — works via the inliner/`instantiate`; full value-generic
  monomorphization (distinct instances per N) partly open (inference of a type arg
  that appears only in return position defaults to `Int`).
- [~] **Iterator-mutation check** ([REFERENCE.md](language/REFERENCE.md) §9a) — local
  non-mutation analysis; not provable → compile error.

## Stdlib + FFI

- [~] **Collections breadth.** `list()`/`map()`/`set()`, `Str` methods, and iterator
  adapters (`fold`/`sum`/`count`/`map`/`filter`/`each`, statement-bodied lambdas)
  work. `tests/vire_iter.sh`. **Open:** `Str.split` (needs a typed `list[Str]`), and
  the full `Option`/`Result` surface (`.wrap(msg)` context/chain — core `?`/`match`
  works).

---

## Features 1–8 (open parts only)

### [1] Multithreading, safe by construction
- [ ] `Mutex.lock(closure)` (scoped-guard form); `parallel_map`; typed `Channel[T]`
  for ref payloads (currently Int values).
- [ ] (M0.1c) measure real multithread atomic contention.

### [2] Template programming
- [ ] Fixed arrays `[T; N]` as a distinct inline-storage value type (value-generic
  `array(N)` already gives constant-size stack arrays).
- [ ] Overlapping/coherence checking for generic impls; inference of a type arg that
  appears only in return position (defaults to `Int` today).

### [3] Compile-time reflection
- [ ] `@typeinfo(T)` (fields/variants/methods/attributes, comptime-iterable).
- [ ] `@derive` via reflection (generic + nested-user-type — see (b) above).
- [ ] `comptime for`, `emit`. **No** runtime reflection (AOT).

### [4] Own optional preprocessor *(= comptime/@if/macros)*
- [ ] Hygienic macros: typed parameters `block`/`pat`, token pasting, diagnostic
  spans into the expansion (typed `expr`/`ident`/`type` + hygiene already done).

### [6] Logger — remaining
- [ ] `with log.span(...)` (scoped context fields).
- [ ] Sinks (colored console / JSON / file), chosen at build time.

### [7] Go-style error handling — remaining
- [ ] `.wrap(msg)` (context, chain), typed errors with attached debug path.

### [8] Debug symbols + crash paths — remaining
- [ ] freestanding: compact symbol table instead of libc `backtrace`; map the entry
  symbol `java_main` back to `main` in the DISubprogram name (cosmetic).

---

## GPU `@gpu` — reach and beat cuda-oxide

*(Near-term GPU perf items live in the Performance Push, Tier 4 above. This is the
full roadmap to match cuda-oxide's feature scope + performance and then exceed it.)*

**Framing (see [benchmarks/gpu/VS-CUDA-OXIDE.md](benchmarks/gpu/VS-CUDA-OXIDE.md)):**
both lower to PTX through the *same* LLVM NVPTX backend, so raw codegen is already
at parity for simple kernels (after the `opt -O3` mid-end that just landed). The
gap is four things: **(1)** device-programming *primitives* Vire can't express,
**(2)** the *high-perf kernel classes* (tensor cores, TMA) that need those
primitives, **(3)** perf *infrastructure* (async/streams/persistent buffers/
autotuning), and **(4)** Vire-only *beat levers* (memory safety, whole-program
specialization, single-source oracle). Honest scope: full tensor-core/TMA parity
is NVIDIA-research-grade (multi-quarter), so sequence primitives first.

### Stage G1 — device-programming primitives (reach parity on the common 80%)
- [x] **Block barrier** (`gpu_sync()` → `@llvm.nvvm.barrier0`) — DONE.
- [x] **Warp intrinsics** — DONE: `gpu_shfl_down` (`shfl.sync.down.i32`) and
  `gpu_warp_reduce_add` (5× shuffle+add full-warp sum). Enables the fast-reduction
  idiom (warp-reduce → atomic) with no shared memory. *Vote/ballot/scan still open.*
- [x] **Device atomics** — DONE: `gpu_atomic_add(arr, idx, v)` → `atomicrmw add`
  (global, Int/Long), returns the old value. Read-only analysis made sound (an array
  passed to any device call counts as written). *CAS/other ops still open.*
- [x] **IEEE device math** — DONE: `gpu_sqrt/fabs/floor/ceil/fmin/fmax` via
  `@llvm.*.f64` (round-to-nearest → bit-exact vs CPU). *Transcendentals below.*
- [ ] **Transcendental math** (sin/cos/exp/log/tan/pow) — needs libdevice
  (`__nv_*`) bitcode linked into the device module (not plain LLVM intrinsics).
- [ ] **Shared memory** (`@shared` arrays, `Workgroup`/`addrspace(3)`) — new syntax
  + IR; unlocks block-level (not just warp-level) reductions and tiling.
- [ ] **Vote/ballot + warp scan**; atomic **CAS**/min/max/exchange.
- [ ] **Tunable launch config**: explicit block size, 2-D/3-D grids, dynamic
  shared-memory size (replaces the fixed `block=256, grid=ceil(N/256)`).
- [ ] **Device `printf`** (debugging).
- [ ] **Device-side helper fns** with inlining (ensure non-kernel device fns emit;
  `opt` inlines them).

### Stage G2 — perf infrastructure (close the transfer/launch overhead)
- [ ] **Write-only H2D elision** — skip the *upload* for output-only buffers
  (complements the read-only D2H skip just shipped).
- [ ] **Persistent device buffers** across launches (no malloc/free per call).
- [ ] **Async launches + CUDA streams**; overlap H2D / compute / D2H.
- [ ] **Pinned (page-locked) host memory** for faster transfers.
- [ ] **Per-arch codegen** (`-mcpu=sm_90`/`sm_100`) + **cubin caching**, not only
  forward-JIT PTX (saves the ~0.2 s JIT on every run).
- [ ] **Occupancy-aware launch autotuning** (`cuOccupancyMaxPotentialBlockSize`).

### Stage G3 — high-performance kernel classes (where cuda-oxide gets 10×+)
- [ ] **`cp.async` / TMA** async global→shared copies (Hopper/Blackwell).
- [ ] **Tensor-core MMA**: `mma.sync` / `wgmma` / `tcgen05` intrinsics.
- [ ] **Cooperative groups / thread-block clusters**.
- [ ] **Tiled-GEMM building block** in-language (comptime-generated) as the
  reference win. *Scope: NVIDIA-research-grade; do G1/G2 first.*

### Stage G4 — the BEAT levers (Vire-only — exceed, don't just match)
- [ ] **Memory-safe device mode.** cuda-oxide device access is unchecked
  (CUDA-like). Vire's solver can prove many device indices in-range (reuse
  `bounds.rs` relational elision) and bounds-check the rest → an *optional safe GPU
  mode* (off by default for parity, on for safety). No CUDA/C++/cuda-oxide analogue.
- [ ] **Whole-program kernel specialization.** const-prop launch bounds +
  monomorphize kernels per call-site (value generics exist) → constant loop trips,
  `__launch_bounds__`, device dead-arg elimination. A single-source whole-program
  compiler can specialize kernels a library-based flow cannot.
- [ ] **Single-source CPU+GPU + bit-exact oracle (already unique).** Extend:
  automatic CPU fallback when no GPU present; **differential CPU-vs-GPU fuzzing** of
  kernels (reuse `fuzz_gen.py`); float kernels with an fp-contract-matched oracle.
- [ ] **comptime kernel generation.** Generate specialized kernels (tile sizes,
  unroll factors) at compile time from the comptime layer — autotuning with no
  runtime JIT.

### Fair measurement (fill the Rust-GPU column)
- [ ] Build the cuda-oxide toolchain (pinned nightly) once; run **identical**
  kernels; compare **kernel-compute time only** (warm context, exclude H2D/D2H).
  Start with saxpy + a shared-mem reduction + a tiled GEMM. Per
  [benchmarks/gpu/VS-CUDA-OXIDE.md](benchmarks/gpu/VS-CUDA-OXIDE.md).

---

## `@vulkan` — safe, easy, full-performance Vulkan (graphics + compute)

**Investigated — high value, de-risked, multi-quarter.** Full design, safety
model, and target ergonomics in [language/GPU-VULKAN.md](language/GPU-VULKAN.md).
The vision: Vulkan **as easy as OpenGL** but with full performance, memory safety,
and Vire's whole-program optimizations — a *compiler-integrated safe Vulkan
framework* (not an FFI binding). What makes it Vire-only: **compile-time
pipeline/descriptor baking** (constants in the binary, no runtime reflection or
first-use hitches), a **static render graph → minimal correct barriers** (the
hardest hand-Vulkan part, done by the compiler), **language-level handle safety**
(RC/region lifetimes → no GPU-resource use-after-free), **zero-cost validation**
(layers under `--debug`, compiled out in release), and **single-source shaders**
(`@vertex`/`@fragment`/`@compute` → SPIR-V via the `@gpu` emitter). Escape hatch:
raw `Vk*` via verified `native "c"`. All deps present here (LLVM `spirv64`,
libvulkan, GLFW/SDL2, Wayland+X11, WSI on both Intel iGPU + RTX).

Staged (each stage runnable):
- [ ] **V1 — safe compute foundation.** `@compute` → SPIR-V → dispatch over a
  minimal safe Vulkan runtime; reuse the `jrt_gpu_*` ABI + read-only analysis. No
  windowing. Delivers vendor-neutral compute (runs on Intel + NVIDIA here). *Smallest
  real step; stands up the SPIR-V emitter + runtime.* (This subsumes the old
  "Vulkan compute backend" idea — it is the foundation, not a separate track.)
- [~] **V2 — hello triangle.** *Mostly DONE — visible in a window.* `vk_window(0)`
  opens a GLFW window + Vulkan swapchain and presents the triangle until closed
  (per-frame acquire/submit/present, FIFO); `vk_triangle()` keeps the headless
  pixel-verified CI path. One runtime shares `build_pipeline`/`build_rp`/`rec_draw`
  across both. Wayland extent-clamp handled. `examples/vire/vulkan_triangle.vr`,
  `crates/driver/src/vk_runtime.c`, `tests/vire_vulkan.sh`. Linked only when used
  (`want_vulkan` → libvulkan+glfw). *Remaining:* the declarative `frame { clear;
  draw }` surface + arbitrary geometry (today the triangle is fixed), and the
  single-source `@vertex`/`@fragment` → SPIR-V shaders (the SPIR-V-emitter item
  below; shaders are bootstrap glslc SPIR-V for now).
- [ ] **V3 — resources.** Buffers/meshes, uniforms, textures/samplers, descriptor
  layouts auto-derived from typed shader signatures; `draw(pipe, mesh, uniforms)`.
- [ ] **V4 — render graph.** Automatic image-layout transitions + minimal barriers;
  depth, multi-pass, MSAA, swapchain-resize.
- [~] **VS — Vire shaders (SPIR-V emitter).** *DECIDED: Vire is the shader language.*
  *Steps 1+2 SHIPPED:* Vire **owns SPIR-V generation** (`crates/backend/src/spirv.rs`
  emits assembly → `spirv-as` → generated `vk_shaders.c`, no glslc), and a real
  **shader compiler** (`crates/vire/src/shader.rs`) compiles an `@fragment fn`
  **body** to SPIR-V ops — float/vector arithmetic (`OpFAdd/Sub/Mul/Div`), `mut`
  bindings, `vecN(...)` constructors, and vector·scalar (`OpVectorTimesScalar`) —
  not just a constant. `@vertex`/`@fragment` parse as item attributes and are pulled
  out of host lowering + inference. Verified (`tests/vire_vulkan.sh`): a computed
  green fragment (`vec4(0.1,0.4,0.15,0.5) * 2.0`) renders green, headless + windowed.
  *Fragment inputs — `gl_FragCoord` DONE:* `frag_x()`/`frag_y()`/`frag_coord()`
  read the pixel position (`OpLoad` + `OpCompositeExtract`, BuiltIn FragCoord added
  to the interface only when used), so a fragment computes **per-pixel** — a
  gradient `vec4(frag_x()/256.0, …)` gives centroid r≈128 from position, verified
  (`tests/vire_vulkan.sh vire_fragment_fragcoord`; `examples/vire/vulkan_triangle.vr`
  shows a visible gradient). *`@vertex` DONE:* a Vire `@vertex fn vs(pos: Vec2) ->
  Vec4` receives the built-in triangle corner (indexed from a fixed array by
  `gl_VertexIndex`) and returns `gl_Position` — so it **transforms** the geometry
  (swizzles `.x/.y` + mixed `vecN` construction added). Verified: a shift `vec4(pos.x
  + 3.0, …)` moves the triangle off-screen (`tests/vire_vulkan.sh vire_vertex_shader`;
  both stages Vire-authored). *Remaining:* (a) real `Vec2/3/4`/`Mat4` in the host type
  system (today vectors are shader-local); (b) **structured control flow**
  (`OpLoopMerge`/`OpSelectionMerge`) + `GLSL.std.450` builtins (normalize/dot/mix…);
  (c) **varyings** (vertex→fragment interpolated outputs); (d) **vertex-buffer**
  geometry from Vire data (today the corners are the fixed built-in array) + typed
  stage I/O — the bridge to VM meshlets.
- [ ] **VM — GPU-driven meshlets (first-class).** On VS + V3. Both GPUs here support
  `VK_EXT_mesh_shader` (`meshShader`/`taskShader = true` on Intel iGPU + RTX). `@task`
  / `@mesh` stages (`TaskEXT`/`MeshEXT`, `SetMeshOutputsEXT`); a Vire `@compute`
  meshlet builder (partition + cone data); GPU frustum/backface/cone culling in
  `@task`; `vkCmdDrawMeshTasksIndirectCountEXT` + bindless GPU scene buffers (typed
  Vire structs). One Vire program = the whole GPU-driven renderer (builder + cull +
  draw shaders + scene data), which is normally GLSL/HLSL + C++ + a mesh toolchain.
- [ ] **`@gpu`-on-Vulkan compute path** (separate from graphics): the SPIR-V dialect
  of the device emitter via `llc -march=spirv64` (StorageBuffer/`Workgroup`, subgroup
  ops, `GLSL.std.450`); G1 intrinsics map directly (barrier→`OpControlBarrier`,
  warp→subgroup, atomic→`OpAtomicIAdd`). Compute-flavor SPIR-V, so `llc` suffices
  here (unlike the graphics stages above).
- [ ] **V5 — Vire optimizations.** Compile-time pipeline/descriptor baking, shader
  monomorphization per material, whole-program resource-lifetime + dead-resource
  elimination, zero-cost validation gating.

---

## Cross-cutting

- [~] **Compile time** whole-program+mono+comptime — measured super-linear; analysis
  caching / incremental is open (also in Perf Push residual/parked).
- [ ] **Overflow default**: checked also in release, wrapping only explicit
  ([REFERENCE.md](language/REFERENCE.md) §3.1). *(Enables Tier 3 `+%` vectorization.)*

## Cross-compilation (see [language/CROSS-COMPILE.md](language/CROSS-COMPILE.md))

Windows works (`--target x86_64-pc-windows-gnu` → running `.exe`). Follow-ups:
- [ ] **macOS cross-compile** — needs the macOS SDK. Wire up
  [osxcross](https://github.com/tpoechtrager/osxcross): detect `OSXCROSS_ROOT`/SDK,
  pass `--sysroot` + the right `-target`. Runtime code is already portable.
- [ ] **FreeBSD/BSD full build** — object emit works; add sysroot handling
  (`--sysroot <freebsd-root>`) so linking an executable succeeds here.
- [ ] **aarch64 targets** — verify `aarch64-pc-windows-gnu` (llvm-mingw) and
  `aarch64-unknown-linux-gnu` end to end (untested; codegen should already work).
- [ ] Windows **threads** produce a `.exe` (winpthreads) but execution under wine was
  flaky — verify on real Windows.

---

## Non-goals (deliberate)
Runtime `eval`/reflection · dynamic loading of unknown code · C-text preprocessor ·
deadlock-freedom guarantee · "all" C++/Rust libraries beyond the C-ABI boundary.
