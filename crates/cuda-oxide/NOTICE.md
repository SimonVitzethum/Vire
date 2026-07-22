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

Only the **idea** was adapted — **no cuda-oxide code is copied, compiled, or
linked** by any Vire artifact. Accordingly, the bulky upstream source tree is
**not tracked in this repository**: it is `.gitignore`d (only this `NOTICE.md`,
the `LICENSE` text, and `update.sh` are tracked) and, when present locally, is
`exclude`d from the workspace `Cargo.toml` (`exclude = ["crates/cuda-oxide"]`)
so `cargo` never descends into it. cuda-oxide is a `rustc` codegen backend (its
own workspace + pinned toolchain) and cannot be used as a library by Vire's
C-hosted runtime; only its *design* is adapted.

To obtain the full upstream tree locally (reference only), run
`sh crates/cuda-oxide/update.sh`, which clones NVlabs/cuda-oxide into this
directory under its Apache-2.0 license (see [`LICENSE`](LICENSE)).

The Vire GPU implementation itself (`crates/backend` NVPTX emitter + C launch
stubs, `crates/driver/src/gpu_runtime.c`, `crates/vire` `@gpu` lowering) is
original work under this repository's GPL-3.0-or-later license.
