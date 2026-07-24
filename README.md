# Vire

**Vire** is a programming language: *as light as Python, as fast as C/Rust,
memory-safe — without lifetimes, without ownership syntax, without manual memory
management.* It compiles **AOT** to native binaries through a whole-program solver
and an LLVM backend, and runs (for the provable majority) **without a runtime**.

> Name from the Latin *vīrēs* ("forces, strength") — light, yet powerful.
> File extension `.vr`. Current state: language specified; front-end, solver and
> backend built, compiling `.vr` to native binaries and benchmarked against
> Rust/C++/gcc.

```vire
fn word_counts(text: Str) -> Map[Str, Int] {
    mut counts = {}
    for word in text.lower().split_whitespace() {
        counts[word] = counts.get(word).or(0) + 1
    }
    counts
}
```

Reads like Python — compiles to a memory-safe, RC-eliminated native binary.

**`@vulkan` lets you write the whole GPU-driven meshlet renderer as one single-source
Vire program** — `@compute` build → `@task` cull → `@mesh` draw → `@fragment` shade —
that Vire compiles to the same SPIR-V as the handwritten GLSL. In a steady-state
mesh-shader benchmark (identical SPIR-V, 5000 frames) it shows no measurable runtime
overhead. The host side — pipeline, render pass, descriptors, synchronization — is
handled by a Vulkan runtime the compiler links in ([`vk_runtime.c`](crates/driver/src/vk_runtime.c)),
so the application author writes only the ~9 lines of shader logic instead of the 85–132
lines of C++/Rust setup. The **descriptor-set layout is now derived from the shader**
(V3, partial): the compiler reflects each stage's resource usage into a `VkIface`, and
the runtime builds the `VkDescriptorSetLayout` from it via one generic reflected path —
the binding, descriptor type, and stage mask come from the shader, not a hardcoded
per-demo layout (verified pixel-identical across textured / multi-sampler / meshlet-SSBO
paths). For the **mesh/meshlet path the whole pipeline layout is now shader-derived** —
the descriptor set *and* the push-constant range (size + stage mask) come from the
shader, so the `@task cull_plane()` push targets exactly the stage that reads it. Still a
fixed runtime protocol (not shader-varying): the graphics vertex/fragment pipeline's
16-byte per-frame `uniform()` channel and the compute-dispatch count; and on the roadmap
a `draw(pipe, mesh, uniforms)` host surface (see [TODO.md](TODO.md) V3/V4).
See [benchmarks/vulkan/](benchmarks/vulkan/) and [language/GPU-VULKAN.md](language/GPU-VULKAN.md).

## The idea in one paragraph

