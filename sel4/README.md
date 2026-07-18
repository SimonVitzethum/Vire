# Freestanding/seL4 target

`fastjavac --freestanding -o app.o …` produces a **relocatable object without
libc**: the runtime uses a static heap allocator and writes output
via two weak hooks that the target environment provides:

```c
void jrt_debug_putchar(char c);   /* emit one byte (e.g. seL4_DebugPutChar) */
void jrt_platform_halt(void);      /* terminate process/thread — never returns */
```

Without a custom definition, weak defaults take effect (putchar = no-op, halt =
infinite loop). The entry point of the compiled class is `int main(void)`.

Heap size overridable via `-DFASTLLVM_HEAP_SIZE=<bytes>` (default 16 MiB);
the heap is a static `.bss` array, there are no `brk`/`mmap` calls.

## Bring-up without seL4 (proof of libc-freedom)

`bringup.c` provides the hooks + a `_start` via raw Linux syscalls and
links statically without a C library:

```sh
fastjavac --freestanding -o app.o App.class
clang -nostdlib -static -fno-stack-protector -ffreestanding bringup.c app.o -o app
./app          # runs without any libc dependency (ldd: not dynamic)
```

## Embedding into seL4

Map `jrt_debug_putchar` to `seL4_DebugPutChar` (debug kernel) or the serial
driver, and `jrt_platform_halt` to `seL4_TCB_Suspend`/endless `seL4_Yield`.
The `app.o` is linked into the root task image like an ordinary object.
The allocator lives entirely in the static heap — no untypeds retyping needed,
as long as `FASTLLVM_HEAP_SIZE` is sufficient.
