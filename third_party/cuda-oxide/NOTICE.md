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

The **full upstream cuda-oxide source tree is vendored in this directory**
(everything except `.git`), redistributed under its Apache-2.0 license (see
[`LICENSE`](LICENSE)). It is kept for **reference and attribution only** and is
**not built**: the repository's workspace `Cargo.toml` lists
`exclude = ["third_party/cuda-oxide"]`, so `cargo` never descends into it, and
none of its code is compiled into or linked by any Vire artifact. cuda-oxide is
a `rustc` codegen backend (with its own workspace and pinned toolchain) and
cannot be used as a library by Vire's C-hosted runtime; only its *design* is
adapted.

The Vire GPU implementation itself (`crates/backend` NVPTX emitter + C launch
stubs, `crates/driver/src/gpu_runtime.c`, `crates/vire` `@gpu` lowering) is
original work under this repository's GPL-3.0-or-later license.
