# Vire — DONE (completed work)

Archive of shipped/closed items moved out of [TODO.md](TODO.md) to keep the
roadmap to *open* work only. Newest sections first. Design basis:
[language/](language/). Soundness floor held for every item: Java heap-balance
oracle **65/65** + `tests/vire_heap.sh` 0-live + all `tests/vire_*.sh` green.

---

## Pipeline baseline (functional + green)

Full pipeline working: lexer → parser → macro expansion → recursive inline →
type inference → lowering to SSA IR → whole-program solver → LLVM backend →
`clang -O2 -flto -march=native`. `vire build`/`vire run` produce native
binaries. Traits (vtable dispatch + devirtualization), arrays, structs/records,
generics-by-inlining, `match`/sum types, `Result`/`Option` + `?`, `comptime if`,
`list()`/`map()`/`set()` collections, `Str` methods, iterator adapters, and
`log.*` compile-time-filtered logging all work.

**Performance headline (measured):** geomean **~1.00× Rust** across 12 Vire
benchmarks, RAM at/under Rust and clang on every row. Compute-bound at parity or
faster; virtual dispatch **2.4× faster** than clang (devirt). See
[benchmarks/vire-lang/README.md](benchmarks/vire-lang/README.md).

---

## Performance (shipped)

- **#1 distinct-array alias metadata (`!alias.scope`/`!noalias`) — RULED OUT, measured.**
  `noalias` on allocator returns already tells LLVM distinct arrays don't alias;
  A/B identical on graph/sort/compression/pquicksort (latency/scheduling-bound,
  not aliasing-bound). Per-access alias metadata adds nothing + carries miscompile
  risk. Do NOT rebuild.
- **RC inline in the backend (retain/release as IR) — BUILT then REVERTED, measured.**
  Correct (65/65, all suites) but (1) dropping `-flto` regresses perf (other hot
  runtime helpers stop inlining: struct −18%), (2) covers only acyclic programs.
  Do NOT re-attempt without keeping LTO's inlining of the other hot helpers.
- **Vtable load `!invariant.load` — fixed** (backend.rs). Same unsound
  calloc-then-write pattern as the array length; fixed soundly before it bit under LTO.
- **Array as a function parameter** — `fn qsort(a: Array[Int], lo, hi){ a[i] }`.
  Element kind recorded in `local_arr` at param binding; `a[i]`/`a[i]=v`/`a.len()`
  lower to real bounds-checked accesses. `tests/vire_heap.sh array_param_qsort`.
  Measured: recursive `qsort` is *slower* than the explicit stack (per-call overhead
  + lost cross-call bounds elision) — benchmark stays as-is; feature value is
  array-taking helpers generally.
- **Allocator gap — closed for the array case.** Non-escaping fixed-size primitive
  arrays stack-promote (`StackNewArray`→`alloca`), reusing the object escape
  analysis (returned/stored arrays correctly stay heap). The `for … array(16)` loop
  that was ~20× Rust → **0.27× Rust**; nested variant **0.06× Rust** (was 9.9×).
  btree 1.08× Rust. `escape.rs STACK_ARR_CAP`.
- **Second (region) stack for dynamic/large arrays + multiple stacks.** Non-escaping
  arrays too big/dynamic for the call stack go into a bump-region arena when in a
  promotable loop body, freed per iteration. Region is **thread-local** → concurrent
  `spawn` workers each own a region stack (no shared `arena_top` race).
  `tests/vire_threads.sh per_thread_arena`.
- **Function-scoped region** for non-escaping dynamic/large arrays not in a loop:
  bump-allocated per-thread (`jrt_region_array`), bracketed `jrt_region_enter/leave`,
  freed en bloc at return. `FASTLLVM_NO_REGION` A/B knob.
- **Interprocedural escape/region for short-lived heap graphs** (the
  `benchmarks/complex/compiler` case): 1.25× C++ / 20 MB → **1.08× C++ / 17 MB**.
  The loop-arena escape check (`while_arena_safe`) is now interprocedural — a
  callee's own `return`/`break`/`continue` no longer disqualifies the arena; every
  allocation the iteration transitively performs (across `parse`/`eval`) lands in
  the thread-local arena, freed en bloc, zero per-node RC + zero heap `malloc`.
  Pinned both directions by `tests/vire_interproc_arena.sh`. Node-pool/SoA rewrite
  ruled out (slower, 0.040 s).
