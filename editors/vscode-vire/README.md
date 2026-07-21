# Vire for VS Code

Language support and native debugging for the [Vire](../../README.md) programming
language (`.vr`).

## Features

- **Syntax highlighting** — keywords, types, `@gpu`/`@derive` attributes, strings,
  numbers, operators, function definitions/calls (TextMate grammar).
- **Diagnostics** — type/parse/lowering errors as you save, via `vire check`
  (red squiggles in the editor + Problems panel). Toggle with `vire.checkOnSave`.
- **Native debugging** — set breakpoints in `.vr` files, step, inspect the call
  stack **and local variables**. The extension compiles the file with `--debug`
  (DWARF, incl. `DILocalVariable` for locals) and drives
  [`lldb-dap`](https://lldb.llvm.org/).
- **Snippets** — `fn`, `main`, `gpu`, `type`, `enum`, `match`, `while`, `for`,
  `trait`, `impl`.
- **Commands** — `Vire: Build File`, `Vire: Run File`.

## Requirements

- The **`vire` compiler** on `PATH`, or set `vire.path` to its location
  (e.g. `${workspaceFolder}/target/release/vire`).
- For debugging: **`lldb-dap`** (ships with LLVM/lldb) on `PATH`, and the same
  toolchain `vire build` needs (clang).

## Setup (from this repo, unpackaged)

```sh
cargo build --release -p vire          # build the compiler
# then point VS Code at the extension folder:
code --extensionDevelopmentPath=editors/vscode-vire   # or symlink into ~/.vscode/extensions
```

Set the compiler path in Settings (`vire.path`) if `vire` is not on `PATH`.

## Debugging

Open a `.vr` file and press **F5**. With no `launch.json`, the extension debugs
the active file. Or add a config:

```json
{
    "type": "vire",
    "request": "launch",
    "name": "Vire: Debug current file",
    "program": "${file}",
    "stopOnEntry": false
}
```

Breakpoints, stepping (over/into/out), the call stack, and **local variables +
parameters** work. Note: debug builds compile at `-O0` (no LTO/inlining) so line
info and variables stay precise; small functions are still not inlined there, so
you can step into helpers.

## Packaging

```sh
npm install -g @vscode/vsce
cd editors/vscode-vire && vsce package      # produces vire-0.1.0.vsix
```

License: GPL-3.0-or-later (same as the Vire toolchain).
