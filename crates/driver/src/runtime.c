/* fastllvm Mini-Runtime (hosted-Variante).
 *
 * Bewusst klein (DESIGN.md §6): println-Intrinsics und die
 * Java-Semantik-Helfer für idiv/irem. Die no_std/seL4-Variante ersetzt
 * stdio durch die dortige Debug-Konsole.
 *
 * Java-String-Literale liegen als { int64_t len; uint8_t bytes[] } vor.
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct {
    int64_t len;
    uint8_t bytes[];
} JStr;

void jrt_print_str(const JStr *s) {
    fwrite(s->bytes, 1, (size_t)s->len, stdout);
}

void jrt_println_str(const JStr *s) {
    jrt_print_str(s);
    fputc('\n', stdout);
}

void jrt_print_int(int32_t v) {
    printf("%d", v);
}

void jrt_println_int(int32_t v) {
    printf("%d\n", v);
}

void jrt_println_ln(void) {
    fputc('\n', stdout);
}

/* Objektallokation: genullt (Java-Defaultwerte für Felder).
 * Stufe 2: kein GC — Objekte leben bis Prozessende; Referenzzählung
 * kommt in Stufe 4 (DESIGN.md §7). */
void *jrt_alloc(int64_t size) {
    void *p = calloc(1, (size_t)size);
    if (!p) {
        fputs("Exception in thread \"main\" java.lang.OutOfMemoryError\n", stderr);
        exit(1);
    }
    return p;
}

void jrt_null_check(const void *p) {
    if (!p) {
        fputs("Exception in thread \"main\" java.lang.NullPointerException\n", stderr);
        exit(1);
    }
}

/* JLS 15.17.2: Division durch 0 wirft ArithmeticException;
 * INT_MIN / -1 ist definiert als INT_MIN (in C wäre beides UB). */
int32_t jrt_idiv(int32_t a, int32_t b) {
    if (b == 0) {
        fputs("Exception in thread \"main\" java.lang.ArithmeticException: / by zero\n", stderr);
        exit(1);
    }
    if (a == INT32_MIN && b == -1)
        return INT32_MIN;
    return a / b;
}

int32_t jrt_irem(int32_t a, int32_t b) {
    if (b == 0) {
        fputs("Exception in thread \"main\" java.lang.ArithmeticException: / by zero\n", stderr);
        exit(1);
    }
    if (a == INT32_MIN && b == -1)
        return 0;
    return a % b;
}
