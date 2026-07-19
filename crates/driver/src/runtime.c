/* fastllvm mini-runtime (platform layer: hosted-libc or freestanding/seL4).
 *
 * Deliberately small (DESIGN.md §6): println intrinsics, Java-semantics helpers
 * for idiv/irem and reference counting (stage 4, DESIGN.md §6/§7). The
 * no_std/seL4 variant replaces stdio/malloc with the primitives available there.
 *
 * Object memory layout (produced by the backend):
 *   { int64_t refcount(packed: flags+count); void *vtable; <fields…> }
 * refcount < 0 ⇒ "immortal" (stack objects, String/Class literals):
 * retain/release are no-ops, it is never freed, and the collector
 * never touches it. rcflags carries the color + buffered bit for the
 * cycle collector. vtable[0] = drop function (releases ref fields),
 * vtable[1] = trace function (visits ref fields with a callback).
 *
 * Memory management: reference counting + synchronous cycle collector after
 * Bacon & Rajan, "Concurrent Cycle Collection in Reference Counted
 * Systems" (2001), section 3 (synchronous variant).
 */
#include <stddef.h>
#include <stdint.h>

/* ====================================================================
 * Platform layer — the ONLY place with OS/libc dependencies.
 * Hosted (default): libc. Freestanding (-DFASTLLVM_FREESTANDING, e.g.
 * seL4): static heap + weak output/halt hooks, no libc.
 * The entire rest of the runtime core calls only plat_, fmt_ and jrt_memcpy.
 * ==================================================================== */

static void *plat_alloc(size_t n);        /* zeroed memory */
static void *plat_realloc(void *p, size_t n);
static void plat_free(void *p);
static void plat_write(const char *s, size_t n); /* bytes → stdout/debug */
static void plat_abort(void);             /* does not return */

/* Portable helpers (without libc). */
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
/* Uncaught message: `Exception in thread "main" <msg>\n` without printf. */
static void plat_uncaught(const char *msg) {
    plat_puts("Exception in thread \"main\" ");
    plat_puts(msg);
    plat_write("\n", 1);
}

/* Signed decimal formatting into buf (>=24 bytes). */
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
/* -------- Freestanding (seL4): no libc ----------------------------- */
/* To be provided by the target environment; weak defaults so it links. */
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

/* Minimal freelist allocator over a static heap. The block header carries
 * the payload size; plat_free() links blocks into a first-fit list. Sufficient for
 * the seL4 bring-up; in production the target environment replaces plat_* with its
 * own allocator. */
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
    /* First-fit in the freelist. */
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
    /* Otherwise from the bump pointer. */
    if (plat_bump + PLAT_HDR + need > FASTLLVM_HEAP_SIZE) return NULL;
    FreeBlock *b = (FreeBlock *)(plat_heap + plat_bump);
    b->size = need;
    plat_bump += PLAT_HDR + need;
    return (uint8_t *)b + PLAT_HDR; /* heap is statically zeroed */
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

/* Minimal %g replacement: sign, integer part, 6 fractional digits. */
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
#include <string.h>
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

/* --- Slab allocator (hosted only) ------------------------------------------
 * Small objects (≤256 B) from segregated size-class pools instead of individually via
 * calloc. Saves the glibc chunk overhead (~8–16 B of metadata PER allocation) and
 * packs equal-sized objects densely (cache + RAM). Slabs are SLAB_SIZE-aligned;
 * `free` finds the slab via `ptr & MASK` and checks it against a hash set of
 * slab bases (safe — no false positive against calloc'd large objects/arrays). Freed
 * cells go into the class freelist (intrusive). Large objects → plat_alloc. */
#ifndef FASTLLVM_FREESTANDING
/* Portable aligned allocator: Linux/BSD/macOS = C11 aligned_alloc,
 * Windows = _aligned_malloc. This way the slab runs on all target OSes. */
#ifdef _WIN32
#include <malloc.h>
static void *plat_aligned(size_t align, size_t size) { return _aligned_malloc(size, align); }
#else
static void *plat_aligned(size_t align, size_t size) { return aligned_alloc(align, size); }
#endif
#define SLAB_SIZE (256u * 1024u)
#define SLAB_MASK (~(uintptr_t)(SLAB_SIZE - 1))
#define SLAB_HDR 16u
#define SLAB_CLASSES 32 /* 8, 16, … 256 B (8-B granularity, hits 40-B objects exactly) */
typedef struct Slab {
    struct Slab *next;
    int64_t cell; /* cell size of this slab */
} Slab;
static void *slab_freelist[SLAB_CLASSES + 1];
static char *slab_cur[SLAB_CLASSES + 1];
static size_t slab_off[SLAB_CLASSES + 1];
static uintptr_t *slab_set; /* hash set of slab bases (open addressing) */
static size_t slab_set_cap, slab_set_len;

static void slab_set_insert(uintptr_t base) {
    if (2 * (slab_set_len + 1) >= slab_set_cap) {
        size_t nc = slab_set_cap ? slab_set_cap * 2 : 256;
        uintptr_t *ns = (uintptr_t *)calloc(nc, sizeof(uintptr_t));
        for (size_t i = 0; i < slab_set_cap; i++) {
            uintptr_t v = slab_set[i];
            if (v) {
                size_t j = (v / SLAB_SIZE) & (nc - 1);
                while (ns[j]) j = (j + 1) & (nc - 1);
                ns[j] = v;
            }
        }
        free(slab_set);
        slab_set = ns;
        slab_set_cap = nc;
    }
    size_t j = (base / SLAB_SIZE) & (slab_set_cap - 1);
    while (slab_set[j]) {
        if (slab_set[j] == base) return;
        j = (j + 1) & (slab_set_cap - 1);
    }
    slab_set[j] = base;
    slab_set_len++;
}
static int slab_set_has(uintptr_t base) {
    if (!slab_set_cap) return 0;
    size_t j = (base / SLAB_SIZE) & (slab_set_cap - 1);
    while (slab_set[j]) {
        if (slab_set[j] == base) return 1;
        j = (j + 1) & (slab_set_cap - 1);
    }
    return 0;
}
static void *slab_alloc(size_t n) {
    if (n == 0) n = 16;
    if (n > (size_t)SLAB_CLASSES * 8) return plat_alloc(n); /* large → calloc */
    int c = (int)((n + 7) / 8);
    size_t cell = (size_t)c * 8;
    void *p = slab_freelist[c];
    if (p) {
        slab_freelist[c] = *(void **)p;
        memset(p, 0, cell);
        return p;
    }
    if (!slab_cur[c] || slab_off[c] + cell > SLAB_SIZE) {
        char *s = (char *)plat_aligned(SLAB_SIZE, SLAB_SIZE);
        if (!s) return plat_alloc(n);
        ((Slab *)s)->next = (Slab *)slab_cur[c];
        ((Slab *)s)->cell = (int64_t)cell;
        slab_set_insert((uintptr_t)s);
        slab_cur[c] = s;
        slab_off[c] = SLAB_HDR;
    }
    p = slab_cur[c] + slab_off[c];
    slab_off[c] += cell;
    memset(p, 0, cell); /* aligned_alloc is NOT zeroed; objects need Java default zero fields */
    return p;
}
static void slab_free(void *p) {
    if (!p) return;
    uintptr_t base = (uintptr_t)p & SLAB_MASK;
    if (slab_set_has(base)) {
        int c = (int)(((Slab *)base)->cell / 8);
        *(void **)p = slab_freelist[c];
        slab_freelist[c] = p;
    } else {
        plat_free(p);
    }
}
#else
#define slab_alloc plat_alloc
#define slab_free plat_free
#endif

