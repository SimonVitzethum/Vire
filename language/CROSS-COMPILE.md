# Cross-compiling Vire (macOS / Windows / BSD)

A whole `.vr` program lowers to **portable C** (the runtime) + **LLVM IR** (the
program), which `clang -target <triple>` compiles for another OS. How far each
target gets depends only on whether the target's **toolchain (linker + libc /
SDK)** is installed — the Vire runtime itself is portable C (C11 `aligned_alloc`
with a `_WIN32` `_aligned_malloc` branch, a POSIX-`mmap` region allocator with a
`malloc` fallback for non-POSIX, `pthreads`, and a `_WIN32` time branch using
`QueryPerformanceCounter`/`FILETIME`).

Use `vire build --target <triple>`. Cross builds automatically use LLD
(`-fuse-ld=lld`) so `-flto` works (the target's default linker can't consume LLVM
bitcode).

## Status matrix (measured from an x86-64 Linux host)

| Target | Triple | Result | Needs |
|---|---|---|---|
| **Windows** | `x86_64-pc-windows-gnu` | ✅ **Working `.exe`** — builds, runs (verified under wine: `fib(20)=6765`) | MinGW-w64 (`x86_64-w64-mingw32`) + LLD (both bundled with most clang installs) |
| **FreeBSD / BSD** | `x86_64-unknown-freebsd` | ⚠️ **Compiles to a valid object**; linking an executable needs a FreeBSD sysroot | a FreeBSD sysroot (`--sysroot`), or build the object here and link on the target |
| **macOS** | `arm64-apple-macos11`, `x86_64-apple-darwin` | ❌ **Needs the macOS SDK** — without it clang falls back to the host's Linux headers and can't even compile `runtime.c` | the macOS SDK via [osxcross](https://github.com/tpoechtrager/osxcross) (Apple's SDK isn't redistributable) |

The blocker for macOS/BSD is **not** Vire code — it is the missing platform SDK /
sysroot, which cannot legally or practically ship with the compiler. On a machine
that has them (or on the target OS itself), the same `vire build --target` line
produces a native binary.

## Windows — the working path

```console
$ vire build --target x86_64-pc-windows-gnu hello.vr -o hello.exe
$ wine hello.exe          # or copy to Windows
6765
```

- Single-threaded programs: fully working.
- `--threads`: links against MinGW's winpthreads and produces a `.exe` (execution
  under wine is flaky for threads — verify on real Windows).
- `@gpu` kernels are **Linux + NVIDIA only** (they need `libcuda`/`llc` NVPTX);
  a Windows build of a GPU program will fail at the CUDA link step.

## BSD — object now, link on target

```console
$ vire build --emit=obj --target x86_64-unknown-freebsd prog.vr -o prog.o
# copy prog.o to FreeBSD and link there, or link here with a sysroot:
$ vire build --target x86_64-unknown-freebsd --sysroot /path/to/freebsd prog.vr -o prog
```

`@when(freebsd)` / `@when(unix)` conditional compilation resolves correctly for
BSD triples (the unix family = linux/macos/freebsd).

## Conditional compilation

Provide per-platform definitions with `@when`:

```vire
@when(windows) fn sep() -> Str { "\\" }
@when(unix)    fn sep() -> Str { "/" }
```

Exactly one survives per target (see `crates/vire/src/platform.rs`).
