/* Minimaler bare-metal-Ersatz für seL4: liefert die schwachen Runtime-Hooks
 * und _start über rohe Linux-Syscalls — keine libc. Beweist, dass die
 * freestanding-Runtime ohne C-Bibliothek läuft. */
#include <stddef.h>
static long sys_write(long fd, const void *b, long n){long r;__asm__ volatile("syscall":"=a"(r):"a"(1L),"D"(fd),"S"(b),"d"(n):"rcx","r11","memory");return r;}
static void sys_exit(long c){__asm__ volatile("syscall"::"a"(60L),"D"(c):"rcx","r11","memory");__builtin_unreachable();}
void jrt_debug_putchar(char c){ sys_write(1,&c,1); }
void jrt_platform_halt(void){ sys_exit(0); }
int main(void);
void _start(void){ main(); sys_exit(0); }