void jrt_noop_drop(void *p);
void jrt_noop_trace(void *p, void (*visit)(void *));
void *jrt_alloc(int64_t size);
void jrt_retain(void *p);
void jrt_throw_npe(void);
void jrt_throw_bounds(void);
void jrt_throw_sioobe(void);
static void jrt_sb_drop(void *p);

/* Vtable for strings created at runtime. Its layout (with type descriptor
 * and the Object method slots) is program-dependent, so it is emitted in the
 * generated code as @vt.java_lang_String; @main sets this pointer at startup.
 * String literals reference the same vtable directly. */
void *jrt_dyn_string_vt = NULL;

/* --- Output ---------------------------------------------------------- */

/* String: full object header (so that literals and runtime-created
 * strings are uniformly RC-managed), then length + bytes (UTF-8).
 * Literals are immortal (refcount -1), concatenated strings are not. */
typedef struct {
    int64_t refcount;
    void *vtable;
    int64_t len;
    uint8_t bytes[];
} JStr;

void jrt_print_str(const JStr *s) {
    plat_write((const char *)s->bytes, (size_t)s->len);
}

#ifndef FASTLLVM_FREESTANDING
/* --- Growable list (Vire `list()`), i64 slots --------------------------------
 * Dynamic array of 64-bit slots (int directly / pointer / F64 bits). Not
 * RC-tracked (leaks at the end) — deliberately simple; typed/generic
 * collections will follow with generic types. */
typedef struct {
    int64_t refcount; /* jrt header: immortal (-1) → RC/collector no-op */
    void *vtable;
    int64_t len, cap;
    int64_t *data;
} VList;
/* Raw data pointer of a Vire array: past the 16-byte object header and the 8-byte
 * length + padding, the element storage begins at offset 32 (matches the backend's
 * array element GEP). Lets `@arraydata(a)` hand a Vire array to inline C as a plain
 * element pointer. */
void *jrt_array_data(void *a) { return (char *)a + 32; }
VList *vire_list_new(void) {
    VList *l = (VList *)malloc(sizeof(VList));
    l->refcount = -1;
    l->vtable = 0;
    l->len = 0;
    l->cap = 8;
    l->data = (int64_t *)malloc((size_t)l->cap * sizeof(int64_t));
    return l;
}
void vire_list_push(VList *l, int64_t v) {
    if (l->len == l->cap) {
        l->cap *= 2;
        l->data = (int64_t *)realloc(l->data, (size_t)l->cap * sizeof(int64_t));
    }
    l->data[l->len++] = v;
}
int64_t vire_list_get(VList *l, int64_t i) { return (i >= 0 && i < l->len) ? l->data[i] : 0; }
void vire_list_set(VList *l, int64_t i, int64_t v) { if (i >= 0 && i < l->len) l->data[i] = v; }
int64_t vire_list_len(VList *l) { return l->len; }
int64_t vire_list_pop(VList *l) { return l->len > 0 ? l->data[--l->len] : 0; }

/* --- Map (Int→Int, open addressing) and Set (Int) -------------------------- */
typedef struct {
    int64_t refcount; /* jrt header: immortal */
    void *vtable;
    int64_t cap, len;
    int64_t *keys, *vals;
    uint8_t *used;
} VMap;
static VMap *vmap_new_cap(int64_t cap) {
    VMap *m = (VMap *)malloc(sizeof(VMap));
    m->refcount = -1;
    m->vtable = 0;
    m->cap = cap;
    m->len = 0;
    m->keys = (int64_t *)malloc((size_t)cap * sizeof(int64_t));
    m->vals = (int64_t *)malloc((size_t)cap * sizeof(int64_t));
    m->used = (uint8_t *)calloc((size_t)cap, 1);
    return m;
}
VMap *vire_map_new(void) { return vmap_new_cap(16); }
static void vmap_grow(VMap *m);
void vire_map_put(VMap *m, int64_t k, int64_t v) {
    if ((m->len + 1) * 4 >= m->cap * 3) vmap_grow(m);
    size_t i = (size_t)(k * 2654435761u) & (size_t)(m->cap - 1);
    while (m->used[i]) {
        if (m->keys[i] == k) { m->vals[i] = v; return; }
        i = (i + 1) & (size_t)(m->cap - 1);
    }
    m->used[i] = 1;
    m->keys[i] = k;
    m->vals[i] = v;
    m->len++;
}
static void vmap_grow(VMap *m) {
    VMap *n = vmap_new_cap(m->cap * 2);
    for (int64_t i = 0; i < m->cap; i++)
        if (m->used[i]) vire_map_put(n, m->keys[i], m->vals[i]);
    free(m->keys); free(m->vals); free(m->used);
    *m = *n;
    free(n);
}
int64_t vire_map_get(VMap *m, int64_t k) {
    size_t i = (size_t)(k * 2654435761u) & (size_t)(m->cap - 1);
    while (m->used[i]) {
        if (m->keys[i] == k) return m->vals[i];
        i = (i + 1) & (size_t)(m->cap - 1);
    }
    return 0;
}
int64_t vire_map_has(VMap *m, int64_t k) {
    size_t i = (size_t)(k * 2654435761u) & (size_t)(m->cap - 1);
    while (m->used[i]) {
        if (m->keys[i] == k) return 1;
        i = (i + 1) & (size_t)(m->cap - 1);
    }
    return 0;
}
int64_t vire_map_len(VMap *m) { return m->len; }
int64_t vire_list_contains(VList *l, int64_t v) {
    for (int64_t i = 0; i < l->len; i++)
        if (l->data[i] == v) return 1;
    return 0;
}
void vire_list_clear(VList *l) { l->len = 0; }
/* Remove key k. Backward-shift deletion keeps the linear-probing invariant
 * (no tombstones): after clearing the slot, pull back any following element
 * whose home slot is at or before the hole. Returns 1 if removed. */
int64_t vire_map_remove(VMap *m, int64_t k) {
    size_t mask = (size_t)(m->cap - 1);
    size_t i = (size_t)(k * 2654435761u) & mask;
    while (m->used[i]) {
        if (m->keys[i] == k) {
            size_t j = i;
            m->used[j] = 0;
            m->len--;
            size_t next = (j + 1) & mask;
            while (m->used[next]) {
                size_t home = (size_t)(m->keys[next] * 2654435761u) & mask;
                if (((next - home) & mask) >= ((next - j) & mask)) {
                    m->keys[j] = m->keys[next];
                    m->vals[j] = m->vals[next];
                    m->used[j] = 1;
                    m->used[next] = 0;
                    j = next;
                }
                next = (next + 1) & mask;
            }
            return 1;
        }
        i = (i + 1) & mask;
    }
    return 0;
}