Classically, memory safety comes with one of three costs: a garbage collector
(runtime/pauses), ownership + lifetimes (Rust's annotation burden), or reference
counting (a small runtime). Vire resolves this **per program site**: a whole-program
solver **proves** ownership where possible (→ zero runtime, like Rust), and falls
back to lean RC where necessary. The programmer writes **zero** memory annotations.
Un-annotated types are **inferred** (Python ergonomics without Python's dynamic cost) —
today by a best-effort *monomorphic* unifier over a scalar type lattice (Int/Float/Bool/
ref/Unit); full Hindley–Milner with trait resolution and reference-type checking is
roadmap, not shipped (see [TODO.md](TODO.md) *Front-end completeness* and
[language/EVALUATION.md](language/EVALUATION.md) §5). This is feasible because Vire is
**closed-world** (all sources available at compile time) and sits on a backend that
already delivers exactly these proofs.

## Status & architecture

Vire is a **front-end** on a **built, measured backend**. The whole pipeline is
functional: `vire build foo.vr -o foo` and `vire run foo.vr` produce and execute
native binaries today.

| Layer | Status |
|---|---|
| **Vire front-end** (`crates/vire`) — lexer, parser, macro expansion, recursive inline, type inference, lowering to SSA IR | **built & working** — compiles `.vr` end-to-end to native code |
| **Mid-level IR** (`crates/ir`) | built |
| **Whole-program solver** (`crates/solver`) — devirtualization, inlining, escape/RC elision, bounds/null-check elision, field auto-narrowing, region inference | built |
| **LLVM backend** (`crates/backend`) — textual IR + clang `-O2 -flto -march=native`; TBAA, `!invariant.load`, branch weights, cold error paths; hosted/freestanding/threads | built |
| **Runtime** (`crates/driver`) — RC + Bacon–Rajan cycle collector, slab allocator, packed 16-byte header | built |
| **GPU kernels** (`@gpu`) — single-source device functions → NVPTX (`llc`) → PTX → CUDA Driver-API launch; up to **16× vs CPU** on an RTX 5070, bit-exact for integer kernels | built — [language/GPU-KERNELS.md](language/GPU-KERNELS.md), [benchmarks/gpu/](benchmarks/gpu/) |
| **`@vulkan` graphics** — Vire-authored shaders (`@vertex`/`@fragment`/`@mesh`/`@task`/`@compute`) → SPIR-V, a full **GPU-driven meshlet renderer** (compute-built scene → GPU frustum/backface cull → mesh-shader draw), plus textures, depth, a **render graph** (offscreen passes with automatic layout barriers), **interactive rendering** (per-frame loop + animated window), **lifetime-safe RC-bound GPU handles** (texture/buffer/session, 0-live verified), `@gpuvk` vendor-neutral compute, and a declarative `frame { }` — all from one Vire program; vendor-neutral (Intel iGPU + RTX 5070) | built — [language/GPU-VULKAN.md](language/GPU-VULKAN.md), [examples/vire/](examples/vire/) (`vulkan_*.vr`), `tests/vire_vulkan.sh` (35), `benchmarks/vulkan/` |
| **Tooling** — VS Code extension (syntax highlighting, `vire check` diagnostics, hover, go-to-definition, completion, quick fixes — via the **frontend compiled to WebAssembly**, so it needs no toolchain) + **native debugging** (breakpoints, stepping, call stack, **local variables**) via `--debug` DWARF + lldb-dap | built — [vscode-vire/](vscode-vire/) |
| **Cross-compilation** (`--target`) — **Windows** produces a working `.exe` (MinGW + LLD); BSD compiles to an object; macOS needs the SDK. The runtime is portable C | built — [language/CROSS-COMPILE.md](language/CROSS-COMPILE.md) |

The backend emits `-O2 -flto` and caches the runtime bitcode + verifies inline
`@c`/`@asm` blocks in parallel, so incremental `vire build` is fast (a small
build ≈ 0.12 s). The backend was developed and hardened via a **Java-bytecode
front-end prototype**
(the `fastjavac` path), whose **65 heap-balance regression tests (0 live objects at
exit)** are the soundness oracle — the floor every optimization must keep green. See
[DESIGN.md](DESIGN.md) and [benchmarks/](benchmarks/).

## Benchmarks (snapshot)

Cross-compiler on this machine (best-of-5, output-verified; Vire vs clang++ 22, g++
16, rustc 1.97, all `-O2 -flto -march=native`; measured 2026-07):

| Benchmark | Vire vs Rust | Vire vs clang++ | Notes |
|---|---|---|---|
| montecarlo / nbody / bitmanip | **~1.00×** | **~1.00×** | compute-bound, parity |
| **struct** (stack structs) | **0.90×** | **0.89×** | beats both |
| **binary-trees** | **0.91×** | 1.29× | region inference + move-on-last-use |
| **matmul** (256³ ikj) | **0.98×** | **0.91×** | ikj order → vectorized SAXPY; affine index elided |
| **vcall** (dyn dispatch) | **1.00×** | **0.44×** (2.3× faster) | solver devirtualization; beats clang `virtual` |
| **binsearch** (10M) | 1.03× | **0.78×** | midpoint check *proved* redundant + elided — safely |
| **sort** (quicksort 2M) | 1.06× | 1.33× | uncatchable checks abort noreturn (Rust's structure) |
| **graph** (BFS + Dijkstra, 1.6M edges) | **~1.00×** | **~1.00×** | was 1.61× — a region-array eager-zero fault, now fixed; RSS + time both at parity |

Across the Vire benchmarks (suite + [benchmarks/vire-lang/](benchmarks/vire-lang/)),
memory-safe Vire vs memory-safe Rust is at **Rust parity on the compute/struct/tree
kernels** — every one within ~9% of Rust and several faster (struct 0.90×, binary-trees
0.91×, matmul 0.98×, vcall = Rust / 0.44× clang). **`graph` (BFS + Dijkstra on a 1.6M-edge
digraph) was the one loss at 1.61×; it is now 1.12×, and the diagnosis is worth keeping**
because two plausible explanations were wrong. It is a flat-`i64`-array, pointer-chasing
kernel — **no objects, no reference counting, no cycle collector** (six integer arrays,
never a heap object), so *not* an RC/object-graph residual. Nor was it a bounds-check tax:
with **all** bounds checks removed (`FASTLLVM_NO_BOUNDS`, bit-identical output) the gap
barely moved, and Vire emits *fewer* checks than Rust here (2 vs 32 panic sites — its BFS
inner loop is 7 instructions, check elided and base in a register, against Rust's 10 with
an inline check and a per-iteration base reload). The actual cause, found by isolating
steady-state compute (8 warm reps over pre-allocated arrays): **compute was already 0.93×
Rust — Vire was *faster*.** The entire gap was one-time paging. `jrt_region_array`
default-zeroed each array with a full `memset`, which faulted in the whole 56 MB working
set — including the tail of the two worst-case-sized binary-heap scratch arrays the
algorithm never touches — whereas Rust's `vec![0; n]` gets lazy zero pages. The runtime
now fills region arrays lazily (memset only the reused prefix below a dirty high-water
mark; the fresh `mmap(MAP_ANONYMOUS)` tail stays zero and unfaulted). Result: **RSS 56 →
30 MB (= Rust's 30), time 38.8 ms vs Rust 39.9 (best of 10) — parity on both**, bounds
checks included (an earlier 1.12× reading was a small-sample artefact). Codegen is
byte-identical; verified by the Java 0-live oracle (67/67) and the differential fuzzer.
On the **Java→native**
oracle path the same backend takes **NBody 35.7× → 1.16×** (`Math.sqrt` now lowers to the
`sqrtsd` intrinsic, not a 60-iteration Newton call) and **binary-trees 1.73× → 0.81×,
beating Rust** (a shape/freshness analysis drops the cycle collector for provably
tree-shaped types). The solver *proves* array indices in range (the `(lo+hi)/2` midpoint,
the affine `r*n+k`) and, where a check can't be elided, makes it as cheap as Rust's (a
noreturn abort when provably uncatchable) — **all fully memory-safe: a genuinely
out-of-bounds access still throws**.

**Memory (peak RSS)** is reported alongside time in every suite: Vire is **at or below
both Rust and C++ on essentially every benchmark** — ~2 MB under clang everywhere (no
`libstdc++`/iostream baseline), level with Rust, and even binary-trees (pure alloc/GC)
peaks *under* both (RC frees eagerly, 0 live, no growing GC heap). `graph` used to be the
exception (56 MB) until the `jrt_region_array` lazy-fill fix above: it now peaks at **30
MB, level with Rust's 30 and under clang++'s 58** — a region array only makes resident the
pages the program actually touches, exactly like Rust's `vec!`.

Beyond single kernels, [benchmarks/complex/](benchmarks/complex/) runs **multi-algorithm
workloads** (a generate→sort→search→histogram pipeline; integer k-means) and **fair
fork/join multithreading** — parallel Monte-Carlo and Mandelbrot with **4 threads in
Vire, Rust, and C++** (bit-identical output). The threading is real: `pmontecarlo` scales
**3.98× on 4 cores** and Vire is at/ahead of Rust/C++ (0.97×) — `spawn`/`Atomic` add no
overhead over raw `std::thread`. See [TODO.md](TODO.md), [benchmarks/](benchmarks/).

## Building & compiling programs

The whole pipeline is one command. Optimization is on by default
(`clang -O2 -flto -march=native`, closed-world AOT for the host CPU).

```console
$ vire build hello.vr -o hello     # compile to a native binary
$ vire run hello.vr                # compile to a temp binary and run it
```

**Common flags** (all additive to the same solver + backend):

| Flag | Effect |
|---|---|
| `-o FILE` | output path |
| `--emit=obj\|asm\|llvm\|ir\|staticlib` | stop at a `.o` (one relocatable C-ABI object, incl. `main`), assembly, LLVM/mid IR, or a `.a` |
| `--deps FILE` | write a Makefile/Ninja depfile (for incremental builds) |
| `-I DIR` | include path for `native "c"` blocks / headers |
| `--pkg NAME` | pull cflags+libs from **pkg-config** (first-class system deps) |
| `-l NAME` / `--obj FILE` | link a library / a `.c`/`.cpp`/`.o`/`.a` (C/C++/Rust via the C ABI) |
| `--target TRIPLE` | cross-compile (e.g. `x86_64-pc-windows-gnu`) |
| `--log-level debug\|info\|warn\|error\|off` | build-time log threshold (below it = zero instructions) |
| `--syntax FILE` | opt-in custom keyword spellings (also via an in-file `//!syntax: FILE`) |
| `-g` / `--backtrace` | DWARF debug info (`.vr:line`) / native backtrace on an uncaught throw |

FFI is source-level: `extern "C" header "h.h" { … }` auto-generates bindings from a C
header at compile time; `native "c"/"c++"/"asm" """ … """` blocks are compiled and linked
in automatically (and **proven memory-safe** by the vendored verifier, the sound
replacement for `unsafe`).

### With Meson

A whole `.vr` program lowers to **one relocatable C-ABI object** (the runtime `main`
included), so Meson links it like any C/C++/Rust object. Using only the stock Meson DSL
(no plugin to install):

```meson
project('app', 'c', meson_version: '>=1.1.0')
vire = find_program('vire')

main_obj = custom_target('main.vr.o',
  input: 'main.vr', output: 'main.vr.o',
  command: [vire, 'build', '@INPUT@', '--emit=obj', '-o', '@OUTPUT@', '--deps', '@DEPFILE@'],
  depfile: 'main.vr.o.d')                 # incremental: rebuilds on source/header change

executable('app', 'util.c', objects: main_obj, link_args: ['-lm'])
```

```console
$ meson setup builddir && ninja -C builddir && ./builddir/app
```

This links a Vire object with a C object (`-lm` because the runtime uses libm). Add
`-pthread` if the program uses `spawn`, and system libraries via `--pkg`. An optional
`import('vire')` module (`vire.executable()` / `vire.static_library()`) and a tested,
runnable example are in [build-integration/meson/](build-integration/meson/).

## Memory management: how little the runtime does

There is no tracing GC. Memory is reference-counted, and the whole-program solver
eliminates as much of the runtime bookkeeping as it can **prove** away statically, so
what's left for the runtime to "handle" (allocate + RC retain/release + cycle
collection) is small — and, since the runtime work this project added, **spike-free**.

**What the solver removes statically** (verified against the code):
- **Compute-bound code → 0% runtime handling.** No heap allocation at all
  (`FASTLLVM_HEAPSTATS` shows no `[heap]` line).
- **Traversal / read-only paths → RC already fully elided.** Borrow-slot analysis
  (a field read from a stable base whose field the function *and its transitive
  callees* never write is a borrow, not an owned ref), interprocedural
  field-write analysis, and move-on-last-use together reduce RC on read/traverse code
  to the `--no-rc` ceiling (`normal == --no-rc`).
- **Provably-local allocations → stack / region / arena, immortal.** Escape analysis
  promotes non-escaping objects to `alloca` (`StackNew`) and arrays to a bump region;
  a `while`-loop body whose allocations provably can't leave the iteration
  (interprocedural check) is bracketed in a per-iteration arena. All of these are
  **immortal** — no per-object RC, no collector, freed en bloc — and are **not**
  counted as runtime-handled at all.

**What still reaches the runtime** — measured, honest: allocation-heavy **object
graphs** (trees, lists, ASTs) whose nodes escape into the structure. Example:
`build(18)` for a binary tree = ~524k heap nodes, all RC-managed (and, being a
self-referential type, kept under the cycle collector). This is the residual *mechanism*.

**Honest scope of that residual.** On the one object-graph benchmark in the suite —
binary-trees — this mechanism costs nothing measurable: Vire runs it **0.91× Rust and
peaks under both Rust and C++** (the analyses below drive its RC to the `--no-rc`
ceiling). The graph benchmark above is *not* an instance of this — it has no objects at
all. So the demonstrated suite does **not** contain a workload where RC/collector traffic
is the measured bottleneck. Such workloads exist — heavily *shared*, topology-*mutating*
object graphs with genuinely dynamic lifetimes (mutable caches, general graph mutation) —
and they are the honest boundary of what these numbers show: the suite demonstrates that
the elision analyses hold on structured-lifetime object graphs, not that RC is free in
full generality. The lever below is aimed at that un-benchmarked class.

**The lever, measured — and the corrected diagnosis.** The lever is *auto-inferred
arena inference*: where the solver can prove a whole subgraph is built, used, and dies
within one scope (`t = build(); use(t); drop`), route its allocations into an arena
(immortal, bulk-freed, zero RC/collector). The mechanism already exists (the
`capsule`/loop arena). [benchmarks/complex/sharedgraph.vr](benchmarks/complex/sharedgraph.vr)
now measures the un-benchmarked class directly — a shared, topology-*mutating*, *cyclic*
object graph, built and dropped per iteration. Rebuilding it three ways decomposes the
cost (400k trials, 8M nodes): `--no-rc` ceiling **273 ms**, `--no-cycles` (RC on,
collector off) **394 ms**, shipping (RC + collector) **729 ms** — so the collector is
**338 ms / 46 %** and RC retain/release **121 ms / 17 %** of runtime. Fair Rust
(`Rc<RefCell<Node>>`, cycle broken by hand) is **145 ms**; Vire is **5.0× here**. This
is the honest cost of the class, not an estimate.

An earlier attempt to auto-fire the arena at function scope was reverted, and this
benchmark **corrected the diagnosis that reverted it.** Recursion is *not* the blocker:
the loop-arena already fires on a recursive build/use/drop (a non-mutating variant of
this benchmark emits `jrt_arena_push`/`pop` around a recursive `chain(20)` and reports
*zero* heap allocations — all 8M nodes immortal in the arena). What suppressed it was one
thing: the ref-storing field mutation `last.next = h`, whose base the region check could
not prove was itself arena-local, so it conservatively bailed. **This lever is now built**
(`while_arena_safe` + `loop_fresh_locals`/`expr_is_fresh` in `crates/vire/src/lower.rs`):
a ref-storing field mutation `obj.f = ref` is admitted into the arena when `obj` *and* the
stored `ref` are provably *iteration-fresh* — every reaching definition comes from a
constructor, a call with fresh arguments, a field-read of a fresh object, `null`, or a
scalar (a greatest-fixpoint freshness dataflow over the loop body). Both fresh ⇒ both live
and die in the arena ⇒ the store creates no dangling pointer and captures no non-arena ref.
On sharedgraph the arena now fires: **729 ms → 352 ms (2.08×; 5.0× → 2.4× Rust)**, zero
heap allocations, zero collector work — verified 0-live and `GUARD_FREE`-clean. It stays
soundness-critical (a wrong freshness verdict would be a use-after-free), so it is pinned
in **both** directions by `tests/vire_interproc_arena.sh` (promote: fresh cyclic mutation;
decline: mutation whose base is an outer object, whose value is an outer ref, or whose base
is only conditionally fresh) plus the Java 0-live oracle, `GUARD_FREE`, and the fuzzer. It
does *not* reach the 273 ms ceiling — the arena's per-iteration push/pop and bump-alloc are
real — but it removes the entire 46 %-collector + 17 %-RC cost on the class.

The **hard floor** is unchanged and real: **genuinely dynamic-lifetime graphs** (mutable
caches, long-lived graphs mutated past any single scope) cannot be proven away by any
static analysis — they structurally need dynamic RC + the collector. The arena lever
covers structured-lifetime graphs (compilers/ASTs, request handlers, per-frame scene
graphs); it does not make RC free in full generality. And even at the ceiling Vire's bare
allocation path here is ~1.9× Rust's `Rc` path — a separate, honestly-open codegen gap,
not something the arena closes.

**No latency spikes.** The three synchronous runtime operations were made incremental/
budgeted (see [DONE.md](DONE.md)): the cycle collector runs in bounded steps
(continuous, buffer-bounded RAM), the release **free-cascade** of a large dead
subgraph is spread across operations, and a large collected garbage cycle's free is
deferred — all verified 0-live (Java oracle 67/67, a `listdrop` leak-catcher, a 2M-node
ring, flat RSS across 8–16× allocation churn).

**Temporal safety of the RC residual — a second eye beyond 0-live.** The 0-live oracle
checks the heap *balance*, not the *timing*: for the interesting configuration (a heap
object with the retain elided, so exactly one release), a premature free — dropping an
object still reachable through a second reference — leaves `live_objects` at 0 anyway.
So `FASTLLVM_GUARD_FREE=1` adds a non-perturbing check (companion to
`FASTLLVM_HEAPSTATS`): each RC-managed object gets its own guard-paged mapping, and at
`rc→0` the runtime `mprotect(PROT_NONE)`s it instead of recycling the memory, turning
any premature free into a **deterministic SIGSEGV at the dereference**. It is runtime-
only — codegen is byte-identical to the shipping binary, so unlike ASan it does not
disturb the RC-elision it checks (a modeled premature free is caught; the same read
without the guard silently returns recycled bytes). Every ownership program — `build(16)`,
shared-child DAGs, escaping chains with a live alias, list traversal — runs **clean**
under it. (The GPU/`@vulkan` binaries are excluded: the guard-page layout perturbs the
GPU driver's own `atexit` teardown — a `SEGV_MAPERR` in its `munmap` cascade, i.e. an
instrument artifact at the external-driver boundary, not an RC premature free.)

## Documents

- **[TODO.md](TODO.md)** — roadmap and remaining work (M0 risk gate, front-end
  pipeline, features 1–8, performance).
- **[DESIGN.md](DESIGN.md)** — backend architecture (solver, memory model,
  benchmarks). Describes the Java-bytecode path = the proof/bootstrap base.
- **[language/EVALUATION.md](language/EVALUATION.md)** — honest feasibility: the three
  tensions (no runtime / all libraries / Python-light) and §7 residual risks
  (alias precision, compile time).
- **[language/LANGUAGE.md](language/LANGUAGE.md)** — syntax tour (quick start).
- **[language/REFERENCE.md](language/REFERENCE.md)** — full syntax/feature reference.
- **[language/FEATURES-EVALUATION.md](language/FEATURES-EVALUATION.md)** — assessment of
  the eight requested features (multithreading, templates, comptime reflection, own
  preprocessor, Meson, logger, Go-style error handling, debug crash paths).
- **[language/PARSER.md](language/PARSER.md)** — parser/front-end build plan.
- **[language/GPU-KERNELS.md](language/GPU-KERNELS.md)** — `@gpu` data-parallel device
  kernels (NVPTX → CUDA).
- **[language/GPU-VULKAN.md](language/GPU-VULKAN.md)** — `@vulkan`: Vire-authored
  shaders and the GPU-driven meshlet renderer. Ends with a **shipped reference** (every
  stage, builtin, host entry point, and the build→cull→draw pipeline).
- **[language/examples/](language/examples/)** — example programs across areas and
  features.
- **[vscode-vire/](vscode-vire/)** — VS Code extension. Language
  intelligence (diagnostics, hover, go-to-definition, outline) runs the **frontend
  compiled to WebAssembly** (`crates/vire-wasm`), so it works on **Windows/macOS/
  Linux with no toolchain installed**. Plus syntax highlighting, snippets, and
  native debugging (breakpoints, stepping, call stack, **local variables**) via
  `--debug` DWARF + lldb-dap.
- **[benchmarks/](benchmarks/)** — benchmark suite (Java/Rust/C++), runner, analysis.
- **[LICENSING.md](LICENSING.md)** — dual license: CSolver (`crates/csolver/`) under
  Apache-2.0, everything else under GPL-3.0-or-later.

## License

Dual-licensed by directory: **CSolver** (`crates/csolver/**`, the vendored
memory-safety verifier) under the **Apache License 2.0**
([`crates/csolver/LICENSE`](crates/csolver/LICENSE)); **everything else** under the
**GNU GPL v3.0 or later** ([`LICENSE`](LICENSE)). See [LICENSING.md](LICENSING.md).

## Core language ideas (in brief)

- **Inference over annotation** — types appear nowhere yet are all known.
- **No `null`** — `Option[T]`; no exceptions — errors are values (Go spirit) with
  `?` propagation.
- **`type`** for product and sum types (value types, no object header), **traits** +
  monomorphized **generics**.
- **`comptime`** — code that runs in the compiler: reflection, derivations,
  conditional compilation — zero-cost, no runtime metadata ballast.
- **Invisible memory** — stack/heap/RC decided by the solver; `&` optional.
- **Concurrency safe by construction** — channels (CSP) + `Mutex`/`Atomic`; the
  solver rejects shared bare mutable state.
- **`capsule`** — a fault-containment sandbox: body allocations go into a private
  arena freed en bloc (RC-/collector-free). Inputs are **deep-copied in**, results
  **deep-copied out** (arrays and arbitrary structs/graphs, cycles + sharing
  handled), so a bug in the body can't touch the caller's data.
- **GPU kernels** — a `@gpu fn k(i: Int, …)` runs data-parallel on the GPU
  (single-source: NVPTX → PTX → CUDA Driver-API launch), with the thread index
  injected like a `parallel_for` worker `(i, …)`. Up to **16× vs CPU** on an
  RTX 5070, bit-exact for integer kernels. See
  [language/GPU-KERNELS.md](language/GPU-KERNELS.md).
- **`@vulkan` graphics** — write the shaders in Vire (`@vertex`/`@fragment`/`@mesh`/
  `@task`/`@compute` → SPIR-V) and get a **GPU-driven meshlet renderer** from one
  program: a `@compute` builder fills the scene on the GPU, `@task` frustum/backface-
  culls each meshlet, `@mesh` draws the survivors, `@fragment` shades — normally
  GLSL/HLSL + C++ + a mesh toolchain. Plus textures, depth, a **render graph**
  (offscreen passes with automatic barriers), **interactive rendering** (per-frame loop
  + animated window), **lifetime-safe RC-bound GPU handles** (freed when the last Vire
  reference drops — no GPU use-after-free), and `@gpuvk` vendor-neutral compute (the
  Vulkan counterpart to the CUDA/ROCm `@gpu` track). Vendor-neutral. See
  [language/GPU-VULKAN.md](language/GPU-VULKAN.md).
- **C native** — `extern "C"`/header bindings; C++/Rust via the C ABI. Meson
  first-class.

The name and details are provisional and easy to change; the design is the core.
