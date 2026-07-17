/* fastllvm Mini-Runtime (Plattformschicht: hosted-libc oder freestanding/seL4).
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

/* ====================================================================
 * Plattformschicht — die EINZIGE Stelle mit OS-/libc-Abhängigkeiten.
 * Hosted (Standard): libc. Freestanding (-DFASTLLVM_FREESTANDING, z.B.
 * seL4): statischer Heap + schwache Ausgabe-/Halt-Hooks, keine libc.
 * Der gesamte übrige Runtime-Kern ruft nur plat_, fmt_ und jrt_memcpy.
 * ==================================================================== */

static void *plat_alloc(size_t n);        /* genullter Speicher */
static void *plat_realloc(void *p, size_t n);
static void plat_free(void *p);
static void plat_write(const char *s, size_t n); /* Bytes → stdout/Debug */
static void plat_abort(void);             /* kehrt nicht zurück */

/* Portable Helfer (ohne libc). */
static void jrt_memcpy(void *d, const void *s, size_t n) {
    uint8_t *dd = (uint8_t *)d;
    const uint8_t *ss = (const uint8_t *)s;
    for (size_t i = 0; i < n; i++) dd[i] = ss[i];
}
static size_t jrt_strlen(const char *s) {
    size_t n = 0;
    while (s[n]) n++;
    return n;
}
static void plat_puts(const char *s) { plat_write(s, jrt_strlen(s)); }
/* Uncaught-Meldung: `Exception in thread "main" <msg>\n` ohne printf. */
static void plat_uncaught(const char *msg) {
    plat_puts("Exception in thread \"main\" ");
    plat_puts(msg);
    plat_write("\n", 1);
}

/* Vorzeichenbehaftete Dezimalformatierung nach buf (>=24 Bytes). */
static int fmt_i64(char *buf, int64_t v) {
    char tmp[24];
    int n = 0;
    int neg = v < 0;
    uint64_t u = neg ? (uint64_t)(-(v + 1)) + 1u : (uint64_t)v;
    do {
        tmp[n++] = (char)('0' + (u % 10u));
        u /= 10u;
    } while (u);
    int len = 0;
    if (neg) buf[len++] = '-';
    while (n) buf[len++] = tmp[--n];
    buf[len] = '\0';
    return len;
}

#ifdef FASTLLVM_FREESTANDING
/* -------- Freestanding (seL4): keine libc -------------------------- */
/* Von der Zielumgebung bereitzustellen; schwache Defaults, damit es linkt. */
__attribute__((weak)) void jrt_debug_putchar(char c) { (void)c; }
__attribute__((weak)) void jrt_platform_halt(void) {
    for (;;) {}
}
static void plat_write(const char *s, size_t n) {
    for (size_t i = 0; i < n; i++) jrt_debug_putchar(s[i]);
}
static void plat_abort(void) {
    jrt_platform_halt();
    for (;;) {}
}

/* Minimaler Freelist-Allokator über einen statischen Heap. Blockheader trägt
 * die Nutzgröße; plat_free() hängt Blöcke in eine First-Fit-Liste. Ausreichend für
 * den seL4-Bring-up; produktiv ersetzt die Zielumgebung plat_* durch ihren
 * eigenen Allokator. */
#ifndef FASTLLVM_HEAP_SIZE
#define FASTLLVM_HEAP_SIZE (16u * 1024u * 1024u)
#endif
typedef struct FreeBlock {
    size_t size;
    struct FreeBlock *next;
} FreeBlock;
static uint8_t plat_heap[FASTLLVM_HEAP_SIZE] __attribute__((aligned(16)));
static size_t plat_bump = 0;
static FreeBlock *plat_freelist = NULL;
#define PLAT_HDR ((sizeof(size_t) + 15u) & ~((size_t)15u))
static void *plat_alloc(size_t n) {
    size_t need = (n + 15u) & ~((size_t)15u);
    /* First-Fit in der Freiliste. */
    FreeBlock **pp = &plat_freelist;
    while (*pp) {
        if ((*pp)->size >= need) {
            FreeBlock *b = *pp;
            *pp = b->next;
            uint8_t *payload = (uint8_t *)b + PLAT_HDR;
            for (size_t i = 0; i < b->size; i++) payload[i] = 0;
            return payload;
        }
        pp = &(*pp)->next;
    }
    /* Sonst vom Bump-Zeiger. */
    if (plat_bump + PLAT_HDR + need > FASTLLVM_HEAP_SIZE) return NULL;
    FreeBlock *b = (FreeBlock *)(plat_heap + plat_bump);
    b->size = need;
    plat_bump += PLAT_HDR + need;
    return (uint8_t *)b + PLAT_HDR; /* Heap ist statisch genullt */
}
static void plat_free(void *p) {
    if (!p) return;
    FreeBlock *b = (FreeBlock *)((uint8_t *)p - PLAT_HDR);
    b->next = plat_freelist;
    plat_freelist = b;
}
static void *plat_realloc(void *p, size_t n) {
    if (!p) return plat_alloc(n);
    FreeBlock *b = (FreeBlock *)((uint8_t *)p - PLAT_HDR);
    if (b->size >= n) return p;
    void *q = plat_alloc(n);
    if (q) jrt_memcpy(q, p, b->size);
    plat_free(p);
    return q;
}

/* Minimaler %g-Ersatz: Vorzeichen, Ganzteil, 6 Nachkommastellen. */
static int fmt_g(char *buf, double v) {
    int len = 0;
    if (v < 0) {
        buf[len++] = '-';
        v = -v;
    }
    int64_t ip = (int64_t)v;
    len += fmt_i64(buf + len, ip);
    double frac = v - (double)ip;
    buf[len++] = '.';
    for (int i = 0; i < 6; i++) {
        frac *= 10.0;
        int d = (int)frac;
        if (d < 0) d = 0;
        if (d > 9) d = 9;
        buf[len++] = (char)('0' + d);
        frac -= (double)d;
    }
    buf[len] = '\0';
    return len;
}
static int fmt_spec_i64(char *buf, const char *spec, int64_t v) {
    (void)spec;
    return fmt_i64(buf, v);
}
static int fmt_spec_f(char *buf, const char *spec, double v) {
    (void)spec;
    return fmt_g(buf, v);
}

#else
/* -------- Hosted: libc -------------------------------------------- */
#include <stdio.h>
#include <stdlib.h>
static void *plat_alloc(size_t n) { return calloc(1, n); }
static void *plat_realloc(void *p, size_t n) { return realloc(p, n); }
static void plat_free(void *p) { free(p); }
static void plat_write(const char *s, size_t n) { fwrite(s, 1, n, stdout); }
static void plat_abort(void) { exit(1); }
static int fmt_g(char *buf, double v) { return snprintf(buf, 32, "%g", v); }
static int fmt_spec_i64(char *buf, const char *spec, int64_t v) {
    return snprintf(buf, 64, spec, v);
}
static int fmt_spec_f(char *buf, const char *spec, double v) {
    return snprintf(buf, 64, spec, v);
}
#endif