/* FFI: Vire string → NUL-terminated C `char*` (for extern-C functions that
 * expect `const char*`). Copies; the buffer is not freed again
 * (short-lived argument strings) — copy it yourself for lasting use. */
char *vire_cstr(const JStr *s) {
    char *c = (char *)malloc((size_t)s->len + 1);
    if (!c) return (char *)"";
    memcpy(c, s->bytes, (size_t)s->len);
    c[s->len] = 0;
    return c;
}
#endif

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

void jrt_print_bool(int32_t v) { plat_puts(v ? "true" : "false"); }
void jrt_println_bool(int32_t v) { jrt_print_bool(v); plat_write("\n", 1); }
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

/* String methods (byte/ASCII semantics, see frontend comment).
 * NPE/StringIndexOutOfBounds are catchable (pending instead of exit). */
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

/* --- Object root methods (virtual dispatch) ------------------------
 * Default implementations for classes that do not override
 * equals/hashCode/toString, plus the String overrides. */
int32_t jrt_obj_equals(void *a, void *b) {
    return a == b; /* reference identity */
}
int32_t jrt_obj_hashcode(void *o) {
    uintptr_t p = (uintptr_t)o;
    return (int32_t)(p ^ (p >> 32));
}

/* String.hashCode (JLS): s[0]*31^(n-1) + … + s[n-1]. */
int32_t jrt_str_hashcode(const JStr *s) {
    /* Accumulate in uint32_t: unsigned arithmetic has DEFINED wraparound. Signed
     * (int32_t) would be undefined on overflow, and clang -O2/LTO then assumes the
     * result is non-negative (sum of non-negative terms from 0, "no overflow") and
     * folds a caller's `hashCode() & 0x7fffffff` down to `hashCode()` — dropping the
     * mask while the value actually wraps negative at runtime (→ bad array index). */
    uint32_t h = 0;
    for (int64_t i = 0; i < s->len; i++) {
        h = 31u * h + (uint8_t)s->bytes[i];
    }
    return (int32_t)h;
}
/* String.toString returns itself. */
void *jrt_str_tostring(void *s) {
    return s;
}

/* Forward declaration (str_from_buf is defined further below). */
static JStr *str_from_buf(const char *buf, int n);
void *jrt_obj_tostring(void *o) {
    (void)o;
    return str_from_buf("object", 6);
}

/* Further string methods (byte/ASCII semantics). Searching/comparing ones
 * return int/bool; substring/trim/concat return new (RC-managed) strings. */
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

/* --- Wrapper classes (autoboxing) -----------------------------------
 * Integer/Long/Boolean are regular objects (RC-managed) with a
 * boxed primitive value and generated vtable (Object methods).
 * @main sets the vtable pointers at startup (program-dependent layout).
 * No value cache (-128..127) → boxed identity may differ from Java;
 * equals is correct. */
void *jrt_integer_vt = NULL;
void *jrt_long_vt = NULL;
void *jrt_boolean_vt = NULL;