- **Relational bounds elision — landed parts:** constant upper/lower-bound Kleene
  fixpoint over loop phis (`compute_ub`/`compute_lb`) → binary search `a[mid]` elides,
  **binsearch 1.23×→1.00× Rust**; guard-aware affine "Path 4" (`N·a+b < N² ≤ len`) →
  **matmul 1.64×→1.22× Rust** (beats clang 0.96×), inner loop 8× FMA; noreturn `_fatal`
  check model → **sort 1.35×→1.05× Rust**. (Foundation in `crates/solver/src/bounds.rs`;
  further field/interproc elision still open — see TODO.)
- **`Math.sqrt` → `@llvm.sqrt.f64`** (single `sqrtsd`, replacing 60-iter Newton) —
  the NBody 35.7×→1.16× win.

---

## Compile-time programming layer (shipped)

- **Phase 0 — persisted type graph** (`tygraph.rs`): source-level structural
  `TypeGraph::build` built after inference, decoupled from lowering. Preserves what
  the IR erases (generics, nested type apps, borrow marks). `vire types FILE.vr`.
  `tests/vire_types.sh` 15/15.
- **Phase 1 — typed expressions** (`infer.rs` `infer_module_typed`): `ExprTypes`
  side-table, resolved type per expression keyed by span. `vire infer FILE.vr`.
  `tests/vire_infer.sh` 8/8.
- **(a) comptime evaluator core** (`comptime.rs` `Interp`): comptime `let`/assignment,
  `for`/`while`/`if` at compile time, pure module fn calls with recursion, step +
  recursion budget (infinite loop = compile error), lexical isolation. Powers
  `const F = fact(6)`, `array(comptime fact(4))`. `tests/vire_comptime.sh` 9/9.
- **`comptime assert(cond[, "msg"])`** — evaluated at compile time; false → compile
  error; non-constant condition rejected; folds to no-op.
- **`@when(platform)` conditional compilation** — `@when(linux|macos|windows|unix|freebsd)`
  on a `fn`/`type`, included only for the matching target, dropped before inference.
  `platform.rs`, `tests/vire_comptime.sh`.

---

## Features 1–8 (shipped parts)

**[1] Multithreading, safe by construction**
- `spawn worker(args…)` + `join(h)` — function-pointer thread model via `jrt_spawn`;
  threads auto-enable on `spawn`; multi-arg env buffer; workers kept as RTA roots.
- `Atomic` (`.fetch_add`/`.load`) + `Mutex` (`.lock`/`.unlock`/`.get`/`.set`) —
  shared race-free primitives. `tests/vire_threads.sh` 8/8.
- **Send check**: a `spawn` worker param must be scalar (copied) or Sync
  (`Atomic`/`Mutex`); sharing a bare mutable record/list = compile error.
- `Channel` (`.send`/`.recv`, blocking) — thread-safe FIFO, a Sync type.

**[2] Template programming**
- Generics `[T: Trait]`, multiple bounds `T: A + B`, static trait resolution →
  direct (inlined) calls; a violated bound = precise compile error at instantiation.
- **Value generics `[comptime N: Int]`** with turbofish `f[N](..)`: distinct monomorph
  per N, N as literal (so `0..N`/`array(N)` become constant → stack-promote).
- Array **parameter** indexing in a Vire body (see [1]).

**[4] Preprocessor (= comptime/@if/macros)**
- `@when` platform switches (see above).

**[5] Build interop, Meson first-class — DONE**
- Stable compiler CLI: `--emit=obj|asm|llvm|ir|staticlib`, `--deps` depfile, `-I DIR`.
  Whole `.vr` program → ONE relocatable C-ABI object; `--emit=staticlib` → `.a`.
- Meson integration (`vire.executable/static_library`), C-ABI `.o`/`.a`,
  `build-integration/meson/`, optional `import('vire')` (`vire.py`).
- pkg-config deps first-class: `--pkg NAME` → `--cflags`/`--libs` forwarded to
  native-block compile + link (tested against zlib). `vire bindgen` for headers.

**[6] Logger (shipped parts)**
- Compile-time level filter (disabled calls = 0 instructions).
- **Structured fields** via `{}`: `log.info("user={} ms={}", id, t)`, positional,
  zero-cost-when-disabled; placeholder/arg mismatch = compile error.