void jrt_noop_drop(void *p);
void jrt_noop_trace(void *p, void (*visit)(void *));
void *jrt_alloc(int64_t size);
void jrt_retain(void *p);
void jrt_throw_npe(void);
void jrt_throw_sioobe(void);
static void jrt_sb_drop(void *p);

/* Vtable für zur Laufzeit erzeugte Strings. Ihr Layout (mit Type-Descriptor
 * und den Object-Methoden-Slots) ist programmabhängig, daher wird sie im
 * generierten Code als @vt.java_lang_String erzeugt; @main setzt diesen
 * Zeiger beim Start. String-Literale referenzieren dieselbe Vtable direkt. */
void *jrt_dyn_string_vt = NULL;

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
    plat_write((const char *)s->bytes, (size_t)s->len);
}

void jrt_println_str(const JStr *s) {
    jrt_print_str(s);
    plat_write("\n", 1);
}

void jrt_print_int(int32_t v) {
    char b[24];
    plat_write(b, (size_t)fmt_i64(b, v));
}

void jrt_println_int(int32_t v) {
    jrt_print_int(v);
    plat_write("\n", 1);
}

void jrt_println_ln(void) {
    plat_write("\n", 1);
}

void jrt_print_char(int32_t c) {
    char cc = (char)c;
    plat_write(&cc, 1);
}

void jrt_println_char(int32_t c) {
    jrt_print_char(c);
    plat_write("\n", 1);
}

/* String-Methoden (Byte-/ASCII-Semantik, s. Frontend-Kommentar).
 * NPE/StringIndexOutOfBounds sind abfangbar (pending statt exit). */
int32_t jrt_str_length(const JStr *s) {
    if (!s) {
        jrt_throw_npe();
        return 0;
    }
    return (int32_t)s->len;
}

int32_t jrt_str_is_empty(const JStr *s) {
    if (!s) {
        jrt_throw_npe();
        return 0;
    }
    return s->len == 0;
}