typedef struct {
    int64_t refcount;
    void *vtable;
    int32_t value;
} JInteger;
typedef struct {
    int64_t refcount;
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

/* Boolean uses the same layout as Integer (0/1). */
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
    int64_t refcount;
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

/* Character uses the same layout as Integer (char = i32). */
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
    int64_t refcount;
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

/* Comparable.compareTo for the wrappers (sign of a−b). */
#define CMP(av, bv) (((av) > (bv)) - ((av) < (bv)))
int32_t jrt_integer_compareto(void *a, void *b) { return CMP(((JInteger *)a)->value, ((JInteger *)b)->value); }
int32_t jrt_long_compareto(void *a, void *b) { return CMP(((JLong *)a)->value, ((JLong *)b)->value); }
int32_t jrt_double_compareto(void *a, void *b) { return CMP(((JDouble *)a)->value, ((JDouble *)b)->value); }
int32_t jrt_float_compareto(void *a, void *b) { return CMP(((JFloat *)a)->value, ((JFloat *)b)->value); }
int32_t jrt_character_compareto(void *a, void *b) { return CMP(((JInteger *)a)->value, ((JInteger *)b)->value); }
int32_t jrt_boolean_compareto(void *a, void *b) { return CMP(((JInteger *)a)->value, ((JInteger *)b)->value); }
#undef CMP

/* --- String concatenation (invokedynamic makeConcatWithConstants) ----
 * Strings created at runtime; refcount-managed (not immortal).
 * jrt_alloc (defined further below) sets refcount=1 and tracks it live. */

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
/* Java Double.toString is the shortest round-trip-safe text; we approximate
 * it with %g (documented deviation, DESIGN.md §6). */
JStr *jrt_double_to_str(double d) {
    char buf[32];
    return str_from_buf(buf, fmt_g(buf, d));
}
JStr *jrt_float_to_str(float f) {
    char buf[32];
    return str_from_buf(buf, fmt_g(buf, (double)f));
}

/* --- StringBuilder (runtime-backed) ---------------------------------
 * Growable byte buffer; RC-managed object with its own drop function
 * (frees the buffer). append(X) returns this (chaining). */
void (*jrt_sb_vtable[3])(void) = {
    (void (*)(void))jrt_sb_drop,
    (void (*)(void))jrt_noop_trace,
    (void (*)(void))0, /* no type descriptor */
};
typedef struct {
    int64_t refcount;
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
/* append returns this (chaining); the caller expects a
 * transferred +1 reference → retain. */
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

/* String.format / printf: parses the format string and interprets the
 * Object[] arguments per specifier (%d/%i/%s/%f/%x/%c/%b/%%). Optional
 * flags/width/precision are passed through to snprintf. %s expects a
 * string (byte copy); wrapper values via %d/%f/… (autoboxing in the caller). */
void jrt_retain(void *p);
void jrt_release(void *p);
JStr *jrt_str_format(const JStr *fmt, void *argsp) {
    /* Object[] layout (packed header 16 B): { rc, vtable, length, elem_size,
     * elems… }; length at offset 16, elements from 32. */
    int64_t nargs = argsp ? *(int64_t *)((char *)argsp + 16) : 0;
    void **elems = argsp ? (void **)((char *)argsp + 32) : NULL;
    JSB *sb = (JSB *)jrt_sb_new();
    int ai = 0;
    for (int64_t i = 0; i < fmt->len; i++) {
        char c = fmt->bytes[i];
        if (c != '%') { sb_append(sb, &c, 1); continue; }
        /* Collect the specifier up to the conversion character. */
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
        if (conv == 'n') { /* platform-independent line break, no arg */
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
    jrt_release(sb); /* free the temporary buffer */
    return r;
}
/* StringBuilder(String) constructor: append without retain (return value
 * discarded, the receiver is borrowed). */
void jrt_sb_init_str(void *p, const JStr *s) {
    if (s) sb_append((JSB *)p, s->bytes, s->len);
    else sb_append((JSB *)p, "null", 4);
}

/* --- Reference counting + cycle collector ---------------------------- */

/* PACKED HEADER (16 B instead of 24): `refcount` carries the counter AND collector
 * flags in ONE word — bits 0-1 color, bit 2 buffered, bits 3-62 reference counter,
 * bit 63 (rc<0) = immortal (the fast test, unchanged). Saves 8 B/object (Node 48→40 B,
 * hits the 40-B malloc size class exactly). retain/release shift the
 * counter by RC_SHIFT; the color masks leave the counter bits in place. */
typedef struct {
    int64_t refcount; /* packed: flags bits 0-2, counter bits 3+, immortal <0 */
    void *vtable;     /* [0]=drop(obj), [1]=trace(obj, visit) */
} JObjHeader;

/* Colors after Bacon-Rajan. */
enum { COL_BLACK = 0, COL_GRAY = 1, COL_WHITE = 2, COL_PURPLE = 3 };

#define RC_SHIFT 3            /* lower 3 bits = flags */
#define RC_ONE   ((int64_t)8) /* one reference = 1 << RC_SHIFT */
#define RC_COUNT(h)     ((h)->refcount >> RC_SHIFT)          /* counter value (rc>=0) */
#define COLOR(h)        ((int)((h)->refcount & 3))
#define SET_COLOR(h, c) ((h)->refcount = ((h)->refcount & ~(int64_t)3) | (c))
#define BUFFERED(h)     (((h)->refcount >> 2) & 1)
#define SET_BUFFERED(h, b) \
    ((h)->refcount = ((h)->refcount & ~(int64_t)4) | ((int64_t)(b) << 2))

typedef void (*trace_fn)(void *, void (*)(void *));

static trace_fn trace_of(JObjHeader *h) {
    void (**vt)(void) = (void (**)(void))h->vtable;
    return vt ? (trace_fn)vt[1] : NULL;
}
static void run_drop(JObjHeader *h) {
    void (**vt)(void *) = (void (**)(void *))h->vtable;
    if (vt && vt[0]) vt[0]((void *)h);
}

/* Balance: with balanced refcounting, live_objects must be 0 at the end
 * (even with cycles — the collector clears them). With FASTLLVM_HEAPSTATS the
 * balance is printed at process exit. */
static int64_t total_allocated = 0;
static int64_t live_objects = 0;
/* MEASUREMENT (oracle curve): elide RC on a fraction of the allocations by
 * marking every k-th object immortal (refcount -1) → retain/release become no-ops
 * on it. Models "region inference elides RC on a SUBSET of the sites"
 * (not just 100%). Fraction via env FASTLLVM_RC_ELIDE_PCT (0..100), read once.
 * -1 = uninitialized. Unsound (elided objects leak), only for ceiling timing. */
static int rc_elide_pct = -1;
static uint64_t rc_elide_counter = 0;
/* Counters atomic under threads (otherwise data race on the heap balance). */
#ifdef FASTLLVM_THREADS
#define CNT_INC(x) __atomic_add_fetch(&(x), 1, __ATOMIC_RELAXED)
#define CNT_DEC(x) __atomic_sub_fetch(&(x), 1, __ATOMIC_RELAXED)
#define CNT_POST_INC(x) __atomic_fetch_add(&(x), 1, __ATOMIC_RELAXED)
#else
#define CNT_INC(x) (++(x))
#define CNT_DEC(x) (--(x))
#define CNT_POST_INC(x) ((x)++)
#endif

/* The synchronous cycle collector runs only when it is needed: not
 * under threads (not thread-safe) and not when the solver has proved the
 * program acyclic (phase 1 of the runtime elimination → pure RC). */
#if !defined(FASTLLVM_THREADS) && !defined(FASTLLVM_NO_CYCLES)
#define FASTLLVM_COLLECTOR 1
#endif

static void free_obj(JObjHeader *h) {
    slab_free(h);
    CNT_DEC(live_objects);
}

/* Iterative drop cascade (soundness, M0): recursive release over a large
 * valid object graph blew the stack (crash = "safe" violation). Instead of
 * descending recursively via `jrt_release → run_drop → jrt_release …`, the
 * single-thread paths collect the objects that dropped to 0 in an explicit buffer and
 * process them in a loop — stack depth O(1) instead of O(graph depth). */
#if !defined(FASTLLVM_THREADS)
static JObjHeader **dropbuf = NULL;
static size_t droplen = 0, dropcap = 0;
static int draining = 0;
static void drop_enq(JObjHeader *h) {
    if (droplen == dropcap) {
        dropcap = dropcap ? dropcap * 2 : 256;
        dropbuf = (JObjHeader **)plat_realloc(dropbuf, dropcap * sizeof(*dropbuf));
    }
    dropbuf[droplen++] = h;
}
#endif

#ifdef FASTLLVM_COLLECTOR
/* Candidate roots for the cycle search (purple objects). */
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
            /* Adaptive threshold: a cycle search scans the candidates together with
             * their transitive closure — with a fixed threshold it runs O(n) times
             * on large live sets → O(n²) (M0). Letting the threshold scale with the
             * live set bounds the frequency → amortized linear.
             * Correctness unaffected: the shutdown collect catches everything (0 live). */
            size_t thresh = (size_t)(live_objects > 0 ? live_objects * 2 : 0);
            if (thresh < ROOTS_THRESHOLD) thresh = ROOTS_THRESHOLD;
            if (roots_len >= thresh) jrt_collect_cycles();
        }
    }
}
#endif /* FASTLLVM_COLLECTOR */

static void jrt_shutdown(void) {
#ifdef FASTLLVM_COLLECTOR
    jrt_collect_cycles();
#endif
#ifndef FASTLLVM_FREESTANDING
    /* Leak detector hosted only (getenv/process exit). */
    if (getenv("FASTLLVM_HEAPSTATS")) {
        char b[24];
        plat_puts("[heap] ");
        plat_write(b, (size_t)fmt_i64(b, total_allocated));
        plat_puts(" allocated, ");
        plat_write(b, (size_t)fmt_i64(b, live_objects));
        plat_puts(" still live (cycle leak)\n");
    }
#endif
}

/* --- capsule arena: bump allocator (pure form, scalar-in/scalar-out) ----
 * Between jrt_arena_push/_pop, allocations in the capsule body go into a private
 * arena: immortal (refcount -1 → retain/release/collector no-op), freed en bloc
 * at the end. No pointer may escape (the lowering enforces a scalar result), so
 * no arena object can outlive the arena. Nesting via prev. Hosted only; under
 * threads global (documented limit). */
#ifndef FASTLLVM_FREESTANDING
typedef struct ArenaChunk {
    struct ArenaChunk *prev;
    size_t used, cap;
    /* Data follows in the same malloc block, 16-aligned. */
} ArenaChunk;
typedef struct Arena {
    struct Arena *prev;
    ArenaChunk *chunk;
} Arena;
static Arena *arena_top = NULL;

void jrt_arena_push(void) {
    Arena *a = (Arena *)malloc(sizeof(Arena));
    a->prev = arena_top;
    a->chunk = NULL;
    arena_top = a;
}
void jrt_arena_pop(void) {
    if (!arena_top) return;
    Arena *a = arena_top;
    ArenaChunk *c = a->chunk;
    while (c) {
        ArenaChunk *p = c->prev;
        free(c);
        c = p;
    }
    arena_top = a->prev;
    free(a);
}
static void *arena_alloc(size_t size) {
    size = (size + 15u) & ~(size_t)15u;
    ArenaChunk *c = arena_top->chunk;
    if (!c || c->used + size > c->cap) {
        size_t cap = size > (1u << 16) ? size : (1u << 16);
        /* NO eager memset of the whole chunk: that would force an extra
         * write pass over 64 KB (page faults), whereas calloc hands out zero pages
         * lazily. Instead each object is zeroed individually (below). */
        ArenaChunk *nc = (ArenaChunk *)malloc(sizeof(ArenaChunk) + cap);
        nc->prev = c;
        nc->used = 0;
        nc->cap = cap;
        arena_top->chunk = nc;
        c = nc;
    }
    void *p = (uint8_t *)c + sizeof(ArenaChunk) + c->used;
    c->used += size;
    memset(p, 0, size); /* zero only the object (Java default fields), like calloc */
    return p;
}
#else
void jrt_arena_push(void) {}
void jrt_arena_pop(void) {}
#endif

void *jrt_alloc(int64_t size) {
#ifndef FASTLLVM_FREESTANDING
    /* In the capsule body: arena bump, immortal (RC/collector do not touch it),
     * not in live_objects (freed en bloc by jrt_arena_pop). */
    if (arena_top) {
        void *ap = arena_alloc((size_t)size);
        ((JObjHeader *)ap)->refcount = -1;
        return ap;
    }
#endif
    void *p = slab_alloc((size_t)size);
    if (!p) {
        plat_puts("Exception in thread \"main\" java.lang.OutOfMemoryError\n");
        plat_abort();
    }
    if (CNT_POST_INC(total_allocated) == 0) {
#ifndef FASTLLVM_FREESTANDING
        atexit(jrt_shutdown);
#endif
    }
#ifndef FASTLLVM_FREESTANDING
    /* Oracle curve: make a fraction of the objects immortal (RC elided on them). */
    if (rc_elide_pct != 0) {
        if (rc_elide_pct < 0) {
            const char *e = getenv("FASTLLVM_RC_ELIDE_PCT");
            rc_elide_pct = e ? atoi(e) : 0;
        }
        if (rc_elide_pct > 0 && (int)(rc_elide_counter++ % 100) < rc_elide_pct) {
            ((JObjHeader *)p)->refcount = -1; /* immortal → retain/release no-op */
            return p; /* not counted in live_objects (leaks deliberately) */
        }
    }
#endif
    CNT_INC(live_objects);
    ((JObjHeader *)p)->refcount = RC_ONE; /* the creator holds the first reference */
    return p;
}

#if defined(FASTLLVM_NO_RC)
/* MEASUREMENT MODE (oracle): reference counting completely off — retain/release are no-ops.
 * Models the ceiling of an ideal region/borrow inference that elides ALL RC ops on
 * the provably-stable set. UNSOUND (leaks — nothing is
 * freed), only for ceiling timing. Implies NO_CYCLES (no collector). */
void jrt_retain(void *p) { (void)p; }
void jrt_release(void *p) { (void)p; }
#elif defined(FASTLLVM_THREADS)
/* Threaded: atomic refcounts. Incremental cycle detection is not
 * thread-safe under threads → disabled; acyclic garbage is freed
 * promptly, cycles remain until program end (documented limit,
 * true concurrent collection would be Bacon-Rajan's concurrent variant). */
void jrt_retain(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (__atomic_load_n(&h->refcount, __ATOMIC_RELAXED) < 0) return; /* immortal */
    __atomic_add_fetch(&h->refcount, RC_ONE, __ATOMIC_RELAXED);
}
void jrt_release(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (__atomic_load_n(&h->refcount, __ATOMIC_RELAXED) < 0) return; /* immortal */
    if ((__atomic_sub_fetch(&h->refcount, RC_ONE, __ATOMIC_ACQ_REL) >> RC_SHIFT) == 0) {
        run_drop(h);
        free_obj(h);
    }
}
#elif defined(FASTLLVM_NO_CYCLES)
/* Solver-proven acyclic → pure RC without color/buffer bookkeeping: just
 * inc/dec, free at 0. The cycle collector is not even linked in. */
void jrt_retain(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (h->refcount < 0) return; /* immortal */
    h->refcount += RC_ONE;
}
void jrt_release(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (h->refcount < 0) return; /* immortal */
    h->refcount -= RC_ONE;
    if (RC_COUNT(h) == 0) {
        if (draining) { drop_enq(h); return; }   /* in cascade: just enqueue */
        draining = 1;
        drop_enq(h);
        while (droplen) {
            JObjHeader *x = dropbuf[--droplen];
            run_drop(x);                          /* child release enqueues */
            free_obj(x);
        }
        draining = 0;
    }
}
#else
void jrt_retain(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (h->refcount < 0) return; /* immortal */
    h->refcount += RC_ONE;
    SET_COLOR(h, COL_BLACK);
}

void jrt_release(void *p) {
    if (!p) return;
    JObjHeader *h = (JObjHeader *)p;
    if (h->refcount < 0) return; /* immortal */
    h->refcount -= RC_ONE;
    if (RC_COUNT(h) == 0) {
        /* Release: decrement children (drop), then free if applicable.
         * Iterative (drop buffer) instead of recursive — see drop_enq (soundness).
         * An object that is still buffered stays put — the collector picks
         * it up in MarkRoots (color black, rc 0). */
        if (draining) { drop_enq(h); return; }
        draining = 1;
        drop_enq(h);
        while (droplen) {
            JObjHeader *x = dropbuf[--droplen];
            run_drop(x);
            SET_COLOR(x, COL_BLACK);
            if (!BUFFERED(x)) free_obj(x);
        }
        draining = 0;
    } else {
        possible_root(h);
    }
}
#endif

/* --- Concurrency: monitors + thread ----------------------------------
 * Thread layout (frontend): {header(3 words), $runnable@24, $handle@32}.
 * run() calls the generated trampoline @jrt_invoke_runnable. Under
 * --threads real pthreads + recursive global monitor; otherwise start()
 * runs synchronously (a valid sequential schedule), monitors are no-ops. */
/* Defined by the generated code (strong symbol) IF threads/Runnable are used.
 * Otherwise this weak no-op default applies: normally `--gc-sections` removes
 * the only caller (jrt_thread_start), but under `-fprofile-generate`/PGO it
 * survives → the weak definition satisfies the linker (it is never called
 * without a real thread start). */
__attribute__((weak)) void jrt_invoke_runnable(void *runnable) { (void)runnable; }

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
    jrt_release(runnable); /* the reference taken at start */
    return NULL;
}
void jrt_thread_start(void *thread) {
    if (!thread) return;
    void *runnable = *(void **)((char *)thread + 16);
    jrt_retain(runnable); /* survives until the thread ends */
    pthread_t tid;
    pthread_create(&tid, NULL, thread_tramp, runnable);
    *(int64_t *)((char *)thread + 24) = (int64_t)tid;
}
void jrt_thread_join(void *thread) {
    if (!thread) return;
    pthread_t tid = (pthread_t) * (int64_t *)((char *)thread + 24);
    if (tid) pthread_join(tid, NULL);
}
#else
void jrt_monitor_enter(void *o) { (void)o; }
void jrt_monitor_exit(void *o) { (void)o; }
void jrt_thread_start(void *thread) {
    if (!thread) return;
    /* Without threads: synchronous run — a valid sequential schedule. */
    jrt_invoke_runnable(*(void **)((char *)thread + 16));
}
void jrt_thread_join(void *thread) { (void)thread; }
#endif

