# Vire for VS Code

Language support and native debugging for the **Vire** programming language
(`.vr`).

## Features

- **Syntax highlighting** — keywords, types, `@gpu`/`@derive` attributes, strings,
  numbers, operators, function definitions/calls (TextMate grammar).
- **Language intelligence — no toolchain needed** — diagnostics (type/parse/
  lowering errors as you type), **hover** (function/type signatures),
  **go-to-definition**, and the **outline / document symbols**. These run the
  **bundled WebAssembly build of the Vire frontend** (`wasm/vire-check.wasm`) via
  Node's built-in WASI, so they work on **Windows, macOS, and Linux with nothing
  installed** — no `vire` binary, no clang.
- **Native debugging** — set breakpoints in `.vr` files, step, inspect the call
  stack **and local variables**. The extension compiles the file with `--debug`
  (DWARF, incl. `DILocalVariable` for locals) and drives
  [`lldb-dap`](https://lldb.llvm.org/).
- **Snippets** — `fn`, `main`, `gpu`, `type`, `enum`, `match`, `while`, `for`,
  `trait`, `impl`.
- **Commands** — `Vire: Build File`, `Vire: Run File`.

## Requirements

- **Language features** (highlighting, diagnostics, hover, go-to-definition):
  **nothing** — the frontend ships as WebAssembly inside the extension.
- **Build/Run/Debug** (native codegen): the **`vire` compiler** on `PATH` (or set
  `vire.path`) plus its toolchain (clang), and for debugging **`lldb-dap`**
  (ships with LLVM/lldb). These are inherently platform-native; the language
  intelligence above does not depend on them.

## Setup (from this repo, unpackaged)

```sh
# language features work out of the box (the wasm is committed). To rebuild it:
sh editors/vscode-vire/build-wasm.sh   # needs: rustup target add wasm32-wasip1
# for Build/Run/Debug also build the native compiler:
cargo build --release -p vire
# then point VS Code at the extension folder:
code --extensionDevelopmentPath=editors/vscode-vire   # or symlink into ~/.vscode/extensions
```

Set the compiler path in Settings (`vire.path`) if `vire` is not on `PATH`
(only needed for Build/Run/Debug).

## How the cross-platform frontend works

`crates/vire-wasm` builds the Vire **frontend only** (lex → parse → infer →
lower) to `wasm32-wasip1`, excluding the LLVM backend and the CSolver verifier
(behind the crate's `native` feature — neither is needed for analysis, and
CSolver assumes 64-bit `usize`). The extension feeds source to that wasm over
stdin and reads a JSON `{ diagnostics, symbols }` back — the same frontend the
native compiler uses, so diagnostics match `vire check` exactly.

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

## Packaging & install

```sh
cd editors/vscode-vire
npx @vscode/vsce package            # produces vire-0.1.0.vsix (bundles the wasm)
code --install-extension vire-0.1.0.vsix
```

The `.vsix` is self-contained (grammar, snippets, and the wasm frontend), so on
Windows/macOS/Linux the language features work immediately after install; only
Build/Run/Debug additionally need the native `vire` compiler on `PATH`.

License: GPL-3.0-or-later (same as the Vire toolchain).
