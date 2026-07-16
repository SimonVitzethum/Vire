/* fastllvm Mini-Runtime (hosted-Variante).
 *
 * Bewusst klein (DESIGN.md §6): println-Intrinsics, Java-Semantik-Helfer
 * für idiv/irem und die Referenzzählung (Stufe 4, DESIGN.md §6/§7). Die
 * no_std/seL4-Variante ersetzt stdio/malloc durch die dortigen Primitive.
 *
 * Objekt-Speicherlayout (vom Backend erzeugt):
 *   { int64_t refcount; int64_t rcflags; void *vtable; <felder…> }
 * refcount < 0 ⇒ "immortal" (Stack-Objekte, String-/Class-Literale):
 * retain/release sind No-Ops, es wird nie freigegeben, der Collector
 * fasst sie nie an. rcflags trägt Farbe + Buffered-Bit für den
 * Zyklen-Collector. vtable[0] = Drop-Funktion (released Ref-Felder),
 * vtable[1] = Trace-Funktion (besucht Ref-Felder mit einem Callback).
 *
 * Speicherverwaltung: Referenzzählung + synchroner Zyklen-Collector nach
 * Bacon & Rajan, "Concurrent Cycle Collection in Reference Counted
 * Systems" (2001), Abschnitt 3 (synchrone Variante).
 */
#include <stddef.h>
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

/* --- Referenzzählung + Zyklen-Collector ------------------------------ */

typedef struct {
    int64_t refcount;
    int64_t rcflags; /* Bits 0-1: Farbe; Bit 2: buffered */
    void *vtable;    /* [0]=drop(obj), [1]=trace(obj, visit) */
} JObjHeader;

/* Farben nach Bacon-Rajan. */
enum { COL_BLACK = 0, COL_GRAY = 1, COL_WHITE = 2, COL_PURPLE = 3 };

#define COLOR(h)        ((int)((h)->rcflags & 3))
#define SET_COLOR(h, c) ((h)->rcflags = ((h)->rcflags & ~(int64_t)3) | (c))
#define BUFFERED(h)     (((h)->rcflags >> 2) & 1)
#define SET_BUFFERED(h, b) \
    ((h)->rcflags = ((h)->rcflags & ~(int64_t)4) | ((int64_t)(b) << 2))

typedef void (*trace_fn)(void *, void (*)(void *));

static trace_fn trace_of(JObjHeader *h) {
    void (**vt)(void) = (void (**)(void))h->vtable;
    return vt ? (trace_fn)vt[1] : NULL;
}
static void run_drop(JObjHeader *h) {
    void (**vt)(void *) = (void (**)(void *))h->vtable;
    if (vt && vt[0]) vt[0]((void *)h);
}

/* Bilanz: bei ausgeglichenem Refcounting muss live_objects am Ende 0 sein
 * (auch bei Zyklen — der Collector räumt sie). Mit FASTLLVM_HEAPSTATS wird
 * die Bilanz bei Prozessende gedruckt. */
static int64_t total_allocated = 0;
static int64_t live_objects = 0;

/* Kandidaten-Wurzeln für die Zyklensuche (purple-Objekte). */
static JObjHeader **roots = NULL;
static size_t roots_len = 0, roots_cap = 0;
#define ROOTS_THRESHOLD 10000

static void jrt_collect_cycles(void);

static void free_obj(JObjHeader *h) {
    free(h);
    live_objects--;
}

static void roots_push(JObjHeader *h) {
    if (roots_len == roots_cap) {
        roots_cap = roots_cap ? roots_cap * 2 : 64;
        roots = (JObjHeader **)realloc(roots, roots_cap * sizeof(*roots));
    }
    roots[roots_len++] = h;
}

static void possible_root(JObjHeader *h) {
    if (COLOR(h) != COL_PURPLE) {
        SET_COLOR(h, COL_PURPLE);
        if (!BUFFERED(h)) {
            SET_BUFFERED(h, 1);
            roots_push(h);
            if (roots_len >= ROOTS_THRESHOLD) jrt_collect_cycles();
        }
    }
}