/* --- Bacon-Rajan: MarkRoots / ScanRoots / CollectRoots --------------- */
#ifdef FASTLLVM_COLLECTOR

static void mark_gray(JObjHeader *h);
static void scan(JObjHeader *h);
static void scan_black(JObjHeader *h);
static void collect_white(JObjHeader *h);

/* Iterative cycle search (soundness, M0): the Bacon-Rajan traversals ran
 * recursively over the object graph and blew the stack on large valid cycles.
 * Here via explicit worklists — stack depth O(1). `cwork` serves the
 * sequential traversals (mark_gray/scan/collect_white), `bwork` the scan_black
 * nested in `scan`, `fwork` the post-order free. */
static JObjHeader **cwork = NULL; static size_t cwl = 0, cwc = 0;
static JObjHeader **bwork = NULL; static size_t bwl = 0, bwc = 0;
static JObjHeader **fwork = NULL; static size_t fwl = 0, fwc = 0;
static void wl_push(JObjHeader ***buf, size_t *len, size_t *cap, JObjHeader *h) {
    if (*len == *cap) { *cap = *cap ? *cap * 2 : 256; *buf = (JObjHeader **)plat_realloc(*buf, *cap * sizeof(**buf)); }
    (*buf)[(*len)++] = h;
}
/* After a collection the work buffers grow to O(graph size) (for a
 * cycle over N nodes: ~N pointers per buffer). Return large buffers to the
 * allocator AFTER the run, otherwise the collector holds this memory permanently —
 * relevant for long-running programs that repeatedly collect cycles. Small
 * buffers stay (amortizes the re-allocation over more, smaller collections). */
