# Licensing

This repository is under **two licenses**, split by directory:

| Part | License | Text |
|---|---|---|
| **CSolver** — everything under [`crates/csolver/`](crates/csolver/) (the vendored memory-safety verifier: `csolver-*` crates) | **Apache License 2.0** | [`crates/csolver/LICENSE`](crates/csolver/LICENSE) |
| **cuda-oxide** — [`crates/cuda-oxide/`](crates/cuda-oxide/): the full upstream source is vendored under its own license (kept for reference/attribution of the `@gpu` design; `exclude`d from the build, never compiled) | **Apache License 2.0** | [`crates/cuda-oxide/LICENSE`](crates/cuda-oxide/LICENSE) |
| **Everything else** — the Vire front-end, shared IR/solver/backend, the Java driver, runtime (incl. the original GPU launch runtime + NVPTX emitter), benchmarks, docs, and build integration | **GNU GPL v3.0 or later** | [`LICENSE`](LICENSE) |

## What this means

- **`crates/csolver/**`** may be used, modified, and redistributed under the terms of
  the **Apache-2.0** license — including in proprietary and non-GPL projects — subject to
  its attribution and patent-grant terms.
- **All other files** in this repository are licensed under the **GPL-3.0-or-later**:
  redistribution and derivative works must remain GPL-compatible and provide source.

## Boundary

The dividing line is the `crates/csolver/` directory. A file's license is determined by
whether its path is inside `crates/csolver/` (Apache-2.0) or not (GPL-3.0-or-later),
regardless of the crate-name prefix. This is also reflected in each crate's `Cargo.toml`
`license` field:

- `csolver-*` crates inherit `license = "Apache-2.0"` from `[workspace.package]`.
- `vire`, `fastllvm-*` crates set `license = "GPL-3.0-or-later"` explicitly.

## Combined use

The default build links the GPL front-end/backend with the Apache-2.0 CSolver verifier.
Apache-2.0 is a permissive license and is one-way compatible with GPL-3.0: the combined
work as distributed here is covered by the GPL-3.0-or-later, while the CSolver portion
remains independently available under Apache-2.0 for reuse elsewhere.