- **Build-time level** `--log-level` (env `FASTLLVM_LOG_LEVEL`), below-threshold
  calls lower to nothing. `tests/vire_log.sh`.

**[8] Debug symbols + crash paths**
- `--backtrace`: native backtrace on uncaught exception / hard crash (SIGSEGV/SIGBUS),
  captured at throw origin, off by default (zero overhead). `tests/vire_debug.sh`.
- **DWARF debug info** (`--debug`/`-g`): `DICompileUnit`/`DIFile`/`DISubprogram`+
  `DILocation` → `.vr` source; debug builds `-O0 -no-pie` so gdb/lldb/addr2line
  resolve to `.vr:line`.
- **Per-statement `DILocation`** (exact crash line) via `DebugLine` markers.
- **`inlinedAt` inline chains**: Vire's inliner splices carry the inline stack;
  backend builds the `!DILocation`→`inlinedAt` chain (`addr2line -i`/gdb show it).

---

## GPU kernels (`@gpu`) — shipped

Single-source `@gpu fn` → nvptx64 LLVM module → PTX (`llc -march=nvptx64 -mcpu=sm_90
-O3`) → embedded C string + launch stubs → CUDA Driver API (libcuda). Kernels in
`Program::gpu_kernels` (out of `functions`). Intrinsics `gpu_gid/gpu_gsize/tid/bid/
bdim/gdim`. Design adapted from NVlabs/cuda-oxide (Apache-2.0, `crates/cuda-oxide`).
`crates/backend/src/nvptx.rs`, `crates/driver/src/gpu_runtime.c`. Guarded by
`tests/vire_gpu.sh` (integer bit-exact vs CPU + error path). **Measured 16× vs CPU**
on an RTX 5070. Docs: [language/GPU-KERNELS.md](language/GPU-KERNELS.md).
- Host `farray[i] = <int>` coerces int→f64 (was invalid IR) — seeds float kernels.

### GPU perf — parts adapted from cuda-oxide's compiler design (idea, not code)
See [benchmarks/gpu/VS-CUDA-OXIDE.md](benchmarks/gpu/VS-CUDA-OXIDE.md) for the full
Vire-vs-cuda-oxide architectural analysis.
- **Device-module middle-end** (`opt -O3` before `llc`): the NVPTX emitter produces
  naive alloca-per-local IR (no phis); `llc` runs codegen passes but not the
  target-independent mid-end, so loop scalars could spill to slow `.local` memory.
  The build now runs `opt -O3` on `gpu_device.ll` before PTX codegen — giving Vire
  kernels the same middle-end a Rust→PTX path gets from rustc. Measured: saxpy 13
  device allocas → 0 (register-promoted). Best-effort fallback if `opt` is absent
  (llc -O3 still runs). `crates/vire/src/main.rs` (`want_gpu` branch).
- **Read-only array analysis** (`read_only_params`, `crates/backend/src/nvptx.rs`):
  adapted from cuda-oxide's typed in/out `DeviceBuffer` distinction. Proves which
  array params a kernel never `ArrayStore`s into (copy-alias fixpoint over the
  kernel body) and skips their D2H copyback. Sound-conservative: an array base not
  traceable to a parameter forces every array to in/out, so a needed copyback is
  never dropped; an array passed to any device Call (e.g. `gpu_atomic_add`) also
  counts as written. Verified bit-exact (`tests/vire_gpu.sh`; saxpy `x` skips D2H,
  `y` still downloads).