static void trim_buf(JObjHeader ***buf, size_t *len, size_t *cap) {
    if (*cap * sizeof(JObjHeader *) > 64u * 1024u) {
        plat_free(*buf);
        *buf = NULL;
        *len = 0;
        *cap = 0;
    }
}
static void collector_trim(void) {
    trim_buf(&cwork, &cwl, &cwc);
    trim_buf(&bwork, &bwl, &bwc);
    trim_buf(&fwork, &fwl, &fwc);
    trim_buf(&roots, &roots_len, &roots_cap);
    trim_buf(&dropbuf, &droplen, &dropcap);
}

/* --- MarkGray: per edge decrement child, color node gray; iterative. --- */
static void visit_mark_gray(void *p) {
    JObjHeader *h = (JObjHeader *)p;
    if (!h || h->refcount < 0) return;
    h->refcount -= RC_ONE;                    /* trial deletion per edge */
    wl_push(&cwork, &cwl, &cwc, h);
}
static void mark_gray(JObjHeader *root) {
    wl_push(&cwork, &cwl, &cwc, root);
    while (cwl) {
        JObjHeader *h = cwork[--cwl];
        if (COLOR(h) == COL_GRAY) continue;
        SET_COLOR(h, COL_GRAY);
        trace_fn t = trace_of(h);
        if (t) t(h, visit_mark_gray);    /* pushes children onto cwork */
    }
}

/* --- ScanBlack: restore refcounts, color black; own stack (bwork). --- */
static void visit_scan_black(void *p) {
    JObjHeader *h = (JObjHeader *)p;
    if (!h || h->refcount < 0) return;
    h->refcount += RC_ONE;
    wl_push(&bwork, &bwl, &bwc, h);
}
static void scan_black(JObjHeader *root) {
    wl_push(&bwork, &bwl, &bwc, root);
    while (bwl) {
        JObjHeader *h = bwork[--bwl];
        if (COLOR(h) == COL_BLACK) continue;
        SET_COLOR(h, COL_BLACK);
        trace_fn t = trace_of(h);
        if (t) t(h, visit_scan_black);
    }
}

/* --- Scan: gray→white (rc==0) or black (rc>0); iterative over cwork. --- */
static void visit_scan(void *p) {
    JObjHeader *h = (JObjHeader *)p;
    if (!h || h->refcount < 0) return;
    wl_push(&cwork, &cwl, &cwc, h);
}
static void scan(JObjHeader *root) {
    wl_push(&cwork, &cwl, &cwc, root);
    while (cwl) {
        JObjHeader *h = cwork[--cwl];
        if (COLOR(h) != COL_GRAY) continue;
        if (RC_COUNT(h) > 0) {
            scan_black(h);               /* uses bwork, drains fully */
        } else {
            SET_COLOR(h, COL_WHITE);
            trace_fn t = trace_of(h);
            if (t) t(h, visit_scan);
        }
    }
}

/* --- CollectWhite: collect white cycles; post-order via free list. --- */
static void visit_collect_white(void *p) {
    JObjHeader *h = (JObjHeader *)p;
    if (!h || h->refcount < 0) return;
    wl_push(&cwork, &cwl, &cwc, h);
}
static void collect_white(JObjHeader *root) {
    wl_push(&cwork, &cwl, &cwc, root);
    while (cwl) {
        JObjHeader *h = cwork[--cwl];
        if (!(COLOR(h) == COL_WHITE && !BUFFERED(h))) continue;
        SET_COLOR(h, COL_BLACK);
        wl_push(&fwork, &fwl, &fwc, h);  /* free only at the end (post-order) */
        trace_fn t = trace_of(h);
        if (t) t(h, visit_collect_white);
    }
}

static void jrt_collect_cycles(void) {
    /* MarkRoots: mark purple with rc>0 gray; remove the rest from the buffer. */
    size_t kept = 0;
    for (size_t i = 0; i < roots_len; i++) {
        JObjHeader *h = roots[i];
        if (COLOR(h) == COL_PURPLE && RC_COUNT(h) > 0) {
            mark_gray(h);
            roots[kept++] = h;
        } else {
            SET_BUFFERED(h, 0);
            if (COLOR(h) == COL_BLACK && RC_COUNT(h) == 0) free_obj(h);
        }
    }
    roots_len = kept;

    /* ScanRoots. */
    for (size_t i = 0; i < roots_len; i++) scan(roots[i]);

    /* CollectRoots: drain the buffer, collect white cycles (gathered in fwork). */
    for (size_t i = 0; i < roots_len; i++) {
        JObjHeader *h = roots[i];
        SET_BUFFERED(h, 0);
        collect_white(h);
    }
    roots_len = 0;
    /* Post-order free: only after all traversals, so that no trace
     * hits an already-freed cycle member. */
    for (size_t i = 0; i < fwl; i++) free_obj(fwork[i]);
    fwl = 0;
    collector_trim();
}
#endif /* FASTLLVM_COLLECTOR */

