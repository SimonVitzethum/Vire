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
#include <string.h>

void jrt_noop_drop(void *p);
void jrt_noop_trace(void *p, void (*visit)(void *));

/* Vtable für Strings (keine Ref-Felder → No-Op-Drop/Trace). Von den
 * String-Literalen im generierten Code und von zur Laufzeit erzeugten
 * Strings gemeinsam genutzt. */
void (*const jrt_string_vtable[2])(void) = {
    (void (*)(void))jrt_noop_drop,
    (void (*)(void))jrt_noop_trace,
};

/* --- Ausgabe --------------------------------------------------------- */

/* String: voller Objekt-Header (damit Literale und zur Laufzeit erzeugte
 * Strings uniform RC-verwaltet sind), dann Länge + Bytes (UTF-8).
 * Literale sind immortal (refcount -1), konkatenierte Strings nicht. */
typedef struct {
    int64_t refcount;
    int64_t rcflags;
    void *vtable;
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

void jrt_print_char(int32_t c) {
    fputc(c, stdout);
}

void jrt_println_char(int32_t c) {
    fputc(c, stdout);
    fputc('\n', stdout);
}

/* String-Methoden (Byte-/ASCII-Semantik, s. Frontend-Kommentar). */
int32_t jrt_str_length(const JStr *s) {
    if (!s) {
        fputs("Exception in thread \"main\" java.lang.NullPointerException\n", stderr);
        exit(1);
    }
    return (int32_t)s->len;
}

int32_t jrt_str_is_empty(const JStr *s) {
    return jrt_str_length(s) == 0;
}

int32_t jrt_str_char_at(const JStr *s, int32_t i) {
    if (!s) {
        fputs("Exception in thread \"main\" java.lang.NullPointerException\n", stderr);
        exit(1);
    }
    if (i < 0 || i >= s->len) {
        fprintf(stderr,
                "Exception in thread \"main\" "
                "java.lang.StringIndexOutOfBoundsException: index %d, length %lld\n",
                i, (long long)s->len);
        exit(1);
    }
    return (int32_t)s->bytes[i];
}

int32_t jrt_str_equals(const JStr *a, const JStr *b) {
    if (a == b) return 1;
    if (!a || !b || a->len != b->len) return 0;
    for (int64_t i = 0; i < a->len; i++) {
        if (a->bytes[i] != b->bytes[i]) return 0;
    }
    return 1;
}

/* --- String-Konkatenation (invokedynamic makeConcatWithConstants) ----
 * Zur Laufzeit erzeugte Strings; refcount-verwaltet (kein immortal).
 * jrt_alloc (weiter unten definiert) setzt refcount=1 und trackt live. */
void *jrt_alloc(int64_t size);

static JStr *str_alloc(int64_t len) {
    JStr *s = (JStr *)jrt_alloc((int64_t)sizeof(JStr) + len);
    s->vtable = (void *)jrt_string_vtable;
    s->len = len;
    return s;
}

JStr *jrt_str_concat(const JStr *a, const JStr *b) {
    static const uint8_t NUL[4] = {'n', 'u', 'l', 'l'};
    const uint8_t *ba = a ? a->bytes : NUL;
    int64_t la = a ? a->len : 4;
    const uint8_t *bb = b ? b->bytes : NUL;
    int64_t lb = b ? b->len : 4;
    JStr *r = str_alloc(la + lb);
    memcpy(r->bytes, ba, (size_t)la);
    memcpy(r->bytes + la, bb, (size_t)lb);
    return r;
}

static JStr *str_from_buf(const char *buf, int n) {
    JStr *r = str_alloc(n);
    memcpy(r->bytes, buf, (size_t)n);
    return r;
}

JStr *jrt_int_to_str(int32_t v) {
    char buf[16];
    return str_from_buf(buf, snprintf(buf, sizeof buf, "%d", v));
}
JStr *jrt_long_to_str(int64_t v) {
    char buf[24];
    return str_from_buf(buf, snprintf(buf, sizeof buf, "%lld", (long long)v));
}
JStr *jrt_char_to_str(int32_t c) {
    char b = (char)c;
    return str_from_buf(&b, 1);
}
JStr *jrt_bool_to_str(int32_t b) {
    return b ? str_from_buf("true", 4) : str_from_buf("false", 5);
}
/* Java-Double.toString ist der kürzeste rundreisesichere Text; wir nähern
 * mit %g an (dokumentierte Abweichung, DESIGN.md §6). */
JStr *jrt_double_to_str(double d) {
    char buf[32];
    return str_from_buf(buf, snprintf(buf, sizeof buf, "%g", d));
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

/* --- Arrays ---------------------------------------------------------- */

/* Gleicher Header wie Objekte, dann die Länge; Elemente folgen direkt. */
typedef struct {
    int64_t refcount;
    int64_t rcflags;
    void *vtable;
    int64_t length;
} JArray;

void *jrt_alloc_array(int64_t count, int64_t elem_size, void *vtable) {
    if (count < 0) {
        fputs("Exception in thread \"main\" java.lang.NegativeArraySizeException\n", stderr);
        exit(1);
    }
    void *p = calloc(1, sizeof(JArray) + (size_t)count * (size_t)elem_size);
    if (!p) {
        fputs("Exception in thread \"main\" java.lang.OutOfMemoryError\n", stderr);
        exit(1);
    }
    if (total_allocated++ == 0) {
        atexit(jrt_shutdown);
    }
    live_objects++;
    JArray *a = (JArray *)p;
    a->refcount = 1;
    a->vtable = vtable;
    a->length = count;
    return p;
}

void jrt_bounds_check(const void *arr, int32_t index) {
    int64_t len = ((const JArray *)arr)->length;
    if (index < 0 || index >= len) {
        fprintf(stderr,
                "Exception in thread \"main\" "
                "java.lang.ArrayIndexOutOfBoundsException: Index %d out of bounds for length %lld\n",
                index, (long long)len);
        exit(1);
    }
}

/* Drop/Trace für ref[]: über die Elemente laufen. */
void jrt_array_ref_drop(void *p) {
    JArray *a = (JArray *)p;
    void **elems = (void **)(a + 1);
    for (int64_t i = 0; i < a->length; i++) {
        jrt_release(elems[i]);
    }
}
void jrt_array_ref_trace(void *p, void (*visit)(void *)) {
    JArray *a = (JArray *)p;
    void **elems = (void **)(a + 1);
    for (int64_t i = 0; i < a->length; i++) {
        visit(elems[i]);
    }
}

/* int[] hat keine Ref-Elemente. */
void jrt_noop_drop(void *p) { (void)p; }
void jrt_noop_trace(void *p, void (*visit)(void *)) { (void)p; (void)visit; }

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

/* --- long/double --------------------------------------------------- */

int64_t jrt_ldiv(int64_t a, int64_t b) {
    if (b == 0) {
        fputs("Exception in thread \"main\" java.lang.ArithmeticException: / by zero\n", stderr);
        exit(1);
    }
    if (a == INT64_MIN && b == -1)
        return INT64_MIN;
    return a / b;
}

int64_t jrt_lrem(int64_t a, int64_t b) {
    if (b == 0) {
        fputs("Exception in thread \"main\" java.lang.ArithmeticException: / by zero\n", stderr);
        exit(1);
    }
    if (a == INT64_MIN && b == -1)
        return 0;
    return a % b;
}

/* lcmp: -1/0/1. */
int32_t jrt_lcmp(int64_t a, int64_t b) {
    return (a > b) - (a < b);
}

/* dcmpl/dcmpg unterscheiden sich nur bei NaN (JVMS 6.5): dcmpl → -1,
 * dcmpg → 1, damit NaN-Vergleiche stets "falsch" liefern. */
int32_t jrt_dcmpl(double a, double b) {
    if (a < b) return -1;
    if (a > b) return 1;
    if (a == b) return 0;
    return -1; /* mindestens ein NaN */
}
int32_t jrt_dcmpg(double a, double b) {
    if (a < b) return -1;
    if (a > b) return 1;
    if (a == b) return 0;
    return 1; /* mindestens ein NaN */
}

/* d2i/d2l saturieren (JLS 5.1.3): NaN → 0, außerhalb des Bereichs auf
 * MIN/MAX geklemmt. */
int32_t jrt_d2i(double d) {
    if (d != d) return 0;
    if (d >= 2147483647.0) return INT32_MAX;
    if (d <= -2147483648.0) return INT32_MIN;
    return (int32_t)d;
}
int64_t jrt_d2l(double d) {
    if (d != d) return 0;
    if (d >= 9223372036854775807.0) return INT64_MAX;
    if (d <= -9223372036854775808.0) return INT64_MIN;
    return (int64_t)d;
}

void jrt_print_long(int64_t v) { printf("%lld", (long long)v); }
void jrt_println_long(int64_t v) { printf("%lld\n", (long long)v); }
/* %g-Näherung; nicht Javas kürzestes rundreisesicheres Format (DESIGN.md §6). */
void jrt_print_double(double d) { printf("%g", d); }
void jrt_println_double(double d) { printf("%g\n", d); }
