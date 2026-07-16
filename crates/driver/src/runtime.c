/* fastllvm Mini-Runtime (hosted-Variante).
 *
 * Bewusst klein (DESIGN.md §6): println-Intrinsics, Java-Semantik-Helfer
 * für idiv/irem und die Referenzzählung (Stufe 4, DESIGN.md §6/§7). Die
 * no_std/seL4-Variante ersetzt stdio/malloc durch die dortigen Primitive.
 *
 * Objekt-Speicherlayout (vom Backend erzeugt):
 *   { int64_t refcount; void *vtable; <felder…> }
 * refcount < 0 ⇒ "immortal" (Stack-Objekte, String-/Class-Literale):
 * retain/release sind No-Ops, es wird nie freigegeben.
 * vtable[0] ist die Drop-Funktion der Klasse (released Ref-Felder).
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

/* --- Ausgabe --------------------------------------------------------- */

/* String-Literal: immortaler Header + Länge + Bytes (UTF-8). */
typedef struct {
    int64_t refcount;
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

/* --- Referenzzählung ------------------------------------------------- */

typedef struct {
    int64_t refcount;
    void *vtable;
} JObjHeader;

/* Bilanz: bei ausgeglichenem Refcounting muss live_objects am Ende 0
 * sein (außer bei Zyklen — die leaken bewusst, DESIGN.md §6). Mit
 * FASTLLVM_HEAPSTATS wird die Bilanz bei Prozessende gedruckt. */
static int64_t total_allocated = 0;
static int64_t live_objects = 0;

static void heap_report(void) {
    if (getenv("FASTLLVM_HEAPSTATS")) {
        fprintf(stderr, "[heap] %lld alloziert, %lld noch live (Leak/Zyklen)\n",
                (long long)total_allocated, (long long)live_objects);
    }
}

void *jrt_alloc(int64_t size) {
    void *p = calloc(1, (size_t)size);
    if (!p) {
        fputs("Exception in thread \"main\" java.lang.OutOfMemoryError\n", stderr);
        exit(1);
    }
    if (total_allocated++ == 0) {
        atexit(heap_report);
    }
    live_objects++;
    ((JObjHeader *)p)->refcount = 1; /* der Erzeuger hält die erste Referenz */
    return p;
}

void jrt_retain(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (h->refcount >= 0) {
        h->refcount++;
    }
}

void jrt_release(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (h->refcount < 0) return; /* immortal */
    if (--h->refcount == 0) {
        /* Drop-Funktion der Klasse released die eigenen Ref-Felder,
         * dann geben wir den Speicher frei. */
        void (**vt)(void *) = (void (**)(void *))h->vtable;
        if (vt && vt[0]) {
            vt[0](p);
        }
        free(p);
        live_objects--;
    }
}

void jrt_null_check(const void *p) {
    if (!p) {
        fputs("Exception in thread \"main\" java.lang.NullPointerException\n", stderr);
        exit(1);
    }
}

/* --- Java-Arithmetik-Semantik ---------------------------------------- */

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