void jrt_null_check(const void *p) {
    if (!p) {
        plat_puts("Exception in thread \"main\" java.lang.NullPointerException\n");
        plat_abort();
    }
}

/* --- Exceptions ------------------------------------------------------
 * "pending exception" model (single-thread): jrt_throw sets the pending
 * exception (and holds a reference to it); the generated code checks
 * jrt_pending_set after every throwing call and jumps to the handler or
 * propagates. jrt_take_pending hands the reference to the handler. */
static void *pending_exception = NULL;
/* Message text of a pending runtime exception (sentinel); NULL for
 * user-thrown exceptions. */
static const char *pending_message = NULL;

void jrt_throw(void *e) {
    jrt_retain(e); /* stays alive as long as it is pending */
    pending_exception = e;
    pending_message = NULL;
}

/* Sentinel objects for runtime exceptions (NPE, ArithmeticException, …):
 * immortal headers (refcount -1) with a no-op vtable without a
 * type descriptor. Caught by catch-all (catch Exception / RuntimeException);
 * their message survives until the uncaught output. */
void *jrt_sentinel_vtable[3] = {(void *)jrt_noop_drop, (void *)jrt_noop_trace, NULL};
static JObjHeader arith_exc_obj = {-1, jrt_sentinel_vtable};
static JObjHeader npe_exc_obj = {-1, jrt_sentinel_vtable};
static JObjHeader bounds_exc_obj = {-1, jrt_sentinel_vtable};

/* Called by the runtime checks: set a pending runtime exception. */
static void throw_runtime(void *sentinel, const char *msg) {
    pending_exception = sentinel;
    pending_message = msg;
}

/* Catchable NullPointerException (field/receiver access): set the sentinel
 * in pending; the generated code skips the access and checks
 * pending afterwards. */
void jrt_throw_npe(void) {
    throw_runtime(&npe_exc_obj, "java.lang.NullPointerException");
}
/* The inline bounds check in the generated code sets pending on error via this
 * helper (instead of a jrt_?aload call per access — this way the access stays a
 * visible load/store for LLVM: hoistable, vectorizable). */
void jrt_throw_bounds(void) {
    throw_runtime(&bounds_exc_obj, "java.lang.ArrayIndexOutOfBoundsException");
}
void jrt_throw_sioobe(void) {
    throw_runtime(&bounds_exc_obj, "java.lang.StringIndexOutOfBoundsException");
}

/* Throwable.getMessage(): reads the $message field (first instance field of
 * java.lang.Throwable → offset 3 words). Runtime sentinels (Arith/NPE/…)
 * have no type descriptor (vt[2]==NULL) and no such field → null.
 * Return value retained (+1 for the caller, owning-slot model). */
void *jrt_throwable_message(void *obj) {
    if (!obj) {
        throw_runtime(&npe_exc_obj, "java.lang.NullPointerException");
        return NULL;
    }
    void **vt = (void **)((JObjHeader *)obj)->vtable;
    if (!vt || vt[2] == NULL) return NULL; /* sentinel without message field */
    void *msg = *(void **)((char *)obj + 16);
    jrt_retain(msg);
    return msg;
}
int32_t jrt_pending_set(void) {
    return pending_exception != NULL;
}
/* Hands the pending reference to the caller (handler) and clears
 * the flag — no retain/release, the +1 is transferred. */
void *jrt_take_pending(void) {
    void *e = pending_exception;
    pending_exception = NULL;
    return e;
}
/* instanceof: walks the object's type-descriptor chain and compares
 * against the target descriptor. Vtable slot 2 is the type descriptor;
 * { ptr super }. Immortal objects without a descriptor (slot 2 null) → false. */
typedef struct TypeDesc {
    struct TypeDesc *super;
    const char *cname; /* dotted class name for the uncaught message */
    void *jclass;      /* Class-object singleton of this class (reflection) */
    struct TypeDesc **ifaces; /* null-terminated: transitive interfaces (instanceof) */
} TypeDesc;

/* Reflection: obj.getClass() → the Class singleton via the type descriptor.
 * getName/getSimpleName read the JStr fields of the Class object (layout:
 * {refcount,rcflags,vtable,name,simpleName} → offsets 24/32). */
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
    return *(void **)((char *)jc + 16);
}
void *jrt_class_getsimplename(void *jc) {
    if (!jc) {
        jrt_throw_npe();
        return NULL;
    }
    return *(void **)((char *)jc + 24);
}

/* Record equals: the caller already has instanceof(other, RecordClass) in
 * `inst` (0 for null/wrong type). On matching type, compare the field range
 * (from offset 16, packed header) via memcmp. Ref fields are compared by identity
 * here (documented limit); primitive records are exact. */
static int jrt_memcmp(const void *a, const void *b, int64_t n) {
    const unsigned char *x = a, *y = b;
    for (int64_t i = 0; i < n; i++)
        if (x[i] != y[i]) return 1;
    return 0;
}
int32_t jrt_record_memeq(void *a, void *b, int32_t inst, int64_t field_bytes) {
    if (!inst) return 0;
    if (a == b) return 1;
    return jrt_memcmp((char *)a + 16, (char *)b + 16, field_bytes) == 0;
}

int32_t jrt_instanceof(void *obj, void *target_td) {
    if (!obj) return 0;
    JObjHeader *h = (JObjHeader *)obj;
    void **vt = (void **)h->vtable;
    if (!vt) return 0;
    TypeDesc *td = (TypeDesc *)vt[2];
    /* Interface check: the concrete class carries the transitive interface set. */
    if (td && td->ifaces) {
        for (TypeDesc **p = td->ifaces; *p; p++) {
            if ((void *)*p == target_td) return 1;
        }
    }
    /* Class chain (superclasses). */
    while (td) {
        if ((void *)td == target_td) return 1;
        td = td->super;
    }
    return 0;
}

/* Checks the pending exception against a catch type (dispatch cascade). */
int32_t jrt_pending_instanceof(void *target_td) {
    return jrt_instanceof(pending_exception, target_td);
}

/* Runtime checkcast to a modeled class: null always passes,
 * otherwise the type must match (ClassCastException = abort). */
void jrt_checkcast(void *obj, void *target_td) {
    if (obj && !jrt_instanceof(obj, target_td)) {
        plat_puts("Exception in thread \"main\" java.lang.ClassCastException\n");
        plat_abort();
    }
}

/* Called by @main after java_main: report an unhandled exception.
 * (Generic without runtime type info — class name/message would be a
 * later step, DESIGN.md §6.) */
void jrt_check_uncaught(void) {
    if (!pending_exception) return;
    if (pending_message) {
        /* Runtime exception (sentinel) with a ready message text. */
        plat_uncaught(pending_message);
    } else {
        /* User exception: class name from the type descriptor. */
        JObjHeader *h = (JObjHeader *)pending_exception;
        void **vt = (void **)h->vtable;
        TypeDesc *td = vt ? (TypeDesc *)vt[2] : NULL;
        if (td && td->cname) {
            plat_uncaught(td->cname);
        } else {
            plat_puts("Exception in thread \"main\" (unhandled exception)\n");
        }
    }
    plat_abort();
}