### GPU G1 — device-programming primitives (intrinsic-based subset)
Added to the `gpu_intrinsic` dispatch (`crates/backend/src/nvptx.rs`) + frontend
(`lower.rs` `gpu_intrinsic_typed`, `infer.rs` return types). All integer/IEEE cases
bit-exact vs CPU on an RTX 5070 (`tests/vire_gpu.sh` 8/8). See
[language/GPU-KERNELS.md](language/GPU-KERNELS.md).
- **Block barrier** `gpu_sync()` → `@llvm.nvvm.barrier0` (unit-typed so it can be a
  kernel's tail statement — required teaching `infer.rs` the intrinsic return types).
- **Warp intrinsics** `gpu_shfl_down(v,d)` (`shfl.sync.down.i32`) and
  `gpu_warp_reduce_add(v)` (5× shuffle+add full-warp sum) → the fast-reduction idiom
  (warp-reduce → atomic) with no shared memory. Verified: 1024 threads → 1024.
- **Device atomics** `gpu_atomic_add(arr, idx, v)` → `atomicrmw add` (global,
  Int/Long), returns the old value. Verified: histogram 1000 → 1000.
- **IEEE math** `gpu_sqrt/fabs/floor/ceil` (`@llvm.<fn>.f64`) + `gpu_fmin/fmax`
  (`@llvm.minnum/maxnum.f64`), round-to-nearest = bit-exact vs the CPU runtime.
  Verified: Σ sqrt(i²)=Σi=4950; Σ floor(3.7)=300.

### GPU — Vulkan/SPIR-V backend investigation
Investigated a vendor-neutral second `@gpu` target. Verdict: high value, de-risked
(LLVM 22 ships `spirv64`; Vulkan stack present; both Intel iGPU + RTX enumerate).
Design/trade-offs/recommendation in [language/GPU-VULKAN.md](language/GPU-VULKAN.md);
roadmap item in TODO.md (GPU G4). Not yet built.

---

## Capsule deep-copy — shipped

`capsule` fault-containment arena with deep-copy in/out. See
[language/M0.2-CAPSULE-ARENA.md](language/M0.2-CAPSULE-ARENA.md).
- Arrays (`array`/`farray`) deep-copied IN (cloned into arena, isolated) and OUT
  (`jrt_arena_export_array` to RC heap), 0-live.
- **Arbitrary concrete structs/graphs** in/out with **cycles + sharing**: vtable
  **slot 3 = deep-copy** (`jrt_deep_copy_ref` dispatches on runtime type), transient
  pointer→pointer **copymap** (register-before-recurse → cycles terminate, shared
  subgraphs stay shared). Copy-IN→arena / copy-OUT→heap, one codepath.
- Array-typed struct fields indexable in the body: `(type, field)→element kind` side
  table tags the GetField result so `x.field[i]`/`= v`/`.len()` lower to bounds-checked
  accesses. Trait-object fields dispatch through slot 3 too.
- Tests (all 0-live): `capsule_array_out/in/io`, `capsule_struct_io/cycle/share/arrayfield`.

---

## Tooling — shipped

- **VS Code extension + native debugger + LSP** ([vscode-vire/](vscode-vire/)):
  highlighting, diagnostics/hover/go-to-def/completion/quick-fixes via a wasm-compiled
  frontend (no toolchain), breakpoints + local-variable inspection via DWARF + lldb-dap.
- **Cross-compilation**: `--target x86_64-pc-windows-gnu` → running `.exe` (`_WIN32`
  time branch + `-fuse-ld=lld`); BSD → object; macOS needs the SDK. See
  [language/CROSS-COMPILE.md](language/CROSS-COMPILE.md).
- **Faster builds**: cached runtime bitcode keyed by (content, `-D`, target, clang
  version) — empty build 0.48 s → 0.12 s, no-inline 0.51 s → 0.14 s (~4×), lossless.
  Skipped under PGO / `-g` / freestanding.
- **Parallel inline-block verification**: cold `@c`/`@asm` verification runs
  concurrently (bounded by CPU count), content-addressed PASS cache.

---

## External usage findings — Baby-LOOM emulator (2026-07-21)

- **Parser call-adjacency fix** (root cause, not lowering): `parse_postfix` bound a
  `(` as a call to any preceding expr across whitespace, so `mut y = x + 0.5  (y as
  Int)` parsed as a call → the M2 error. Fix: a `(` forms a call only when adjacent
  (`toks[pos-1].span.1 == toks[pos].span.0`). `f(x)` = call, `f (x)` = two stmts. The
  whole corpus uses `f(x)` (no regression). This also fixed the `farray`-in-helper
  symptom (same root cause) and cverify.sh 4/14→14/14. Suites 109/109.

**Worked well (no action):** `farray`/`array` params with in-place writes through
helpers; nested `while`; `if/else`; `%`, casts, Float arithmetic; `@gpu` kernels with
`farray` params + `while`-loop reduction bit-exact vs CPU (matvec 128/128).
