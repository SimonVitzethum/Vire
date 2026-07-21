# cuda-oxide — attribution (Apache-2.0)

Vire's `@gpu` kernel feature is a **design adaptation** of
[NVlabs/cuda-oxide](https://github.com/NVlabs/cuda-oxide), an experimental
"single-source" Rust→CUDA compiler, licensed under the **Apache License 2.0**
(full text in [`LICENSE`](LICENSE)).

## What was adapted (the *idea*, not the code)

cuda-oxide's core idea is single-source GPU programming: you write kernels in
the *same* high-level language, and the compiler lowers them through **LLVM IR
to PTX**, embeds the PTX in the host binary, and provides safe host-side buffer
management and a typed launch. Vire already owns an equivalent pipeline
(Vire source → shared IR → LLVM IR → codegen), so `@gpu` re-uses it: a
`@gpu` function is lowered to an `nvptx64` LLVM module, compiled to PTX with
`llc`, embedded in the binary, and launched at runtime via the CUDA Driver API.

The following cuda-oxide concepts have direct Vire counterparts:

| cuda-oxide | Vire `@gpu` |
|---|---|
| `#[kernel]` attribute | `@gpu` function attribute |
| `thread::index_1d()` | `gpu_gid()` intrinsic |
| `DeviceBuffer::from_host` / `zeroed` | runtime H2D upload / D2H copyback of array args |
| `LaunchConfig::for_num_elems(N)` | launch convention: first int param = thread count N |
| Rust→MIR→…→LLVM IR→PTX | Vire→IR→LLVM IR→PTX (`llc -march=nvptx64`) |

## Source

**No cuda-oxide source code is compiled into or redistributed by this
repository.** cuda-oxide is a `rustc` codegen backend and cannot be used as a
library by Vire's C-hosted runtime; only its *design* is referenced. This
directory preserves the upstream Apache-2.0 license text as attribution for
that design reuse, per the courtesy of the Apache-2.0 NOTICE convention.

The Vire GPU implementation itself (`crates/backend` NVPTX emitter + C launch
stubs, `crates/driver/src/gpu_runtime.c`, `crates/vire` `@gpu` lowering) is
original work under this repository's GPL-3.0-or-later license.
