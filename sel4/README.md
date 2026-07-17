# Freestanding-/seL4-Ziel

`fastjavac --freestanding -o app.o …` erzeugt ein **relozierbares Objekt ohne
libc**: die Runtime nutzt einen statischen Heap-Allokator und schreibt Ausgabe
über zwei schwache Hooks, die die Zielumgebung bereitstellt:

```c
void jrt_debug_putchar(char c);   /* ein Byte ausgeben (z.B. seL4_DebugPutChar) */
void jrt_platform_halt(void);      /* Prozess/Thread beenden — kehrt nie zurück */
```

Ohne eigene Definition greifen schwache Defaults (putchar = no-op, halt =
Endlosschleife). Der Einstiegspunkt der übersetzten Klasse ist `int main(void)`.

Heapgröße per `-DFASTLLVM_HEAP_SIZE=<bytes>` überschreibbar (Default 16 MiB);
der Heap ist ein statisches `.bss`-Array, es gibt keine `brk`/`mmap`-Aufrufe.

## Bring-up ohne seL4 (Beweis der libc-Freiheit)

`bringup.c` liefert die Hooks + einen `_start` über rohe Linux-Syscalls und
linkt statisch ohne C-Bibliothek:

```sh
fastjavac --freestanding -o app.o App.class
clang -nostdlib -static -fno-stack-protector -ffreestanding bringup.c app.o -o app
./app          # läuft ohne jede libc-Abhängigkeit (ldd: nicht dynamisch)
```

## Einbettung in seL4

`jrt_debug_putchar` auf `seL4_DebugPutChar` (Debug-Kernel) bzw. den seriellen
Treiber abbilden, `jrt_platform_halt` auf `seL4_TCB_Suspend`/Endlos-`seL4_Yield`.
Das `app.o` wird wie ein gewöhnliches Objekt in das Root-Task-Image gelinkt.
Der Allokator lebt komplett im statischen Heap — kein Untypeds-Retyping nötig,
solange `FASTLLVM_HEAP_SIZE` reicht.