static void jrt_shutdown(void) {
    jrt_collect_cycles();
    if (getenv("FASTLLVM_HEAPSTATS")) {
        fprintf(stderr, "[heap] %lld alloziert, %lld noch live (Zyklen-Leak)\n",
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
        atexit(jrt_shutdown);
    }
    live_objects++;
    ((JObjHeader *)p)->refcount = 1; /* der Erzeuger hält die erste Referenz */
    return p;
}

void jrt_retain(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (h->refcount < 0) return; /* immortal */
    h->refcount++;
    SET_COLOR(h, COL_BLACK);
}

void jrt_release(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (h->refcount < 0) return; /* immortal */
    if (--h->refcount == 0) {
        /* Release: Kinder dekrementieren (drop), dann ggf. freigeben.
         * Ein noch gepuffertes Objekt bleibt liegen — der Collector holt
         * es in MarkRoots ab (color black, rc 0). */
        run_drop(h);
        SET_COLOR(h, COL_BLACK);
        if (!BUFFERED(h)) free_obj(h);
    } else {
        possible_root(h);
    }
}

/* --- Bacon-Rajan: MarkRoots / ScanRoots / CollectRoots --------------- */

static void mark_gray(JObjHeader *h);
static void scan(JObjHeader *h);
static void scan_black(JObjHeader *h);
static void collect_white(JObjHeader *h);

static void visit_mark_gray(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (h->refcount < 0) return; /* immortal: nicht antasten */
    h->refcount--;
    mark_gray(h);
}
static void mark_gray(JObjHeader *h) {
    if (COLOR(h) == COL_GRAY) return;
    SET_COLOR(h, COL_GRAY);
    trace_fn t = trace_of(h);
    if (t) t(h, visit_mark_gray);
}

static void visit_scan(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (h->refcount < 0) return;
    scan(h);
}
static void scan(JObjHeader *h) {
    if (COLOR(h) != COL_GRAY) return;
    if (h->refcount > 0) {
        scan_black(h);
    } else {
        SET_COLOR(h, COL_WHITE);
        trace_fn t = trace_of(h);
        if (t) t(h, visit_scan);
    }
}

static void visit_scan_black(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (h->refcount < 0) return;
    h->refcount++;
    if (COLOR(h) != COL_BLACK) scan_black(h);
}
static void scan_black(JObjHeader *h) {
    SET_COLOR(h, COL_BLACK);
    trace_fn t = trace_of(h);
    if (t) t(h, visit_scan_black);
}

static void visit_collect_white(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (h->refcount < 0) return;
    collect_white(h);
}
static void collect_white(JObjHeader *h) {
    if (COLOR(h) == COL_WHITE && !BUFFERED(h)) {
        SET_COLOR(h, COL_BLACK);
        trace_fn t = trace_of(h);
        if (t) t(h, visit_collect_white);
        free_obj(h);
    }
}

static void jrt_collect_cycles(void) {
    /* MarkRoots: purple mit rc>0 grau markieren; Rest aus dem Buffer. */
    size_t kept = 0;
    for (size_t i = 0; i < roots_len; i++) {
        JObjHeader *h = roots[i];
        if (COLOR(h) == COL_PURPLE && h->refcount > 0) {
            mark_gray(h);
            roots[kept++] = h;
        } else {
            SET_BUFFERED(h, 0);
            if (COLOR(h) == COL_BLACK && h->refcount == 0) free_obj(h);
        }
    }
    roots_len = kept;

    /* ScanRoots. */
    for (size_t i = 0; i < roots_len; i++) scan(roots[i]);

    /* CollectRoots: Buffer leeren, weiße Zyklen einsammeln. */
    for (size_t i = 0; i < roots_len; i++) {
        JObjHeader *h = roots[i];
        SET_BUFFERED(h, 0);
        collect_white(h);
    }
    roots_len = 0;
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
