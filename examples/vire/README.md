# Vire examples

Small, self-contained Vire programs. Each builds and runs with the standard
compiler — no syntax config needed:

```sh
cargo build --release -p vire
target/release/vire run examples/vire/threads_atomic.vr
```

Run and check them all at once:

```sh
sh examples/vire/run.sh
```

## Concurrency (safe by construction)

Vire's threading is race-free by construction: a value crossing a `spawn`
boundary must be a scalar (copied per thread) or a `Sync` type (`Atomic` /
`Mutex`). Sharing a bare mutable object is a **compile error** — you cannot write
the data race. Threads (atomic reference counting + pthreads) link in
automatically whenever a program uses `spawn`.

| File | Shows |
|---|---|
| [threads_atomic.vr](threads_atomic.vr) | `spawn` / `join` and a shared `Atomic` counter (`fetch_add` / `load`) |
| [threads_workers.vr](threads_workers.vr) | multi-argument workers — each thread gets its own id plus a shared accumulator |
| [threads_mutex.vr](threads_mutex.vr) | `Mutex` guarding a read-modify-write critical section (`lock` / `unlock` / `get` / `set`) |
| [threads_parallel_sum.vr](threads_parallel_sum.vr) | fork/join parallel reduction — sum a range across four threads, private compute + one shared fold |

Primitives:

- `spawn worker(args…)` → a thread handle. `join(h)` waits and returns the
  worker's result. One or more arguments (a scalar, or `Atomic`/`Mutex`).
- `Atomic(v)` → `.fetch_add(d)` (returns the previous value), `.load()`.
- `Mutex(v)` → `.lock()`, `.unlock()`, `.get()`, `.set(v)`.

Not yet: `Channel`, `Mutex.lock(closure)`, `parallel_for`/`parallel_map` — see
the repo `TODO.md`.

## Language

| File | Shows |
|---|---|
| [generics.vr](generics.vr) | bounded generics `[T: Shape]` with static (inlined) trait dispatch; the bound is enforced |
| [collections.vr](collections.vr) | growable `list()`, hash `map()`/`set()`, `Str` methods |
| [iterators.vr](iterators.vr) | `fold`/`sum`/`map`/`filter`/`each` over ranges & lists |
| [compile_time.vr](compile_time.vr) | `const`/`comptime`, `@derive`, and a hygienic item macro |
| [inferred.vr](inferred.vr) | type inference — no annotation on any local or return, one parameter fully inferred from use |
| [object_graph.vr](object_graph.vr) | `type` objects with references, built + traversed recursively; heap balances to 0 live (RC/ownership proven) |

## Graphics (`@vulkan`)

Vire-authored shaders (`@vertex`/`@fragment`/`@mesh`/`@task`/`@compute`) → SPIR-V, with
the descriptor/pipeline layout derived from the shader signatures. Need a Vulkan device +
`spirv-as`. See [language/GPU-VULKAN.md](../../language/GPU-VULKAN.md) and the many
`vulkan_*.vr` files.

| File | Shows |
|---|---|
| [vulkan_draw.vr](vulkan_draw.vr) | the generic `vk_draw(verts, uniform)` surface — program geometry + uniform through the program's own shaders |
| [sphere.vr](sphere.vr) | **a complete renderer: a rotating, Lambert-shaded sphere.** Vire does the geometry + 3D rotation + lighting; Vulkan rasterizes; each frame is written to `frame_NNN.ppm`. Run it, then `convert -delay 4 -loop 0 frame_*.ppm sphere.gif` to watch it spin |
| [sphere3d.vr](sphere3d.vr) | the same sphere, **GPU edition** — 3D vertex input (`x,y,z`) + a `@vertex` that rotates and lights the mesh on the GPU. The geometry is built once; only the angle changes per frame. Back-face culling hides the far side (no depth buffer). ~2× faster than the CPU version |