/* --- Arrays ---------------------------------------------------------- */

/* Same header as objects, then length + element size (for arraycopy/
 * clone without a static type); elements follow directly (from offset 40). */
typedef struct {
    int64_t refcount;
    void *vtable;
    int64_t length;
    int64_t elem_size;
} JArray;

void *jrt_alloc_array(int64_t count, int64_t elem_size, void *vtable) {
    if (count < 0) {
        plat_puts("Exception in thread \"main\" java.lang.NegativeArraySizeException\n");
        plat_abort();
    }
#ifndef FASTLLVM_FREESTANDING
    /* In the capsule/loop arena: bump-allocate the array too (immortal, not tracked
     * in live_objects, freed en bloc by jrt_arena_pop). Same soundness gate as
     * jrt_alloc: promotion only happens where the lowering proved no escape, so no
     * arena array can outlive the arena. arena_alloc already zeroes the block. */
    if (arena_top) {
        JArray *aa = (JArray *)arena_alloc(sizeof(JArray) + (size_t)count * (size_t)elem_size);
        aa->refcount = -1;
        aa->vtable = vtable;
        aa->length = count;
        aa->elem_size = elem_size;
        return aa;
    }
#endif
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
    a->refcount = RC_ONE;
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

/* Catchable array accesses: check + access encapsulated so they can throw via
 * the pending model (NPE/ArrayIndexOutOfBounds) instead of aborting.
 * On error a safe default is returned; the generated code checks
 * pending afterwards and jumps to the handler or propagates. */
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
/* Narrow primitive arrays (byte/boolean=1B, char/short=2B). Load widens
 * to int (byte/short sign-extended, bool/char zero-extended), store truncates. */
int32_t jrt_baload(void *arr, int32_t i) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return 0;
    return ((int8_t *)(a + 1))[i];
}
void jrt_bastore(void *arr, int32_t i, int32_t v) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return;
    ((int8_t *)(a + 1))[i] = (int8_t)v;
}
int32_t jrt_caload(void *arr, int32_t i) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return 0;
    return ((uint16_t *)(a + 1))[i];
}
void jrt_castore(void *arr, int32_t i, int32_t v) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return;
    ((uint16_t *)(a + 1))[i] = (uint16_t)v;
}
int32_t jrt_saload(void *arr, int32_t i) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return 0;
    return ((int16_t *)(a + 1))[i];
}
void jrt_sastore(void *arr, int32_t i, int32_t v) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return;
    ((int16_t *)(a + 1))[i] = (int16_t)v;
}
/* Typed primitive arrays (long/double/float). */
int64_t jrt_laload(void *arr, int32_t i) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return 0;
    return ((int64_t *)(a + 1))[i];
}
void jrt_lastore(void *arr, int32_t i, int64_t v) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return;
    ((int64_t *)(a + 1))[i] = v;
}
double jrt_daload(void *arr, int32_t i) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return 0.0;
    return ((double *)(a + 1))[i];
}
void jrt_dastore(void *arr, int32_t i, double v) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return;
    ((double *)(a + 1))[i] = v;
}
float jrt_faload(void *arr, int32_t i) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return 0.0f;
    return ((float *)(a + 1))[i];
}
void jrt_fastore(void *arr, int32_t i, float v) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return;
    ((float *)(a + 1))[i] = v;
}
void *jrt_aaload(void *arr, int32_t i) {
    JArray *a = (JArray *)arr;
    if (!arr_ok(a, i)) return NULL;
    return ((void **)(a + 1))[i]; /* borrowed; caller retains */
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

/* Drop/trace for ref[]: iterate over the elements. */
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

/* int[] has no ref elements. */
void jrt_noop_drop(void *p) { (void)p; }
void jrt_noop_trace(void *p, void (*visit)(void *)) { (void)p; (void)visit; }

/* enum valueOf: iterate over the values array and find the element with matching
 * $name (first instance field, offset 3 words). Return value retained (+1
 * for the caller); the array itself remains owned by the caller and is
 * freed by its owning-slot cleanup. */
int32_t jrt_str_equals(const JStr *a, const JStr *b);
void *jrt_enum_valueof(void *values, void *name) {
    JArray *a = (JArray *)values;
    void *found = NULL;
    void **elems = (void **)(a + 1);
    for (int64_t i = 0; i < a->length; i++) {
        void *e = elems[i];
        if (!e) continue;
        JStr *ename = *(JStr **)((char *)e + 16);
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

/* Shallow copy of an array (among other things for enum values()). Same vtable and
 * length; for ref arrays each copied element is retained. */
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

/* System.arraycopy: element-size/ref-correct (elem_size + ref vtable in the
 * header). Overlap-safe (memmove semantics); ref arrays retain the
 * source before freeing the destination. Errors abort (not catchable). */
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
        for (int32_t i = 0; i < len; i++) jrt_retain(se[srcPos + i]);   /* secure the source */
        for (int32_t i = 0; i < len; i++) jrt_release(de[dstPos + i]);  /* free the destination */
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

/* --- Number parsing / math / time (runtime intrinsics) -------------- */

/* Integer.parseInt / Long.parseLong (byte/ASCII semantics). Invalid
 * input aborts (NumberFormatException not catchable). */
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
/* Portable sqrt (Newton-Raphson) — libm-free, also freestanding. */
double jrt_math_sqrt(double x) {
    if (x < 0) return 0.0 / 0.0; /* NaN */
    if (x == 0.0) return 0.0;
    double g = x > 1.0 ? x : 1.0;
    for (int i = 0; i < 60; i++) g = 0.5 * (g + x / g);
    return g;
}

/* Time: hosted real monotonicity, freestanding via a weak hook (default 0). */
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

/* --- Java arithmetic semantics --------------------------------------- */

/* JLS 15.17.2: division by 0 throws ArithmeticException (now catchable
 * via the pending model); INT_MIN / -1 is defined as INT_MIN. */
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

/* dcmpl/dcmpg differ only for NaN (JVMS 6.5): dcmpl → -1,
 * dcmpg → 1, so that NaN comparisons always yield "false". */
int32_t jrt_dcmpl(double a, double b) {
    if (a < b) return -1;
    if (a > b) return 1;
    if (a == b) return 0;
    return -1; /* at least one NaN */
}
int32_t jrt_dcmpg(double a, double b) {
    if (a < b) return -1;
    if (a > b) return 1;
    if (a == b) return 0;
    return 1; /* at least one NaN */
}

/* d2i/d2l saturate (JLS 5.1.3): NaN → 0, out of range clamped to
 * MIN/MAX. */
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

/* float comparisons/conversions (analogous to double). */
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
/* %g approximation; not Java's shortest round-trip-safe format (DESIGN.md §6). */
void jrt_print_double(double d) { char b[40]; plat_write(b, (size_t)fmt_g(b, d)); }
void jrt_println_double(double d) { jrt_print_double(d); plat_write("\n", 1); }
