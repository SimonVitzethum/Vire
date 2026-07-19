# Vire ↔ Meson interop

Vire is whole-program and closed-world, so a `.vr` program lowers to **one relocatable
object** that exposes C-ABI symbols (the runtime `main` is included). Meson links that
object exactly like any C/C++/Rust object — that is the whole integration. Two ways to
use it, both on the same stable CLI (`vire build --emit=…`).

## 1. Stock Meson DSL (recommended — no install, version-robust)

This is the tested path ([`example/`](example/) builds and runs it). It uses only
`find_program` + `custom_target` + `executable`:

```meson
project('app', 'c', meson_version: '>=1.1.0')
vire = find_program('vire')

main_obj = custom_target('main.vr.o',
  input: 'main.vr',
  output: 'main.vr.o',
  command: [vire, 'build', '@INPUT@', '--emit=obj', '-o', '@OUTPUT@', '--deps', '@DEPFILE@'],
  depfile: 'main.vr.o.d',            # incremental: rebuilds on source/header change
)

executable('app', 'util.c', objects: main_obj, link_args: ['-lm'])
```

```console
$ cd example && meson setup builddir && ninja -C builddir && ./builddir/app
42
```

The example links a Vire object with a plain C object (`util.c`) — Vire's `main` calls a
C function resolved at link time, proving real cross-language linking. `-lm` is needed
because the Vire runtime uses libm math intrinsics; add `-pthread` if the program uses
`spawn`, and pkg-config libs via `--pkg` (below).

## 2. `import('vire')` module (optional ergonomic layer)

[`vire.py`](vire.py) wraps the same CLI so you can write:

```meson
vire = import('vire')
vire.executable('app', 'main.vr', c_sources: ['util.c'], pkg: ['zlib'])
```

Install by copying `vire.py` into your Meson's module directory:

```console
$ python3 -c "import mesonbuild.modules,os; print(os.path.dirname(mesonbuild.modules.__file__))"
```

Meson's module ABI shifts between releases; if it mismatches your Meson, use pattern 1
(version-robust and tested).

## The CLI these build on

| Flag | Meaning |
|---|---|
| `--emit=obj` | whole `.vr` program → one relocatable `.o` (C-ABI, includes `main`) |
| `--emit=staticlib` | → a `.a` archive |
| `--emit=asm` | program IR → assembly (inspection) |
| `--emit=llvm` / `--emit=ir` | LLVM IR / mid-level IR to stdout |
| `--deps FILE` | Makefile/Ninja depfile → Meson `depfile:` (incremental) |
| `-I DIR` | include path for `native "c"` blocks / headers |
| `--pkg NAME` | pull cflags+libs from pkg-config (first-class dependency use) |

All are additive to the normal `vire build` (same solver, same `-O2 -march=native`
codegen) — the emitted object behaves identically to a `vire build` executable, fully
memory-safe.
