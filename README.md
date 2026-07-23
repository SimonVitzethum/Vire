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

**`@vulkan` compiles to the same Vulkan API calls as handwritten C++ and Rust.** In a
steady-state mesh-shader benchmark (identical SPIR-V, 5000 frames), Vire shows no
measurable runtime overhead while reducing the application code from 85–132 lines to
just 9 — the pipeline, render pass, descriptors, and synchronization are compiler-
generated. The same single-source program spans the whole GPU-driven meshlet renderer
(`@compute` build → `@task` cull → `@mesh` draw → `@fragment` shade). See
[benchmarks/vulkan/](benchmarks/vulkan/) and [language/GPU-VULKAN.md](language/GPU-VULKAN.md).

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

Across the 12 Vire benchmarks (suite + [benchmarks/vire-lang/](benchmarks/vire-lang/)),
memory-safe Vire vs memory-safe Rust is a **geometric-mean 1.00× (median 1.00×) — at
Rust parity**, with every benchmark within ~9% of Rust and several faster (struct 0.90×,
binary-trees 0.91×, matmul 0.98×, vcall = Rust / 0.44× clang). On the **Java→native**
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
peaks *under* both (RC frees eagerly, 0 live, no growing GC heap).

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
self-referential type, kept under the cycle collector). This is the residual.

**Pushing the residual under ~0.5% — the lever and the honest ceiling.** The obvious
lever is *auto-inferred arena inference*: where the solver can prove a whole subgraph
is built, used, and dies within one scope (`t = build(); use(t); drop`), route its
allocations into an arena (immortal, bulk-freed, zero RC/collector). The mechanism
already exists (the `capsule`/loop arena). An attempt to auto-fire it at function
scope was **reverted**: (a) the interprocedural escape check conservatively rejects
*recursive* builders — exactly the tree/AST case we'd want — so extending it safely is
non-trivial soundness-critical work (a wrong escape verdict is a use-after-free), and
(b) the simple non-recursive cases the escape analysis *already* stack-promotes. So it
stays a carefully-scoped future item ([TODO.md](TODO.md), Tier 1). And there is a hard
floor: **topology-mutating / genuinely dynamic-lifetime graphs** (general mutable
graphs, unpredictable-lifetime caches) cannot be proven away by any static analysis —
they structurally need dynamic RC + the collector. For the common structured-lifetime
workloads (compilers/ASTs, request handlers, batch processing) sub-0.5% is reachable;
in full generality it is not.

**No latency spikes.** The three synchronous runtime operations were made incremental/
budgeted (see [DONE.md](DONE.md)): the cycle collector runs in bounded steps
(continuous, buffer-bounded RAM), the release **free-cascade** of a large dead
subgraph is spread across operations, and a large collected garbage cycle's free is
deferred — all verified 0-live (Java oracle 67/67, a `listdrop` leak-catcher, a 2M-node
ring, flat RSS across 8–16× allocation churn).

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