int32_t jrt_str_char_at(const JStr *s, int32_t i) {
    if (!s) {
        jrt_throw_npe();
        return 0;
    }
    if (i < 0 || i >= s->len) {
        jrt_throw_sioobe();
        return 0;
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

/* --- Object-Wurzelmethoden (virtueller Dispatch) --------------------
 * Default-Implementierungen für Klassen, die equals/hashCode/toString
 * nicht überschreiben, plus die String-Überschreibungen. */
int32_t jrt_obj_equals(void *a, void *b) {
    return a == b; /* Referenzidentität */
}
int32_t jrt_obj_hashcode(void *o) {
    uintptr_t p = (uintptr_t)o;
    return (int32_t)(p ^ (p >> 32));
}

/* String.hashCode (JLS): s[0]*31^(n-1) + … + s[n-1]. */
int32_t jrt_str_hashcode(const JStr *s) {
    int32_t h = 0;
    for (int64_t i = 0; i < s->len; i++) {
        h = 31 * h + (int32_t)s->bytes[i];
    }
    return h;
}
/* String.toString gibt sich selbst zurück. */
void *jrt_str_tostring(void *s) {
    return s;
}

/* Vorwärtsdeklaration (str_from_buf ist weiter unten definiert). */
static JStr *str_from_buf(const char *buf, int n);
void *jrt_obj_tostring(void *o) {
    (void)o;
    return str_from_buf("object", 6);
}

/* Weitere String-Methoden (Byte-/ASCII-Semantik). Suchende/vergleichende
 * geben int/bool; substring/trim/concat neue (RC-verwaltete) Strings. */
int32_t jrt_str_indexof(const JStr *s, const JStr *sub) {
    if (!s) { jrt_throw_npe(); return -1; }
    if (!sub || sub->len == 0) return 0;
    if (sub->len > s->len) return -1;
    for (int64_t i = 0; i + sub->len <= s->len; i++) {
        int64_t j = 0;
        while (j < sub->len && s->bytes[i + j] == sub->bytes[j]) j++;
        if (j == sub->len) return (int32_t)i;
    }
    return -1;
}
int32_t jrt_str_startswith(const JStr *s, const JStr *p) {
    if (!s) { jrt_throw_npe(); return 0; }
    if (!p || p->len > s->len) return 0;
    for (int64_t i = 0; i < p->len; i++)
        if (s->bytes[i] != p->bytes[i]) return 0;
    return 1;
}
int32_t jrt_str_endswith(const JStr *s, const JStr *p) {
    if (!s) { jrt_throw_npe(); return 0; }
    if (!p || p->len > s->len) return 0;
    int64_t off = s->len - p->len;
    for (int64_t i = 0; i < p->len; i++)
        if (s->bytes[off + i] != p->bytes[i]) return 0;
    return 1;
}
int32_t jrt_str_compareto(const JStr *a, const JStr *b) {
    if (!a || !b) { jrt_throw_npe(); return 0; }
    int64_t n = a->len < b->len ? a->len : b->len;
    for (int64_t i = 0; i < n; i++) {
        int d = (int)a->bytes[i] - (int)b->bytes[i];
        if (d) return d;
    }
    return (int32_t)(a->len - b->len);
}
void *jrt_str_substring2(const JStr *s, int32_t from, int32_t to) {
    if (!s) { jrt_throw_npe(); return NULL; }
    if (from < 0 || (int64_t)to > s->len || from > to) { jrt_throw_sioobe(); return NULL; }
    return str_from_buf((const char *)s->bytes + from, to - from);
}
void *jrt_str_substring1(const JStr *s, int32_t from) {
    if (!s) { jrt_throw_npe(); return NULL; }
    return jrt_str_substring2(s, from, (int32_t)s->len);
}
void *jrt_str_trim(const JStr *s) {
    if (!s) { jrt_throw_npe(); return NULL; }
    int64_t a = 0, b = s->len;
    while (a < b && (unsigned char)s->bytes[a] <= ' ') a++;
    while (b > a && (unsigned char)s->bytes[b - 1] <= ' ') b--;
    return str_from_buf((const char *)s->bytes + a, (int)(b - a));
}

/* --- Wrapper-Klassen (Autoboxing) -----------------------------------
 * Integer/Long/Boolean sind reguläre Objekte (RC-verwaltet) mit einem
 * eingepackten Primitivwert und generierter Vtable (Object-Methoden).
 * Die Vtable-Zeiger setzt @main beim Start (programmabhängiges Layout).
 * Kein Wertecache (-128..127) → boxed-Identität kann von Java abweichen;
 * equals ist korrekt. */
void *jrt_integer_vt = NULL;
void *jrt_long_vt = NULL;
void *jrt_boolean_vt = NULL;

typedef struct {
    int64_t refcount, rcflags;
    void *vtable;
    int32_t value;
} JInteger;
typedef struct {
    int64_t refcount, rcflags;
    void *vtable;
    int64_t value;
} JLong;

void *jrt_integer_valueof(int32_t v) {
    JInteger *o = (JInteger *)jrt_alloc((int64_t)sizeof(JInteger));
    o->vtable = jrt_integer_vt;
    o->value = v;
    return o;
}
int32_t jrt_integer_intvalue(void *o) { return ((JInteger *)o)->value; }
int32_t jrt_integer_hashcode(void *o) { return ((JInteger *)o)->value; }
int32_t jrt_integer_equals(void *a, void *b) {
    if (!b || ((JInteger *)b)->vtable != jrt_integer_vt) return 0;
    return ((JInteger *)a)->value == ((JInteger *)b)->value;
}
void *jrt_integer_tostring(void *o) {
    char buf[16];
    return str_from_buf(buf, fmt_i64(buf, ((JInteger *)o)->value));
}

void *jrt_long_valueof(int64_t v) {
    JLong *o = (JLong *)jrt_alloc((int64_t)sizeof(JLong));
    o->vtable = jrt_long_vt;
    o->value = v;
    return o;
}
int64_t jrt_long_longvalue(void *o) { return ((JLong *)o)->value; }
int32_t jrt_long_hashcode(void *o) {
    int64_t v = ((JLong *)o)->value;
    return (int32_t)(v ^ (v >> 32)); /* Java Long.hashCode */
}
int32_t jrt_long_equals(void *a, void *b) {
    if (!b || ((JLong *)b)->vtable != jrt_long_vt) return 0;
    return ((JLong *)a)->value == ((JLong *)b)->value;
}
void *jrt_long_tostring(void *o) {
    char buf[24];
    return str_from_buf(buf, fmt_i64(buf, ((JLong *)o)->value));
}

/* Boolean nutzt dasselbe Layout wie Integer (0/1). */
void *jrt_boolean_valueof(int32_t v) {
    JInteger *o = (JInteger *)jrt_alloc((int64_t)sizeof(JInteger));
    o->vtable = jrt_boolean_vt;
    o->value = v ? 1 : 0;
    return o;
}
int32_t jrt_boolean_booleanvalue(void *o) { return ((JInteger *)o)->value; }
int32_t jrt_boolean_hashcode(void *o) { return ((JInteger *)o)->value ? 1231 : 1237; }
int32_t jrt_boolean_equals(void *a, void *b) {
    if (!b || ((JInteger *)b)->vtable != jrt_boolean_vt) return 0;
    return ((JInteger *)a)->value == ((JInteger *)b)->value;
}
void *jrt_boolean_tostring(void *o) {
    return ((JInteger *)o)->value ? str_from_buf("true", 4) : str_from_buf("false", 5);
}

void *jrt_double_vt = NULL;
void *jrt_character_vt = NULL;
typedef struct {
    int64_t refcount, rcflags;
    void *vtable;
    double value;
} JDouble;

void *jrt_double_valueof(double v) {
    JDouble *o = (JDouble *)jrt_alloc((int64_t)sizeof(JDouble));
    o->vtable = jrt_double_vt;
    o->value = v;
    return o;
}
double jrt_double_doublevalue(void *o) { return ((JDouble *)o)->value; }
int32_t jrt_double_hashcode(void *o) {
    int64_t bits;
    jrt_memcpy(&bits, &((JDouble *)o)->value, sizeof bits);
    return (int32_t)(bits ^ (bits >> 32)); /* Java Double.hashCode */
}
int32_t jrt_double_equals(void *a, void *b) {
    if (!b || ((JDouble *)b)->vtable != jrt_double_vt) return 0;
    return ((JDouble *)a)->value == ((JDouble *)b)->value;
}
void *jrt_double_tostring(void *o) {
    char buf[32];
    return str_from_buf(buf, fmt_g(buf, ((JDouble *)o)->value));
}

/* Character nutzt dasselbe Layout wie Integer (char = i32). */
void *jrt_character_valueof(int32_t v) {
    JInteger *o = (JInteger *)jrt_alloc((int64_t)sizeof(JInteger));
    o->vtable = jrt_character_vt;
    o->value = v;
    return o;
}
int32_t jrt_character_charvalue(void *o) { return ((JInteger *)o)->value; }
int32_t jrt_character_hashcode(void *o) { return ((JInteger *)o)->value; }
int32_t jrt_character_equals(void *a, void *b) {
    if (!b || ((JInteger *)b)->vtable != jrt_character_vt) return 0;
    return ((JInteger *)a)->value == ((JInteger *)b)->value;
}
void *jrt_character_tostring(void *o) {
    char c = (char)((JInteger *)o)->value;
    return str_from_buf(&c, 1);
}

void *jrt_float_vt = NULL;
typedef struct {
    int64_t refcount, rcflags;
    void *vtable;
    float value;
} JFloat;

void *jrt_float_valueof(float v) {
    JFloat *o = (JFloat *)jrt_alloc((int64_t)sizeof(JFloat));
    o->vtable = jrt_float_vt;
    o->value = v;
    return o;
}
float jrt_float_floatvalue(void *o) { return ((JFloat *)o)->value; }
int32_t jrt_float_hashcode(void *o) {
    int32_t bits;
    float v = ((JFloat *)o)->value;
    jrt_memcpy(&bits, &v, sizeof bits);
    return bits; /* Java Float.hashCode = floatToIntBits */
}
int32_t jrt_float_equals(void *a, void *b) {
    if (!b || ((JFloat *)b)->vtable != jrt_float_vt) return 0;
    return ((JFloat *)a)->value == ((JFloat *)b)->value;
}
void *jrt_float_tostring(void *o) {
    char buf[32];
    return str_from_buf(buf, fmt_g(buf, (double)((JFloat *)o)->value));
}

/* --- String-Konkatenation (invokedynamic makeConcatWithConstants) ----
 * Zur Laufzeit erzeugte Strings; refcount-verwaltet (kein immortal).
 * jrt_alloc (weiter unten definiert) setzt refcount=1 und trackt live. */

static JStr *str_alloc(int64_t len) {
    JStr *s = (JStr *)jrt_alloc((int64_t)sizeof(JStr) + len);
    s->vtable = jrt_dyn_string_vt;
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
    jrt_memcpy(r->bytes, ba, (size_t)la);
    jrt_memcpy(r->bytes + la, bb, (size_t)lb);
    return r;
}

static JStr *str_from_buf(const char *buf, int n) {
    JStr *r = str_alloc(n);
    jrt_memcpy(r->bytes, buf, (size_t)n);
    return r;
}

JStr *jrt_int_to_str(int32_t v) {
    char buf[16];
    return str_from_buf(buf, fmt_i64(buf, v));
}
JStr *jrt_long_to_str(int64_t v) {
    char buf[24];
    return str_from_buf(buf, fmt_i64(buf, v));
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
    return str_from_buf(buf, fmt_g(buf, d));
}
JStr *jrt_float_to_str(float f) {
    char buf[32];
    return str_from_buf(buf, fmt_g(buf, (double)f));
}

/* --- StringBuilder (runtime-gestützt) -------------------------------
 * Wachsender Byte-Puffer; RC-verwaltetes Objekt mit eigener drop-Funktion
 * (gibt den Puffer frei). append(X) gibt this zurück (Verkettung). */
void (*jrt_sb_vtable[3])(void) = {
    (void (*)(void))jrt_sb_drop,
    (void (*)(void))jrt_noop_trace,
    (void (*)(void))0, /* kein Type-Descriptor */
};
typedef struct {
    int64_t refcount, rcflags;
    void *vtable;
    uint8_t *buf;
    int64_t len, cap;
} JSB;

static void jrt_sb_drop(void *p) { plat_free(((JSB *)p)->buf); }

void *jrt_sb_new(void) {
    JSB *sb = (JSB *)jrt_alloc((int64_t)sizeof(JSB));
    sb->vtable = (void *)jrt_sb_vtable;
    sb->cap = 16;
    sb->buf = (uint8_t *)plat_alloc(16);
    sb->len = 0;
    return sb;
}
static void sb_append(JSB *sb, const void *b, int64_t n) {
    if (sb->len + n > sb->cap) {
        while (sb->len + n > sb->cap) sb->cap *= 2;
        sb->buf = (uint8_t *)plat_realloc(sb->buf, (size_t)sb->cap);
    }
    jrt_memcpy(sb->buf + sb->len, b, (size_t)n);
    sb->len += n;
}
/* append gibt this zurück (Verkettung); der Aufrufer erwartet eine
 * transferierte +1-Referenz → retain. */
void *jrt_sb_append_str(void *p, const JStr *s) {
    if (s) sb_append((JSB *)p, s->bytes, s->len);
    else sb_append((JSB *)p, "null", 4);
    jrt_retain(p);
    return p;
}
void *jrt_sb_append_int(void *p, int32_t v) {
    char b[16];
    sb_append((JSB *)p, b, fmt_i64(b, v));
    jrt_retain(p);
    return p;
}
void *jrt_sb_append_long(void *p, int64_t v) {
    char b[24];
    sb_append((JSB *)p, b, fmt_i64(b, v));
    jrt_retain(p);
    return p;
}
void *jrt_sb_append_double(void *p, double v) {
    char b[32];
    sb_append((JSB *)p, b, fmt_g(b, v));
    jrt_retain(p);
    return p;
}
void *jrt_sb_append_char(void *p, int32_t c) {
    char b = (char)c;
    sb_append((JSB *)p, &b, 1);
    jrt_retain(p);
    return p;
}
void *jrt_sb_append_bool(void *p, int32_t v) {
    if (v) sb_append((JSB *)p, "true", 4);
    else sb_append((JSB *)p, "false", 5);
    jrt_retain(p);
    return p;
}
JStr *jrt_sb_tostring(void *p) {
    JSB *sb = (JSB *)p;
    return str_from_buf((const char *)sb->buf, (int)sb->len);
}
int32_t jrt_sb_length(void *p) { return (int32_t)((JSB *)p)->len; }

/* String.format / printf: parst den Format-String und interpretiert die
 * Object[]-Argumente je Spezifizierer (%d/%i/%s/%f/%x/%c/%b/%%). Optionale
 * Flags/Breite/Präzision werden an snprintf durchgereicht. %s erwartet einen
 * String (Byte-Kopie); Wrapper-Werte über %d/%f/… (Autoboxing im Aufrufer). */
void jrt_retain(void *p);
void jrt_release(void *p);
JStr *jrt_str_format(const JStr *fmt, void *argsp) {
    /* Object[]-Layout: { rc, rcflags, vtable, length, elems… }; length bei
     * Offset 24, Elemente ab 40 (JArray ist weiter unten definiert). */
    int64_t nargs = argsp ? *(int64_t *)((char *)argsp + 24) : 0;
    void **elems = argsp ? (void **)((char *)argsp + 40) : NULL;
    JSB *sb = (JSB *)jrt_sb_new();
    int ai = 0;
    for (int64_t i = 0; i < fmt->len; i++) {
        char c = fmt->bytes[i];
        if (c != '%') { sb_append(sb, &c, 1); continue; }
        /* Spezifizierer bis zum Konversionszeichen sammeln. */
        char spec[16];
        int sl = 0;
        spec[sl++] = '%';
        i++;
        while (i < fmt->len && sl < 14) {
            char s = fmt->bytes[i];
            spec[sl++] = s;
            if (s == 'd' || s == 'i' || s == 's' || s == 'f' || s == 'x'
                || s == 'X' || s == 'c' || s == 'b' || s == '%' || s == 'n')
                break;
            i++;
        }
        char conv = spec[sl - 1];
        spec[sl] = '\0';
        char buf[64];
        if (conv == '%') {
            sb_append(sb, "%", 1);
            continue;
        }
        if (conv == 'n') { /* plattformunabhängiger Zeilenumbruch, kein Arg */
            sb_append(sb, "\n", 1);
            continue;
        }
        void *arg = (ai < nargs) ? elems[ai++] : NULL;
        switch (conv) {
        case 'd':
        case 'i': {
            spec[sl - 1] = 'd';
            sb_append(sb, buf, fmt_spec_i64(buf, spec, arg ? ((JInteger *)arg)->value : 0));
            break;
        }
        case 'x':
        case 'X':
            sb_append(sb, buf, fmt_spec_i64(buf, spec, arg ? ((JInteger *)arg)->value : 0));
            break;
        case 'f': {
            sb_append(sb, buf, fmt_spec_f(buf, spec, arg ? ((JDouble *)arg)->value : 0.0));
            break;
        }
        case 'c': {
            char ch = arg ? (char)((JInteger *)arg)->value : '?';
            sb_append(sb, &ch, 1);
            break;
        }
        case 'b': {
            int t = arg && ((JInteger *)arg)->value;
            sb_append(sb, t ? "true" : "false", t ? 4 : 5);
            break;
        }
        case 's':
        default: {
            JStr *s = (JStr *)arg;
            if (s) sb_append(sb, s->bytes, s->len);
            else sb_append(sb, "null", 4);
            break;
        }
        }
    }
    JStr *r = jrt_sb_tostring(sb);
    jrt_release(sb); /* temporären Puffer freigeben */
    return r;
}
/* StringBuilder(String)-Konstruktor: anhängen ohne retain (Rückgabe
 * verworfen, der Receiver ist geborgt). */
void jrt_sb_init_str(void *p, const JStr *s) {
    if (s) sb_append((JSB *)p, s->bytes, s->len);
    else sb_append((JSB *)p, "null", 4);
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
/* Zähler unter Threads atomar (sonst Datenrennen der Heap-Bilanz). */
#ifdef FASTLLVM_THREADS
#define CNT_INC(x) __atomic_add_fetch(&(x), 1, __ATOMIC_RELAXED)
#define CNT_DEC(x) __atomic_sub_fetch(&(x), 1, __ATOMIC_RELAXED)
#define CNT_POST_INC(x) __atomic_fetch_add(&(x), 1, __ATOMIC_RELAXED)
#else
#define CNT_INC(x) (++(x))
#define CNT_DEC(x) (--(x))
#define CNT_POST_INC(x) ((x)++)
#endif

/* Der synchrone Zyklen-Collector läuft nur, wenn er gebraucht wird: nicht
 * unter Threads (nicht thread-safe) und nicht, wenn der Solver das Programm
 * als azyklisch bewiesen hat (Phase 1 der Runtime-Elimination → reine RC). */
#if !defined(FASTLLVM_THREADS) && !defined(FASTLLVM_NO_CYCLES)
#define FASTLLVM_COLLECTOR 1
#endif

static void free_obj(JObjHeader *h) {
    plat_free(h);
    CNT_DEC(live_objects);
}

#ifdef FASTLLVM_COLLECTOR
/* Kandidaten-Wurzeln für die Zyklensuche (purple-Objekte). */
static JObjHeader **roots = NULL;
static size_t roots_len = 0, roots_cap = 0;
#define ROOTS_THRESHOLD 10000

static void jrt_collect_cycles(void);

static void roots_push(JObjHeader *h) {
    if (roots_len == roots_cap) {
        roots_cap = roots_cap ? roots_cap * 2 : 64;
        roots = (JObjHeader **)plat_realloc(roots, roots_cap * sizeof(*roots));
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
#endif /* FASTLLVM_COLLECTOR */

static void jrt_shutdown(void) {
#ifdef FASTLLVM_COLLECTOR
    jrt_collect_cycles();
#endif
#ifndef FASTLLVM_FREESTANDING
    /* Leak-Detektor nur hosted (getenv/Prozess-Exit). */
    if (getenv("FASTLLVM_HEAPSTATS")) {
        char b[24];
        plat_puts("[heap] ");
        plat_write(b, (size_t)fmt_i64(b, total_allocated));
        plat_puts(" alloziert, ");
        plat_write(b, (size_t)fmt_i64(b, live_objects));
        plat_puts(" noch live (Zyklen-Leak)\n");
    }
#endif
}

void *jrt_alloc(int64_t size) {
    void *p = plat_alloc((size_t)size);
    if (!p) {
        plat_puts("Exception in thread \"main\" java.lang.OutOfMemoryError\n");
        plat_abort();
    }
    if (CNT_POST_INC(total_allocated) == 0) {
#ifndef FASTLLVM_FREESTANDING
        atexit(jrt_shutdown);
#endif
    }
    CNT_INC(live_objects);
    ((JObjHeader *)p)->refcount = 1; /* der Erzeuger hält die erste Referenz */
    return p;
}

#ifdef FASTLLVM_THREADS
/* Threaded: atomare Refcounts. Inkrementelle Zyklen-Erkennung ist unter
 * Threads nicht thread-safe → deaktiviert; azyklischer Müll wird prompt
 * freigegeben, Zyklen bleiben bis Programmende liegen (dokumentierte Grenze,
 * echte nebenläufige Collection wäre Bacon-Rajans concurrent-Variante). */
void jrt_retain(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (__atomic_load_n(&h->refcount, __ATOMIC_RELAXED) < 0) return; /* immortal */
    __atomic_add_fetch(&h->refcount, 1, __ATOMIC_RELAXED);
}
void jrt_release(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (__atomic_load_n(&h->refcount, __ATOMIC_RELAXED) < 0) return; /* immortal */
    if (__atomic_sub_fetch(&h->refcount, 1, __ATOMIC_ACQ_REL) == 0) {
        run_drop(h);
        free_obj(h);
    }
}
#elif defined(FASTLLVM_NO_CYCLES)
/* Solver-bewiesen azyklisch → reine RC ohne Farb-/Puffer-Buchhaltung: nur
 * inc/dec, bei 0 freigeben. Der Zyklen-Collector ist gar nicht mitgelinkt. */
void jrt_retain(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (h->refcount < 0) return; /* immortal */
    h->refcount++;
}
void jrt_release(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (h->refcount < 0) return; /* immortal */
    if (--h->refcount == 0) {
        run_drop(h);
        free_obj(h);
    }
}
#else
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
#endif

/* --- Nebenläufigkeit: Monitore + Thread ------------------------------
 * Thread-Layout (Frontend): {header(3 Worte), $runnable@24, $handle@32}.
 * run() ruft die generierte Trampoline @jrt_invoke_runnable auf. Unter
 * --threads echte pthreads + rekursiver globaler Monitor; sonst läuft
 * start() synchron (gültiger sequentieller Schedule), Monitore sind No-Ops. */
void jrt_invoke_runnable(void *runnable); /* vom generierten Code definiert */

#ifdef FASTLLVM_THREADS
#include <pthread.h>
static pthread_mutex_t g_monitor;
static pthread_once_t g_monitor_once = PTHREAD_ONCE_INIT;
static void init_monitor(void) {
    pthread_mutexattr_t a;
    pthread_mutexattr_init(&a);
    pthread_mutexattr_settype(&a, PTHREAD_MUTEX_RECURSIVE);
    pthread_mutex_init(&g_monitor, &a);
}
void jrt_monitor_enter(void *o) {
    (void)o;
    pthread_once(&g_monitor_once, init_monitor);
    pthread_mutex_lock(&g_monitor);
}
void jrt_monitor_exit(void *o) {
    (void)o;
    pthread_mutex_unlock(&g_monitor);
}
static void *thread_tramp(void *runnable) {
    jrt_invoke_runnable(runnable);
    jrt_release(runnable); /* die beim Start genommene Referenz */
    return NULL;
}
void jrt_thread_start(void *thread) {
    if (!thread) return;
    void *runnable = *(void **)((char *)thread + 24);
    jrt_retain(runnable); /* überlebt bis der Thread endet */
    pthread_t tid;
    pthread_create(&tid, NULL, thread_tramp, runnable);
    *(int64_t *)((char *)thread + 32) = (int64_t)tid;
}
void jrt_thread_join(void *thread) {
    if (!thread) return;
    pthread_t tid = (pthread_t) * (int64_t *)((char *)thread + 32);
    if (tid) pthread_join(tid, NULL);
}
#else
void jrt_monitor_enter(void *o) { (void)o; }
void jrt_monitor_exit(void *o) { (void)o; }
void jrt_thread_start(void *thread) {
    if (!thread) return;
    /* Ohne Threads: synchroner Lauf — ein gültiger sequentieller Schedule. */
    jrt_invoke_runnable(*(void **)((char *)thread + 24));
}
void jrt_thread_join(void *thread) { (void)thread; }
#endif

/* --- Bacon-Rajan: MarkRoots / ScanRoots / CollectRoots --------------- */
#ifdef FASTLLVM_COLLECTOR

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
#endif /* FASTLLVM_COLLECTOR */

void jrt_null_check(const void *p) {
    if (!p) {
        plat_puts("Exception in thread \"main\" java.lang.NullPointerException\n");
        plat_abort();
    }
}

/* --- Exceptions ------------------------------------------------------
 * "pending exception"-Modell (single-thread): jrt_throw setzt die schwebende
 * Exception (und hält eine Referenz darauf); der generierte Code prüft nach
 * jedem werfenden Aufruf jrt_pending_set und springt zum Handler oder
 * propagiert. jrt_take_pending übergibt die Referenz an den Handler. */
static void *pending_exception = NULL;
/* Meldungstext einer schwebenden Laufzeit-Exception (Sentinel); NULL bei
 * benutzergeworfenen Exceptions. */
static const char *pending_message = NULL;

void jrt_throw(void *e) {
    jrt_retain(e); /* bleibt am Leben, solange sie schwebt */
    pending_exception = e;
    pending_message = NULL;
}

/* Sentinel-Objekte für Laufzeit-Exceptions (NPE, ArithmeticException, …):
 * immortale Header (refcount -1) mit einer No-Op-Vtable ohne
 * Type-Descriptor. Von catch-all (catch Exception / RuntimeException)
 * gefangen; ihre Meldung überlebt bis zur Uncaught-Ausgabe. */
void *jrt_sentinel_vtable[3] = {(void *)jrt_noop_drop, (void *)jrt_noop_trace, NULL};
static JObjHeader arith_exc_obj = {-1, 0, jrt_sentinel_vtable};
static JObjHeader npe_exc_obj = {-1, 0, jrt_sentinel_vtable};
static JObjHeader bounds_exc_obj = {-1, 0, jrt_sentinel_vtable};

/* Von den Runtime-Checks aufgerufen: schwebende Laufzeit-Exception setzen. */
static void throw_runtime(void *sentinel, const char *msg) {
    pending_exception = sentinel;
    pending_message = msg;
}

/* Abfangbare NullPointerException (Feld-/Receiver-Zugriff): Sentinel in
 * pending setzen; der generierte Code überspringt den Zugriff und prüft
 * danach pending. */
void jrt_throw_npe(void) {
    throw_runtime(&npe_exc_obj, "java.lang.NullPointerException");
}
void jrt_throw_sioobe(void) {
    throw_runtime(&bounds_exc_obj, "java.lang.StringIndexOutOfBoundsException");
}

/* Throwable.getMessage(): liest das $message-Feld (erstes Instanzfeld von
 * java.lang.Throwable → Offset 3 Worte). Laufzeit-Sentinels (Arith/NPE/…)
 * haben keinen Type-Descriptor (vt[2]==NULL) und kein solches Feld → null.
 * Rückgabe retained (+1 für den Aufrufer, Owning-Slot-Modell). */
void *jrt_throwable_message(void *obj) {
    if (!obj) {
        throw_runtime(&npe_exc_obj, "java.lang.NullPointerException");
        return NULL;
    }
    void **vt = (void **)((JObjHeader *)obj)->vtable;
    if (!vt || vt[2] == NULL) return NULL; /* Sentinel ohne Message-Feld */
    void *msg = *(void **)((char *)obj + 24);
    jrt_retain(msg);
    return msg;
}
int32_t jrt_pending_set(void) {
    return pending_exception != NULL;
}
/* Übergibt die schwebende Referenz an den Aufrufer (Handler) und löscht
 * das Flag — kein retain/release, die +1 wird transferiert. */
void *jrt_take_pending(void) {
    void *e = pending_exception;
    pending_exception = NULL;
    return e;
}
/* instanceof: läuft die Type-Descriptor-Kette des Objekts ab und vergleicht
 * mit dem Ziel-Descriptor. Vtable-Slot 2 ist der Type-Descriptor;
 * { ptr super }. Immortale Objekte ohne Descriptor (Slot 2 null) → false. */
typedef struct TypeDesc {
    struct TypeDesc *super;
    const char *cname; /* gepunkteter Klassenname für Uncaught-Meldung */
    void *jclass;      /* Class-Objekt-Singleton dieser Klasse (Reflection) */
} TypeDesc;

/* Reflection: obj.getClass() → das Class-Singleton über den Type-Descriptor.
 * getName/getSimpleName lesen die JStr-Felder des Class-Objekts (Layout:
 * {refcount,rcflags,vtable,name,simpleName} → Offsets 24/32). */
void *jrt_get_class(void *obj) {
    if (!obj) {
        jrt_throw_npe();
        return NULL;
    }
    void **vt = (void **)((JObjHeader *)obj)->vtable;
    if (!vt) return NULL;
    TypeDesc *td = (TypeDesc *)vt[2];
    return td ? td->jclass : NULL;
}
void *jrt_class_getname(void *jc) {
    if (!jc) {
        jrt_throw_npe();
        return NULL;
    }
    return *(void **)((char *)jc + 24);
}
void *jrt_class_getsimplename(void *jc) {
    if (!jc) {
        jrt_throw_npe();
        return NULL;
    }
    return *(void **)((char *)jc + 32);
}

int32_t jrt_instanceof(void *obj, void *target_td) {
    if (!obj) return 0;
    JObjHeader *h = (JObjHeader *)obj;
    void **vt = (void **)h->vtable;
    if (!vt) return 0;
    TypeDesc *td = (TypeDesc *)vt[2];
    while (td) {
        if ((void *)td == target_td) return 1;
        td = td->super;
    }
    return 0;
}

/* Prüft die schwebende Exception gegen einen catch-Typ (Dispatch-Kaskade). */
int32_t jrt_pending_instanceof(void *target_td) {
    return jrt_instanceof(pending_exception, target_td);
}

/* Laufzeit-checkcast auf eine modellierte Klasse: null passiert immer,
 * sonst muss der Typ passen (ClassCastException = Abbruch). */
void jrt_checkcast(void *obj, void *target_td) {
    if (obj && !jrt_instanceof(obj, target_td)) {
        plat_puts("Exception in thread \"main\" java.lang.ClassCastException\n");
        plat_abort();
    }
}

/* Von @main nach java_main aufgerufen: unbehandelte Exception melden.
 * (Ohne Laufzeit-Typinfo generisch — Klassenname/Message wären ein
 * späterer Schritt, DESIGN.md §6.) */
void jrt_check_uncaught(void) {
    if (!pending_exception) return;
    if (pending_message) {
        /* Laufzeit-Exception (Sentinel) mit fertigem Meldungstext. */
        plat_uncaught(pending_message);
    } else {
        /* Benutzer-Exception: Klassenname aus dem Type-Descriptor. */
        JObjHeader *h = (JObjHeader *)pending_exception;
        void **vt = (void **)h->vtable;
        TypeDesc *td = vt ? (TypeDesc *)vt[2] : NULL;
        if (td && td->cname) {
            plat_uncaught(td->cname);
        } else {
            plat_puts("Exception in thread \"main\" (unbehandelte Exception)\n");
        }
    }
    plat_abort();
}

/* --- Arrays ---------------------------------------------------------- */

/* Gleicher Header wie Objekte, dann Länge + Elementgröße (für arraycopy/
 * clone ohne statischen Typ); Elemente folgen direkt (ab Offset 40). */
typedef struct {
    int64_t refcount;
    int64_t rcflags;
    void *vtable;
    int64_t length;
    int64_t elem_size;
} JArray;

void *jrt_alloc_array(int64_t count, int64_t elem_size, void *vtable) {
    if (count < 0) {
        plat_puts("Exception in thread \"main\" java.lang.NegativeArraySizeException\n");
        plat_abort();
    }
    void *p = plat_alloc(sizeof(JArray) + (size_t)count * (size_t)elem_size);
    if (!p) {
        plat_puts("Exception in thread \"main\" java.lang.OutOfMemoryError\n");
        plat_abort();
    }
    if (CNT_POST_INC(total_allocated) == 0) {
#ifndef FASTLLVM_FREESTANDING
        atexit(jrt_shutdown);
#endif
    }
    CNT_INC(live_objects);
    JArray *a = (JArray *)p;
    a->refcount = 1;
    a->vtable = vtable;
    a->length = count;
    a->elem_size = elem_size;
    return p;
}

void jrt_bounds_check(const void *arr, int32_t index) {
    int64_t len = ((const JArray *)arr)->length;
    if (index < 0 || index >= len) {
        char nb[24];
        plat_puts("Exception in thread \"main\" "
                  "java.lang.ArrayIndexOutOfBoundsException: Index ");
        plat_write(nb, (size_t)fmt_i64(nb, index));
        plat_puts(" out of bounds for length ");
        plat_write(nb, (size_t)fmt_i64(nb, len));
        plat_write("\n", 1);
        plat_abort();
    }
}

/* Abfangbare Array-Zugriffe: Check + Zugriff gekapselt, damit sie über das
 * pending-Modell werfen können (NPE/ArrayIndexOutOfBounds) statt abzubrechen.
 * Bei Fehler wird ein safe default zurückgegeben; der generierte Code prüft
 * danach pending und springt zum Handler oder propagiert. */
#define NPE_MSG "java.lang.NullPointerException"
#define AIOOBE_MSG "java.lang.ArrayIndexOutOfBoundsException"

static int arr_ok(const JArray *a, int32_t i) {
    if (!a) {
        throw_runtime(&npe_exc_obj, NPE_MSG);
        return 0;
    }
    if (i < 0 || i >= a->length) {
        throw_runtime(&bounds_exc_obj, AIOOBE_MSG);
        return 0;
    }
    return 1;
}

int32_t jrt_iaload(void *arr, int32_t i) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return 0;
    return ((int32_t *)(a + 1))[i];
}
void jrt_iastore(void *arr, int32_t i, int32_t v) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return;
    ((int32_t *)(a + 1))[i] = v;
}
void *jrt_aaload(void *arr, int32_t i) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return NULL;
    return ((void **)(a + 1))[i]; /* geborgt; Aufrufer retained */
}
void jrt_aastore(void *arr, int32_t i, void *v) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return;
    void **slot = &((void **)(a + 1))[i];
    jrt_retain(v);
    jrt_release(*slot);
    *slot = v;
}
int32_t jrt_arraylen(void *arr) {
    if (!arr) {
        throw_runtime(&npe_exc_obj, NPE_MSG);
        return 0;
    }
    return (int32_t)((JArray *)arr)->length;
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

/* enum valueOf: über das values-Array laufen und das Element mit passendem
 * $name (erstes Instanzfeld, Offset 3 Worte) suchen. Rückgabe retained (+1
 * für den Aufrufer); das Array selbst bleibt Eigentum des Aufrufers und wird
 * von dessen Owning-Slot-Cleanup freigegeben. */
int32_t jrt_str_equals(const JStr *a, const JStr *b);
void *jrt_enum_valueof(void *values, void *name) {
    JArray *a = (JArray *)values;
    void *found = NULL;
    void **elems = (void **)(a + 1);
    for (int64_t i = 0; i < a->length; i++) {
        void *e = elems[i];
        if (!e) continue;
        JStr *ename = *(JStr **)((char *)e + 24);
        if (jrt_str_equals(ename, (const JStr *)name)) {
            found = e;
            break;
        }
    }
    if (!found) {
        plat_puts("Exception in thread \"main\" java.lang.IllegalArgumentException\n");
        plat_abort();
    }
    jrt_retain(found);
    return found;
}

/* Flache Kopie eines Arrays (u.a. für enum values()). Gleiche vtable und
 * Länge; bei Ref-Arrays wird jedes kopierte Element retained. */
void *jrt_array_clone(void *arr, int64_t elem_size, int32_t is_ref) {
    if (!arr) {
        throw_runtime(&npe_exc_obj, NPE_MSG);
        return NULL;
    }
    JArray *a = (JArray *)arr;
    void *p = jrt_alloc_array(a->length, elem_size, a->vtable);
    jrt_memcpy((JArray *)p + 1, a + 1, (size_t)a->length * (size_t)elem_size);
    if (is_ref) {
        void **elems = (void **)((JArray *)p + 1);
        for (int64_t i = 0; i < a->length; i++) {
            jrt_retain(elems[i]);
        }
    }
    return p;
}

/* System.arraycopy: elementgrößen-/ref-korrekt (elem_size + Ref-Vtable im
 * Header). Überlappungsfest (memmove-Semantik); Ref-Arrays retainen die
 * Quelle vor dem Freigeben des Ziels. Fehler brechen ab (nicht abfangbar). */
void jrt_array_ref_drop(void *p);
void jrt_arraycopy(void *src, int32_t srcPos, void *dst, int32_t dstPos, int32_t len) {
    if (!src || !dst) {
        plat_uncaught("java.lang.NullPointerException");
        plat_abort();
    }
    JArray *s = (JArray *)src, *d = (JArray *)dst;
    if (srcPos < 0 || dstPos < 0 || len < 0 ||
        (int64_t)srcPos + len > s->length || (int64_t)dstPos + len > d->length) {
        plat_uncaught("java.lang.ArrayIndexOutOfBoundsException");
        plat_abort();
    }
    if (len == 0) return;
    int64_t es = s->elem_size;
    void **svt = (void **)s->vtable;
    int is_ref = svt && svt[0] == (void *)jrt_array_ref_drop;
    if (is_ref) {
        void **se = (void **)(s + 1), **de = (void **)(d + 1);
        for (int32_t i = 0; i < len; i++) jrt_retain(se[srcPos + i]);   /* Quelle sichern */
        for (int32_t i = 0; i < len; i++) jrt_release(de[dstPos + i]);  /* Ziel freigeben */
        if (dstPos < srcPos)
            for (int32_t i = 0; i < len; i++) de[dstPos + i] = se[srcPos + i];
        else
            for (int32_t i = len - 1; i >= 0; i--) de[dstPos + i] = se[srcPos + i];
    } else {
        uint8_t *sb = (uint8_t *)(s + 1) + (int64_t)srcPos * es;
        uint8_t *db = (uint8_t *)(d + 1) + (int64_t)dstPos * es;
        int64_t n = (int64_t)len * es;
        if (db < sb)
            for (int64_t i = 0; i < n; i++) db[i] = sb[i];
        else
            for (int64_t i = n - 1; i >= 0; i--) db[i] = sb[i];
    }
}

/* --- Zahl-Parsing / Math / Zeit (Runtime-Intrinsics) ----------------- */

/* Integer.parseInt / Long.parseLong (Byte-/ASCII-Semantik). Ungültige
 * Eingabe bricht ab (NumberFormatException nicht abfangbar). */
static int64_t parse_signed(const JStr *s) {
    if (!s || s->len == 0) {
        plat_uncaught("java.lang.NumberFormatException");
        plat_abort();
    }
    int64_t i = 0, sign = 1;
    if (s->bytes[0] == '-') { sign = -1; i = 1; }
    else if (s->bytes[0] == '+') { i = 1; }
    if (i >= s->len) {
        plat_uncaught("java.lang.NumberFormatException");
        plat_abort();
    }
    int64_t v = 0;
    for (; i < s->len; i++) {
        uint8_t c = s->bytes[i];
        if (c < '0' || c > '9') {
            plat_uncaught("java.lang.NumberFormatException");
            plat_abort();
        }
        v = v * 10 + (c - '0');
    }
    return sign * v;
}
int32_t jrt_parse_int(const JStr *s) { return (int32_t)parse_signed(s); }
int64_t jrt_parse_long(const JStr *s) { return parse_signed(s); }

int32_t jrt_math_abs_i(int32_t v) { return v < 0 ? -v : v; }
int64_t jrt_math_abs_l(int64_t v) { return v < 0 ? -v : v; }
double jrt_math_abs_d(double v) { return v < 0 ? -v : v; }
float jrt_math_abs_f(float v) { return v < 0 ? -v : v; }
int32_t jrt_math_max_i(int32_t a, int32_t b) { return a > b ? a : b; }
int32_t jrt_math_min_i(int32_t a, int32_t b) { return a < b ? a : b; }
int64_t jrt_math_max_l(int64_t a, int64_t b) { return a > b ? a : b; }
int64_t jrt_math_min_l(int64_t a, int64_t b) { return a < b ? a : b; }
double jrt_math_max_d(double a, double b) { return a > b ? a : b; }
double jrt_math_min_d(double a, double b) { return a < b ? a : b; }
/* Portables sqrt (Newton-Raphson) — libm-frei, auch freestanding. */
double jrt_math_sqrt(double x) {
    if (x < 0) return 0.0 / 0.0; /* NaN */
    if (x == 0.0) return 0.0;
    double g = x > 1.0 ? x : 1.0;
    for (int i = 0; i < 60; i++) g = 0.5 * (g + x / g);
    return g;
}

/* Zeit: hosted echte Monotonie, freestanding über schwachen Hook (Default 0). */
#ifdef FASTLLVM_FREESTANDING
__attribute__((weak)) int64_t jrt_platform_time_ns(void) { return 0; }
int64_t jrt_nano_time(void) { return jrt_platform_time_ns(); }
int64_t jrt_current_time_millis(void) { return jrt_platform_time_ns() / 1000000; }
#else
#include <time.h>
int64_t jrt_nano_time(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}
int64_t jrt_current_time_millis(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (int64_t)ts.tv_sec * 1000LL + ts.tv_nsec / 1000000LL;
}
#endif

/* --- Java-Arithmetik-Semantik ---------------------------------------- */

/* JLS 15.17.2: Division durch 0 wirft ArithmeticException (jetzt abfangbar
 * über das pending-Modell); INT_MIN / -1 ist definiert als INT_MIN. */
#define ARITH_MSG "java.lang.ArithmeticException: / by zero"

int32_t jrt_idiv(int32_t a, int32_t b) {
    if (b == 0) {
        throw_runtime(&arith_exc_obj, ARITH_MSG);
        return 0;
    }
    if (a == INT32_MIN && b == -1)
        return INT32_MIN;
    return a / b;
}

int32_t jrt_irem(int32_t a, int32_t b) {
    if (b == 0) {
        throw_runtime(&arith_exc_obj, ARITH_MSG);
        return 0;
    }
    if (a == INT32_MIN && b == -1)
        return 0;
    return a % b;
}

/* --- long/double --------------------------------------------------- */

int64_t jrt_ldiv(int64_t a, int64_t b) {
    if (b == 0) {
        throw_runtime(&arith_exc_obj, ARITH_MSG);
        return 0;
    }
    if (a == INT64_MIN && b == -1)
        return INT64_MIN;
    return a / b;
}

int64_t jrt_lrem(int64_t a, int64_t b) {
    if (b == 0) {
        throw_runtime(&arith_exc_obj, ARITH_MSG);
        return 0;
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

/* float-Vergleiche/Konvertierungen (analog double). */
int32_t jrt_fcmpl(float a, float b) {
    if (a < b) return -1;
    if (a > b) return 1;
    if (a == b) return 0;
    return -1;
}
int32_t jrt_fcmpg(float a, float b) {
    if (a < b) return -1;
    if (a > b) return 1;
    if (a == b) return 0;
    return 1;
}
int32_t jrt_f2i(float f) {
    if (f != f) return 0;
    if (f >= 2147483647.0f) return INT32_MAX;
    if (f <= -2147483648.0f) return INT32_MIN;
    return (int32_t)f;
}
int64_t jrt_f2l(float f) {
    if (f != f) return 0;
    if (f >= 9223372036854775807.0f) return INT64_MAX;
    if (f <= -9223372036854775808.0f) return INT64_MIN;
    return (int64_t)f;
}
void jrt_print_float(float f) { char b[40]; plat_write(b, (size_t)fmt_g(b, (double)f)); }
void jrt_println_float(float f) { jrt_print_float(f); plat_write("\n", 1); }

void jrt_print_long(int64_t v) { char b[24]; plat_write(b, (size_t)fmt_i64(b, v)); }
void jrt_println_long(int64_t v) { jrt_print_long(v); plat_write("\n", 1); }
/* %g-Näherung; nicht Javas kürzestes rundreisesicheres Format (DESIGN.md §6). */
void jrt_print_double(double d) { char b[40]; plat_write(b, (size_t)fmt_g(b, d)); }
void jrt_println_double(double d) { jrt_print_double(d); plat_write("\n", 1); }
