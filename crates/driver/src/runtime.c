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
void *jrt_noop_copy(void *src, void *map); /* deep-copy vtable slot-3 stub */
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

/* --- Set (Int) --------------------------------------------------------------
 * A hash set is a map whose values are ignored, so it reuses VMap's tested
 * open-addressing + backward-shift delete verbatim (dummy value 1). The `$Set`
 * sentinel in the frontend keeps its method surface (add/contains/remove/len)
 * distinct from a map's. */
VMap *vire_set_new(void) { return vmap_new_cap(16); }
void vire_set_add(VMap *m, int64_t k) { vire_map_put(m, k, 1); }
int64_t vire_set_contains(VMap *m, int64_t k) { return vire_map_has(m, k); }
int64_t vire_set_remove(VMap *m, int64_t k) { return vire_map_remove(m, k); }
int64_t vire_set_len(VMap *m) { return m->len; }

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

/* Forward declarations (defined further below). */
static JStr *str_from_buf(const char *buf, int n);
static JStr *str_alloc(int64_t len);
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
/* ASCII case folding → new (RC-managed) strings (str_alloc / str_from_buf are
 * declared above). Non-ASCII bytes pass through unchanged. */
void *jrt_str_lower(const JStr *s) {
    if (!s) { jrt_throw_npe(); return NULL; }
    JStr *r = str_alloc(s->len);
    for (int64_t i = 0; i < s->len; i++) {
        uint8_t c = s->bytes[i];
        r->bytes[i] = (c >= 'A' && c <= 'Z') ? (uint8_t)(c + 32) : c;
    }
    return r;
}
void *jrt_str_upper(const JStr *s) {
    if (!s) { jrt_throw_npe(); return NULL; }
    JStr *r = str_alloc(s->len);
    for (int64_t i = 0; i < s->len; i++) {
        uint8_t c = s->bytes[i];
        r->bytes[i] = (c >= 'a' && c <= 'z') ? (uint8_t)(c - 32) : c;
    }
    return r;
}
/* JSON string escaping (RFC 8259): ", \, and the C0 control chars become escape
 * sequences (\", \\, \n, \r, \t, \b, \f, else \u00XX) → a new RC-managed string.
 * Two passes: measure the escaped length, allocate once, then fill. Used by
 * `@derive(Json)` so a Str field with quotes/newlines yields valid JSON. */
void *jrt_str_json_escape(const JStr *s) {
    if (!s) { jrt_throw_npe(); return NULL; }
    int64_t out = 0;
    for (int64_t i = 0; i < s->len; i++) {
        uint8_t c = s->bytes[i];
        switch (c) {
            case '"': case '\\': case '\n': case '\r':
            case '\t': case '\b': case '\f': out += 2; break;
            default: out += (c < 0x20) ? 6 : 1; /* \u00XX */
        }
    }
    JStr *r = str_alloc(out);
    static const char hex[] = "0123456789abcdef";
    int64_t j = 0;
    for (int64_t i = 0; i < s->len; i++) {
        uint8_t c = s->bytes[i];
        switch (c) {
            case '"':  r->bytes[j++] = '\\'; r->bytes[j++] = '"';  break;
            case '\\': r->bytes[j++] = '\\'; r->bytes[j++] = '\\'; break;
            case '\n': r->bytes[j++] = '\\'; r->bytes[j++] = 'n';  break;
            case '\r': r->bytes[j++] = '\\'; r->bytes[j++] = 'r';  break;
            case '\t': r->bytes[j++] = '\\'; r->bytes[j++] = 't';  break;
            case '\b': r->bytes[j++] = '\\'; r->bytes[j++] = 'b';  break;
            case '\f': r->bytes[j++] = '\\'; r->bytes[j++] = 'f';  break;
            default:
                if (c < 0x20) {
                    r->bytes[j++] = '\\'; r->bytes[j++] = 'u';
                    r->bytes[j++] = '0'; r->bytes[j++] = '0';
                    r->bytes[j++] = hex[(c >> 4) & 0xf];
                    r->bytes[j++] = hex[c & 0xf];
                } else {
                    r->bytes[j++] = c;
                }
        }
    }
    return r;
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
void (*jrt_sb_vtable[4])(void) = {
    (void (*)(void))jrt_sb_drop,
    (void (*)(void))jrt_noop_trace,
    (void (*)(void))0, /* no type descriptor */
    (void (*)(void))jrt_noop_copy, /* deep-copy (slot 3) */
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
/* Free-cascade budget: bound the frees done per top-level release so dropping a
 * large dead subgraph is spread across operations, not one synchronous burst (a
 * latency spike). Most cascades are ≤ FREE_BUDGET and still complete in one release
 * (no deferral, no RAM overhead); only a huge cascade spreads. FREE_PUMP is drained
 * per allocation to keep the queue draining. The queue is LIFO (depth-first), so it
 * only ever holds the DFS frontier (~depth), not the whole subgraph. */
#define FREE_BUDGET 4096
#define FREE_PUMP 64
static void drop_enq(JObjHeader *h) {
    if (droplen == dropcap) {
        dropcap = dropcap ? dropcap * 2 : 256;
        dropbuf = (JObjHeader **)plat_realloc(dropbuf, dropcap * sizeof(*dropbuf));
    }
    dropbuf[droplen++] = h;
}
/* Run up to `budget` deferred drops (0 = drain fully). Sound: queued objects are
 * rc==0 (unreachable — the mutator cannot obtain a new reference to one), so
 * deferring their free is invisible; their children stay alive (still held by the
 * not-yet-run drop) until their turn. Re-entrant child releases only enqueue. */
static void drain_drops(size_t budget) {
    if (draining) return;
    draining = 1;
    size_t n = 0;
    while (droplen && (budget == 0 || n < budget)) {
        JObjHeader *x = dropbuf[--droplen];
        run_drop(x); /* child releases enqueue (draining=1) */
#ifdef FASTLLVM_COLLECTOR
        SET_COLOR(x, COL_BLACK);
        if (!BUFFERED(x)) free_obj(x);
#else
        free_obj(x);
#endif
        n++;
    }
    draining = 0;
}
#endif

#ifdef FASTLLVM_COLLECTOR
/* Candidate roots for the cycle search (purple objects). */
static JObjHeader **roots = NULL;
static size_t roots_len = 0, roots_cap = 0;
/* Incremental collection: each step drains a small tail batch (+ its connected
 * garbage component) so the buffer stays small — continuous, bounded per-step pause
 * (no accumulate-then-big-pass spike), low steady RAM. A step runs when the buffer
 * exceeds the soft cap. */
#define COLLECT_BATCH 256
#define ROOTS_SOFT_CAP 1024

static void jrt_collect_step(void);   /* one bounded incremental step */
static void jrt_collect_cycles(void); /* full drain (shutdown): steps until empty */

static void roots_push(JObjHeader *h) {
    if (roots_len == roots_cap) {
        roots_cap = roots_cap ? roots_cap * 2 : 64;
        roots = (JObjHeader **)plat_realloc(roots, roots_cap * sizeof(*roots));
    }
    roots[roots_len++] = h;
}

/* Deferred free queue for COLLECTED cycle garbage (free-only — the members' drops are
 * not run; their refs are internal to the freed cycle, handled by the mark/scan
 * accounting). Draining a large collected component here (a bounded amount per step,
 * per allocation, and fully at shutdown) spreads the free_obj cost — the dominant part
 * of reclaiming a big garbage cycle — so it is not one synchronous burst. Sound:
 * collected garbage is unreachable (immutable) and already removed from the root
 * buffer, so nothing references it before its deferred free. */
static JObjHeader **gbuf = NULL;
static size_t gbl = 0, gbc = 0;
static void gb_push(JObjHeader *h) {
    if (gbl == gbc) {
        gbc = gbc ? gbc * 2 : 256;
        gbuf = (JObjHeader **)plat_realloc(gbuf, gbc * sizeof(*gbuf));
    }
    gbuf[gbl++] = h;
}
static void drain_gbuf(size_t budget) {
    size_t n = 0;
    while (gbl && (budget == 0 || n < budget)) { free_obj(gbuf[--gbl]); n++; }
}

static void possible_root(JObjHeader *h) {
    if (COLOR(h) != COL_PURPLE) {
        SET_COLOR(h, COL_PURPLE);
        if (!BUFFERED(h)) {
            SET_BUFFERED(h, 1);
            roots_push(h);
            /* Incremental: drain one bounded batch when the buffer exceeds the soft
             * cap, keeping it small. Each candidate is processed once (no O(n²)
             * re-scan of the live set); the shutdown drain catches the rest. */
            if (roots_len >= ROOTS_SOFT_CAP) jrt_collect_step();
        }
    }
}
#endif /* FASTLLVM_COLLECTOR */

static void jrt_shutdown(void) {
#if !defined(FASTLLVM_THREADS)
    drain_drops(0); /* flush any deferred free-cascade (may buffer cyclic roots) */
#endif
#ifdef FASTLLVM_COLLECTOR
    jrt_collect_cycles();
    drain_gbuf(0); /* free all deferred collected-cycle garbage → 0 live */
#endif
#if !defined(FASTLLVM_THREADS)
    drain_drops(0); /* and anything the collect surfaced → 0 live */
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

/* --- capsule/loop arena: bump allocator (a second, region stack) -------------
 * Between jrt_arena_push/_pop, allocations in the body go into a private arena:
 * immortal (refcount -1 → retain/release/collector no-op), freed en bloc at the
 * end. Non-escaping by construction (the escape analysis / lowering proves it),
 * so no arena object outlives the arena. Nesting via prev.
 *
 * SCALES TO MULTIPLE STACKS: `arena_top` is thread-local under threads, so each
 * thread owns an independent region stack — concurrent `spawn` workers running
 * arena-promoted loops no longer share (and race on) one global region. Without
 * threads (or freestanding) it is a single global stack. Hosted only. */
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
#define ARENA_CHUNK_DEFAULT (1u << 16) /* 64 KiB standard chunk */
#define ARENA_POOL_MAX 64              /* recycle up to 64 chunks (≈4 MiB) per thread */

#ifdef FASTLLVM_THREADS
static _Thread_local Arena *arena_top = NULL; /* one region stack per thread */
static _Thread_local ArenaChunk *arena_pool = NULL; /* recycled 64 KiB chunks */
static _Thread_local int arena_pool_count = 0;
#else
static Arena *arena_top = NULL;
static ArenaChunk *arena_pool = NULL; /* free-list of recycled default chunks */
static int arena_pool_count = 0;
#endif

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
        /* Recycle standard chunks into a capped pool instead of returning them to
         * the OS: turns the pop from an O(chunks) free() burst into O(chunks)
         * pointer splices, and lets the next capsule reuse the memory (removes the
         * per-arena malloc + zeroing fixed cost). Oversized/overflow chunks are
         * still freed so the pool stays bounded. */
        if (c->cap == ARENA_CHUNK_DEFAULT && arena_pool_count < ARENA_POOL_MAX) {
            c->prev = arena_pool;
            arena_pool = c;
            arena_pool_count++;
        } else {
            free(c);
        }
        c = p;
    }
    arena_top = a->prev;
    free(a);
}
static void *arena_alloc(size_t size) {
    size = (size + 15u) & ~(size_t)15u;
    ArenaChunk *c = arena_top->chunk;
    if (!c || c->used + size > c->cap) {
        ArenaChunk *nc;
        if (size <= ARENA_CHUNK_DEFAULT && arena_pool) {
            /* Reuse a recycled chunk (cap is exactly ARENA_CHUNK_DEFAULT ≥ size). */
            nc = arena_pool;
            arena_pool = nc->prev;
            arena_pool_count--;
            nc->used = 0;
        } else {
            size_t cap = size > ARENA_CHUNK_DEFAULT ? size : ARENA_CHUNK_DEFAULT;
            /* NO eager memset of the whole chunk: that would force an extra
             * write pass over 64 KB (page faults), whereas calloc hands out zero pages
             * lazily. Instead each object is zeroed individually (below). */
            nc = (ArenaChunk *)malloc(sizeof(ArenaChunk) + cap);
            nc->used = 0;
            nc->cap = cap;
        }
        nc->prev = c;
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
#if !defined(FASTLLVM_THREADS)
    /* Amortize the deferred free-cascade against allocation: each alloc pays off a
     * few pending drops, so deferred frees don't accumulate (RAM stays bounded
     * relative to the allocation rate). */
    if (droplen) drain_drops(FREE_PUMP);
#endif
#ifdef FASTLLVM_COLLECTOR
    if (gbl) drain_gbuf(FREE_PUMP); /* likewise for deferred collected-cycle garbage */
#endif
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
        drop_enq(h);
        drain_drops(FREE_BUDGET); /* bounded; re-entrant child releases just enqueue */
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
        /* Release: decrement children (drop), then free — iterative (drop buffer)
         * and BUDGETED so a large dead subgraph frees across operations, not in one
         * burst (see drain_drops). A still-buffered object stays put — the collector
         * picks it up in MarkRoots (color black, rc 0). */
        drop_enq(h);
        drain_drops(FREE_BUDGET);
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

/* --- Vire concurrency: spawn/join + Atomic ---------------------------------
 * A first-class, function-pointer thread model for Vire `spawn f(arg)` (distinct
 * from the Java Runnable/vtable path above). `jrt_spawn` takes a worker
 * `long(void*)` and its single argument, runs it on a pthread, and returns an
 * opaque handle; `jrt_join` waits and yields the worker's return value.
 *
 * The handle/Atomic/Mutex objects carry a jrt object header with refcount = -1
 * (immortal → retain/release are no-ops and they are NOT tracked in
 * live_objects), exactly like the Vire `list()`/`map()` objects: RC-safe when
 * held in a Vire ref local, and the heap-balance oracle stays at 0-live. They
 * are small and freed at process exit (documented, like the global monitor).
 *
 * Under FASTLLVM_THREADS the RC path is already atomic (see jrt_retain above),
 * so shared refcounted state crossing threads is safe; without it, `spawn` runs
 * the worker synchronously (a valid sequential schedule) and Atomic is a plain
 * counter. */
#ifndef FASTLLVM_FREESTANDING /* needs malloc/pthreads — hosted only */
typedef struct {
    int64_t refcount; /* jrt header: immortal */
    void *vtable;
    int64_t (*fn)(void *);
    void *arg;
    int64_t result;
    int64_t tid;
} VThread;

typedef struct {
    int64_t refcount;
    void *vtable;
    int64_t val;
} VAtomic;

#if defined(FASTLLVM_THREADS) && !defined(FASTLLVM_FREESTANDING)
static void *vthread_tramp(void *p) {
    VThread *t = (VThread *)p;
    t->result = t->fn(t->arg);
    return NULL;
}
void *jrt_spawn(int64_t (*fn)(void *), void *arg) {
    VThread *t = (VThread *)malloc(sizeof(VThread));
    t->refcount = -1;
    t->vtable = 0;
    t->fn = fn;
    t->arg = arg;
    t->result = 0;
    pthread_t tid;
    pthread_create(&tid, NULL, vthread_tramp, t);
    t->tid = (int64_t)tid;
    return t;
}
int64_t jrt_join(void *h) {
    if (!h) return 0;
    VThread *t = (VThread *)h;
    if (t->tid) pthread_join((pthread_t)t->tid, NULL);
    t->tid = 0;
    return t->result;
}
int64_t jrt_atomic_add(void *a, int64_t d) {
    return __atomic_fetch_add(&((VAtomic *)a)->val, d, __ATOMIC_SEQ_CST);
}
int64_t jrt_atomic_get(void *a) {
    return __atomic_load_n(&((VAtomic *)a)->val, __ATOMIC_SEQ_CST);
}
typedef struct {
    int64_t refcount; /* jrt header: immortal */
    void *vtable;
    int64_t val;
    pthread_mutex_t m;
} VMutex;
void *jrt_mutex_new(int64_t v) {
    VMutex *x = (VMutex *)malloc(sizeof(VMutex));
    x->refcount = -1;
    x->vtable = 0;
    x->val = v;
    pthread_mutex_init(&x->m, NULL);
    return x;
}
void jrt_mutex_lock(void *p) { pthread_mutex_lock(&((VMutex *)p)->m); }
void jrt_mutex_unlock(void *p) { pthread_mutex_unlock(&((VMutex *)p)->m); }
int64_t jrt_mutex_get(void *p) { return ((VMutex *)p)->val; }
void jrt_mutex_set(void *p, int64_t v) { ((VMutex *)p)->val = v; }
/* parallel_for(n, shared, worker): fork n threads, worker(i, shared) for
 * i in 0..n, join all. shared is a Sync object (Atomic/Mutex). */
typedef struct {
    int64_t i;
    void *shared;
    int64_t (*fn)(int64_t, void *);
} PForArg;
static void *pfor_tramp(void *p) {
    PForArg *a = (PForArg *)p;
    a->fn(a->i, a->shared);
    return NULL;
}
void jrt_parallel_for(int64_t n, void *shared, int64_t (*fn)(int64_t, void *)) {
    if (n <= 0) return;
    pthread_t *tids = (pthread_t *)malloc((size_t)n * sizeof(pthread_t));
    PForArg *args = (PForArg *)malloc((size_t)n * sizeof(PForArg));
    for (int64_t i = 0; i < n; i++) {
        args[i].i = i;
        args[i].shared = shared;
        args[i].fn = fn;
        pthread_create(&tids[i], NULL, pfor_tramp, &args[i]);
    }
    for (int64_t i = 0; i < n; i++) pthread_join(tids[i], NULL);
    free(args);
    free(tids);
}
/* Channel[Int]: a thread-safe FIFO queue of i64 values (mutex + condvar).
 * send enqueues + signals; recv blocks until an item is available. The safe
 * message-passing primitive between spawned workers. */
typedef struct ChNode {
    struct ChNode *next;
    int64_t val;
} ChNode;
typedef struct {
    int64_t refcount; /* immortal header */
    void *vtable;
    pthread_mutex_t m;
    pthread_cond_t cv;
    ChNode *head, *tail;
} VChan;
void *jrt_chan_new(void) {
    VChan *c = (VChan *)malloc(sizeof(VChan));
    c->refcount = -1;
    c->vtable = 0;
    pthread_mutex_init(&c->m, NULL);
    pthread_cond_init(&c->cv, NULL);
    c->head = c->tail = NULL;
    return c;
}
void jrt_chan_send(void *p, int64_t v) {
    VChan *c = (VChan *)p;
    ChNode *n = (ChNode *)malloc(sizeof(ChNode));
    n->next = NULL;
    n->val = v;
    pthread_mutex_lock(&c->m);
    if (c->tail) c->tail->next = n;
    else c->head = n;
    c->tail = n;
    pthread_cond_signal(&c->cv);
    pthread_mutex_unlock(&c->m);
}
int64_t jrt_chan_recv(void *p) {
    VChan *c = (VChan *)p;
    pthread_mutex_lock(&c->m);
    while (!c->head) pthread_cond_wait(&c->cv, &c->m);
    ChNode *n = c->head;
    c->head = n->next;
    if (!c->head) c->tail = NULL;
    int64_t v = n->val;
    pthread_mutex_unlock(&c->m);
    free(n);
    return v;
}
#else
void *jrt_spawn(int64_t (*fn)(void *), void *arg) {
    /* No threads: run synchronously now, stash the result for jrt_join. */
    VThread *t = (VThread *)malloc(sizeof(VThread));
    t->refcount = -1;
    t->vtable = 0;
    t->fn = fn;
    t->arg = arg;
    t->tid = 0;
    t->result = fn(arg);
    return t;
}
int64_t jrt_join(void *h) { return h ? ((VThread *)h)->result : 0; }
int64_t jrt_atomic_add(void *a, int64_t d) {
    int64_t old = ((VAtomic *)a)->val;
    ((VAtomic *)a)->val = old + d;
    return old;
}
int64_t jrt_atomic_get(void *a) { return ((VAtomic *)a)->val; }
/* Single-threaded: the lock is a no-op (a valid sequential schedule). */
typedef struct {
    int64_t refcount;
    void *vtable;
    int64_t val;
} VMutex;
void *jrt_mutex_new(int64_t v) {
    VMutex *x = (VMutex *)malloc(sizeof(VMutex));
    x->refcount = -1;
    x->vtable = 0;
    x->val = v;
    return x;
}
void jrt_mutex_lock(void *p) { (void)p; }
void jrt_mutex_unlock(void *p) { (void)p; }
int64_t jrt_mutex_get(void *p) { return ((VMutex *)p)->val; }
void jrt_mutex_set(void *p, int64_t v) { ((VMutex *)p)->val = v; }
void jrt_parallel_for(int64_t n, void *shared, int64_t (*fn)(int64_t, void *)) {
    /* No threads: run the iterations sequentially (a valid schedule). */
    for (int64_t i = 0; i < n; i++) fn(i, shared);
}
/* No threads: a plain FIFO queue; recv on an empty channel returns 0 (a
 * single-threaded program sends before it receives). */
typedef struct ChNode {
    struct ChNode *next;
    int64_t val;
} ChNode;
typedef struct {
    int64_t refcount;
    void *vtable;
    ChNode *head, *tail;
} VChan;
void *jrt_chan_new(void) {
    VChan *c = (VChan *)malloc(sizeof(VChan));
    c->refcount = -1;
    c->vtable = 0;
    c->head = c->tail = NULL;
    return c;
}
void jrt_chan_send(void *p, int64_t v) {
    VChan *c = (VChan *)p;
    ChNode *n = (ChNode *)malloc(sizeof(ChNode));
    n->next = NULL;
    n->val = v;
    if (c->tail) c->tail->next = n;
    else c->head = n;
    c->tail = n;
}
int64_t jrt_chan_recv(void *p) {
    VChan *c = (VChan *)p;
    if (!c->head) return 0;
    ChNode *n = c->head;
    c->head = n->next;
    if (!c->head) c->tail = NULL;
    int64_t v = n->val;
    free(n);
    return v;
}
#endif

void *jrt_atomic_new(int64_t v) {
    VAtomic *a = (VAtomic *)malloc(sizeof(VAtomic));
    a->refcount = -1;
    a->vtable = 0;
    a->val = v;
    return a;
}

/* Argument env for multi-argument `spawn worker(a, b, ...)`: a header + N i64
 * slots (scalars directly, ref pointer-bits as i64). Immortal (refcount -1 →
 * RC-safe, heap-oracle-clean); the generated packer/unpacker C shims read/write
 * the slots at offset 16 (past the header). Small, freed at exit. */
void *jrt_env_new(int64_t n) {
    int64_t *e = (int64_t *)malloc(16 + (size_t)n * 8);
    e[0] = -1; /* refcount: immortal */
    e[1] = 0;  /* vtable */
    return e;
}
#endif /* !FASTLLVM_FREESTANDING */

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
    if (gbl == 0) trim_buf(&gbuf, &gbl, &gbc); /* only when no deferred garbage pending */
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
        /* Collect every white cycle member, incl. buffered ones (a garbage cycle
         * may span several buffered roots across batches) — the step's compaction
         * removes freed objects from the root buffer, so no dangling pointer. */
        if (COLOR(h) != COL_WHITE) continue;
        SET_COLOR(h, COL_BLACK);
        wl_push(&fwork, &fwl, &fwc, h);  /* free only at the end (post-order) */
        trace_fn t = trace_of(h);
        if (t) t(h, visit_collect_white);
    }
}

/* One bounded incremental collection step. Processes the last COLLECT_BATCH
 * candidate roots (the tail) plus their connected components to completion, then
 * compacts the buffer. Sound: the step runs the full mark/scan/collect atomically
 * on its batch and leaves the graph consistent between steps (every object BLACK or
 * PURPLE-buffered), so the mutator runs freely in between — no write barrier needed.
 *
 * CRITICAL (the fix over the first, unsound attempt): `mark_gray` of an earlier
 * batch root TRIAL-DELETES later batch roots' refcounts to 0 (temporarily). A
 * trial-deleted node is GRAY, NOT dead — MarkRoots must free only BLACK rc==0
 * objects (drop already ran); a GRAY rc==0 node is restored to BLACK by `scan`
 * (`scan_black` from the root that still has an external reference) or collected as
 * WHITE if it is truly garbage. Freeing on rc==0 alone frees a live node mid-trial
 * → corruption/leak (caught by tests/run.sh listdrop). */
static void jrt_collect_step(void) {
    if (!roots_len) return;
    size_t total = roots_len;

    /* Pass 1 (WHOLE buffer): free already-dead acyclic buffered objects — BLACK
     * rc==0, i.e. dropped by RC (drop ran in jrt_release) but left buffered so the
     * free was deferred to the collector. Doing this over the whole buffer (not just
     * the tail) is what fixes the first leak: otherwise dead head-of-buffer nodes are
     * dropped by the compaction below WITHOUT being freed. Bounded (buffer is small).
     * Safe vs the collect below: cycle members are PURPLE/GRAY here (not BLACK rc==0),
     * so pass 1 never touches an object that `collect_white` will free (no double
     * free), and it runs before any node enters `fwork`. */
    for (size_t i = 0; i < total; i++) {
        JObjHeader *h = roots[i];
        if (h && COLOR(h) == COL_BLACK && RC_COUNT(h) == 0) {
            SET_BUFFERED(h, 0);
            free_obj(h);
            roots[i] = NULL;
        }
    }

    /* Tail batch: mark_gray the live candidates. A trial-deleted node is GRAY (not
     * dead) — never freed here; `scan_black` from a root with an external reference
     * restores it before it is wrongly collected. */
    size_t start = total > COLLECT_BATCH ? total - COLLECT_BATCH : 0;
    for (size_t i = start; i < total; i++) {
        JObjHeader *h = roots[i];
        if (h && COLOR(h) == COL_PURPLE && RC_COUNT(h) > 0) mark_gray(h);
    }
    /* ScanRoots + CollectRoots over the batch (each traversal covers the whole
     * connected component, incl. buffered members from earlier in the buffer). */
    for (size_t i = start; i < total; i++) {
        JObjHeader *h = roots[i];
        if (h && COLOR(h) == COL_GRAY) scan(h);
    }
    for (size_t i = start; i < total; i++) {
        JObjHeader *h = roots[i];
        if (h && COLOR(h) == COL_WHITE) collect_white(h);
    }
    /* Compact the WHOLE buffer: keep only still-live PURPLE candidates; everything
     * else — restored-live roots (BLACK, freed later by RC) and collected cycle
     * members (BLACK, in fwork, freed post-order below) — has BUFFERED cleared and
     * is dropped. No dead node is dropped unfreed (pass 1 freed the acyclic dead;
     * fwork frees the cyclic dead). Reads run before the frees below. */
    size_t kept = 0;
    for (size_t i = 0; i < total; i++) {
        JObjHeader *h = roots[i];
        if (!h) continue;
        if (BUFFERED(h) && COLOR(h) == COL_PURPLE && RC_COUNT(h) > 0) {
            roots[kept++] = h;
        } else {
            SET_BUFFERED(h, 0);
        }
    }
    roots_len = kept;
    /* Post-order free: after compaction, so no live buffer slot points at a freed
     * object and no trace hits an already-freed cycle member. Route the collected
     * garbage through the deferred free queue and free only a bounded amount now —
     * a giant component's free_obj cost spreads across steps/allocations instead of
     * one burst (sound: this garbage is unreachable). */
    for (size_t i = 0; i < fwl; i++) gb_push(fwork[i]);
    fwl = 0;
    drain_gbuf(FREE_BUDGET);
    collector_trim();
}

/* Full drain (shutdown / on demand): step until the buffer is empty → 0 live. */
static void jrt_collect_cycles(void) {
    while (roots_len) jrt_collect_step();
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
void *jrt_sentinel_vtable[4] = {(void *)jrt_noop_drop, (void *)jrt_noop_trace, NULL, (void *)jrt_noop_copy};
static JObjHeader arith_exc_obj = {-1, jrt_sentinel_vtable};
static JObjHeader npe_exc_obj = {-1, jrt_sentinel_vtable};
static JObjHeader bounds_exc_obj = {-1, jrt_sentinel_vtable};

/* --- Debug backtraces (--backtrace, off by default) --------------------------
 * Capture a native backtrace at the THROW ORIGIN (where the stack still holds the
 * failing frames) and print it only if the exception goes uncaught. A SIGSEGV/
 * SIGABRT handler covers hard crashes too. Needs -rdynamic so the Vire function
 * names resolve. Zero cost without the flag (empty stubs). Hosted only. */
#if defined(FASTLLVM_BACKTRACE) && !defined(FASTLLVM_FREESTANDING)
#include <execinfo.h>
#include <signal.h>
#include <unistd.h>
static void *bt_buf[64];
static int bt_n = 0;
static void capture_backtrace(void) { bt_n = backtrace(bt_buf, 64); }
static void print_backtrace(void) {
    if (bt_n <= 0) return;
    plat_puts("backtrace (most recent call first, needs symbols; pipe through addr2line for file:line):\n");
    backtrace_symbols_fd(bt_buf, bt_n, 2 /* stderr */);
}
static void bt_signal(int sig) {
    void *buf[64];
    int n = backtrace(buf, 64);
    plat_puts(sig == SIGSEGV ? "\nfatal: segmentation fault\n" : "\nfatal: aborted\n");
    backtrace_symbols_fd(buf, n, 2);
    _exit(139);
}
__attribute__((constructor)) static void bt_install(void) {
    signal(SIGSEGV, bt_signal);
    signal(SIGBUS, bt_signal);
}
#else
static void capture_backtrace(void) {}
static void print_backtrace(void) {}
#endif

/* Called by the runtime checks: set a pending runtime exception. */
static void throw_runtime(void *sentinel, const char *msg) {
    capture_backtrace();
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

/* Fatal (noreturn) variants: when the whole program provably has NO handler that
 * could catch a runtime exception (no try/catch anywhere — the common case, always
 * true for pure Vire), an inline bounds/NPE failure cannot be caught and MUST end
 * the program. Reporting immediately here (identical text to jrt_check_uncaught)
 * lets the generated check end its failure block with `unreachable` instead of the
 * pending-continue merge — so the checked load's result is a direct value, exactly
 * like Rust's `panic` path. Memory safety is unchanged: the check still runs; only
 * the (uncatchable) throw aborts at the site instead of propagating via pending,
 * which is *more* faithful to Java's unwinding semantics. */
_Noreturn void jrt_throw_bounds_fatal(void) {
    capture_backtrace();
    plat_uncaught("java.lang.ArrayIndexOutOfBoundsException");
    print_backtrace();
    plat_abort();
    __builtin_unreachable();
}
_Noreturn void jrt_throw_npe_fatal(void) {
    capture_backtrace();
    plat_uncaught("java.lang.NullPointerException");
    print_backtrace();
    plat_abort();
    __builtin_unreachable();
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
    print_backtrace();
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

/* --- Function-scoped region (a second stack) ---------------------------------
 * For a non-escaping array that is dynamic or too large for the call stack AND
 * not inside a loop: the compiler brackets the function with jrt_region_enter/
 * _leave and allocates the array with jrt_region_array — an immortal bump
 * allocation in a per-thread region, freed en bloc when the function returns.
 * No per-call malloc/free (the backing buffer persists; leave just rewinds the
 * offset), so it beats the RC heap for hot functions with scratch arrays.
 *
 * The region is a single large, lazily-committed virtual buffer per thread
 * (POSIX mmap MAP_NORESERVE — pages cost nothing until touched), so a marker is
 * one offset and any size is handled without chunk bookkeeping. Thread-local →
 * one region stack per thread (scales to multiple stacks, like the arena).
 * Hosted only. Overflowing the reserve is treated as OOM. */
#if !defined(FASTLLVM_FREESTANDING)
#if defined(__unix__) || defined(__APPLE__)
#include <sys/mman.h>
#define REGION_MMAP 1
#endif
#ifdef FASTLLVM_THREADS
#define REGION_TLS _Thread_local
#else
#define REGION_TLS
#endif
#define REGION_RESERVE ((size_t)1 << 32) /* 4 GiB virtual per thread (lazy) */
static REGION_TLS char *r_base = NULL;
static REGION_TLS size_t r_used = 0;
static REGION_TLS size_t *r_marks = NULL;
static REGION_TLS int r_depth = 0, r_mcap = 0;

static void region_init(void) {
#ifdef REGION_MMAP
    r_base = (char *)mmap(NULL, REGION_RESERVE, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
    if (r_base == MAP_FAILED) r_base = NULL;
#else
    /* Non-POSIX fallback: a fixed committed buffer (heavier, secondary target). */
    r_base = (char *)malloc(REGION_RESERVE >> 6); /* 64 MiB */
#endif
}
void jrt_region_enter(void) {
    if (r_depth == r_mcap) {
        r_mcap = r_mcap ? r_mcap * 2 : 64;
        r_marks = (size_t *)realloc(r_marks, (size_t)r_mcap * sizeof(size_t));
    }
    r_marks[r_depth++] = r_used;
}
void jrt_region_leave(void) {
    if (r_depth > 0) r_used = r_marks[--r_depth];
}
void *jrt_region_array(int64_t count, int64_t elem_size, void *vtable) {
    if (count < 0) {
        plat_puts("Exception in thread \"main\" java.lang.NegativeArraySizeException\n");
        plat_abort();
    }
    /* Measurement knob: FASTLLVM_NO_REGION routes region arrays to the heap
     * instead (immortal → matches the compiler's treatment; leaks, so for timing
     * comparisons only). */
    static int region_off = -1;
    if (region_off < 0) {
        const char *e = getenv("FASTLLVM_NO_REGION");
        region_off = e ? 1 : 0;
    }
    if (region_off) {
        JArray *h = (JArray *)plat_alloc((size_t)32 + (size_t)count * (size_t)elem_size);
        h->refcount = -1;
        h->vtable = vtable;
        h->length = count;
        h->elem_size = elem_size;
        return h;
    }
    size_t size = (size_t)32 + (size_t)count * (size_t)elem_size;
    size = (size + 15u) & ~(size_t)15u;
    if (!r_base) region_init();
    size_t cap = REGION_RESERVE;
#ifndef REGION_MMAP
    cap = REGION_RESERVE >> 6;
#endif
    if (!r_base || r_used + size > cap) {
        plat_puts("Exception in thread \"main\" java.lang.OutOfMemoryError\n");
        plat_abort();
    }
    JArray *a = (JArray *)(r_base + r_used);
    r_used += size;
    memset(a, 0, size); /* array default values */
    a->refcount = -1;   /* immortal → RC/collector no-op; freed by region_leave */
    a->vtable = vtable;
    a->length = count;
    a->elem_size = elem_size;
    return a;
}
#else
/* Freestanding: no OS region; allocate an immortal array (RC-safe, matches the
 * compiler's immortal treatment) and leak it — freestanding is short-lived/rare. */
void jrt_region_enter(void) {}
void jrt_region_leave(void) {}
void *jrt_region_array(int64_t count, int64_t elem_size, void *vtable) {
    JArray *a = (JArray *)plat_alloc((size_t)32 + (size_t)count * (size_t)elem_size);
    a->refcount = -1;
    a->vtable = vtable;
    a->length = count;
    a->elem_size = elem_size;
    return a;
}
#endif

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

/* Deep-copy a PRIMITIVE (non-ref) array OUT of the capsule arena to the RC heap,
 * so a capsule's array result survives jrt_arena_pop. The destination is forced
 * onto the real heap (the active arena is bypassed for it) and returned as a
 * normal RC object (refcount=1, tracked live); the source is the arena array. */
void *jrt_arena_export_array(void *src, int64_t elem_size) {
    if (!src) return NULL;
    JArray *s = (JArray *)src;
#ifndef FASTLLVM_FREESTANDING
    Arena *saved = arena_top;
    arena_top = NULL; /* destination goes to the RC heap, not the arena */
    JArray *d = (JArray *)jrt_alloc_array(s->length, elem_size, s->vtable);
    arena_top = saved;
#else
    JArray *d = (JArray *)jrt_alloc_array(s->length, elem_size, s->vtable);
#endif
    jrt_memcpy((JArray *)d + 1, (JArray *)s + 1, (size_t)s->length * (size_t)elem_size);
    return d;
}

/* ===== Deep copy (capsule struct in/out) — Phase 1 foundation ==========
 * A transient pointer→pointer map (original object → its copy) lets a deep copy
 * TERMINATE on cycles and keep sharing (a DAG stays a DAG, not exploded). Open
 * addressing, power-of-two capacity; created + freed per deep copy. */
typedef struct { void *key; void *val; } JCopyEnt;
typedef struct { JCopyEnt *ents; int64_t cap; int64_t len; } JCopyMap;

#ifndef FASTLLVM_FREESTANDING
static int64_t copymap_slot(void *p, int64_t cap) {
    uint64_t h = (uint64_t)(uintptr_t)p;
    h ^= h >> 33; h *= 0xff51afd7ed558ccdULL; h ^= h >> 33;
    return (int64_t)(h & (uint64_t)(cap - 1));
}
void *jrt_copymap_new(void) {
    JCopyMap *m = (JCopyMap *)malloc(sizeof(JCopyMap));
    m->cap = 64;
    m->len = 0;
    m->ents = (JCopyEnt *)calloc((size_t)m->cap, sizeof(JCopyEnt));
    return m;
}
void jrt_copymap_free(void *mp) {
    JCopyMap *m = (JCopyMap *)mp;
    free(m->ents);
    free(m);
}
void *jrt_copymap_get(void *mp, void *key) {
    JCopyMap *m = (JCopyMap *)mp;
    int64_t i = copymap_slot(key, m->cap);
    while (m->ents[i].key) {
        if (m->ents[i].key == key) return m->ents[i].val;
        i = (i + 1) & (m->cap - 1);
    }
    return NULL;
}
static void copymap_grow(JCopyMap *m);
void jrt_copymap_put(void *mp, void *key, void *val) {
    JCopyMap *m = (JCopyMap *)mp;
    if ((m->len + 1) * 4 >= m->cap * 3) copymap_grow(m);
    int64_t i = copymap_slot(key, m->cap);
    while (m->ents[i].key) {
        if (m->ents[i].key == key) {
            m->ents[i].val = val;
            return;
        }
        i = (i + 1) & (m->cap - 1);
    }
    m->ents[i].key = key;
    m->ents[i].val = val;
    m->len++;
}
static void copymap_grow(JCopyMap *m) {
    int64_t oc = m->cap;
    JCopyEnt *oe = m->ents;
    m->cap *= 2;
    m->ents = (JCopyEnt *)calloc((size_t)m->cap, sizeof(JCopyEnt));
    m->len = 0;
    for (int64_t i = 0; i < oc; i++)
        if (oe[i].key) jrt_copymap_put(m, oe[i].key, oe[i].val);
    free(oe);
}
#else
/* Freestanding has no capsules → the deep-copy machinery is never invoked; these
 * no-op stubs only satisfy the linker for the emitted copy.<class>/array-copy
 * vtable slots (referenced but never reached). No heap (no malloc). */
void *jrt_copymap_new(void) { return 0; }
void jrt_copymap_free(void *mp) { (void)mp; }
void *jrt_copymap_get(void *mp, void *key) { (void)mp; (void)key; return 0; }
void jrt_copymap_put(void *mp, void *key, void *val) { (void)mp; (void)key; (void)val; }
#endif

/* Identity copy — the Phase-2 vtable slot-3 stub, still used by internal objects
 * (StringBuilder, exception sentinels) that user capsules never deep-copy. */
void *jrt_noop_copy(void *src, void *map) {
    (void)map;
    return src;
}

typedef void *(*JCopyFn)(void *, void *);

/* Deep-copy ANY ref value by dispatching through its own vtable slot 3 (the
 * per-type copy fn). Null → null. On a map hit (a shared sub-graph or a cycle)
 * the existing copy is returned with an extra reference (a no-op on the immortal
 * arena copies); otherwise the type's copy fn allocates a fresh copy (refcount 1,
 * consumed by the field/array slot that stores it). Cycle-safe because each copy
 * fn registers itself in the map BEFORE recursing into its fields. */
void *jrt_deep_copy_ref(void *obj, void *map) {
    if (!obj) return NULL;
    void *existing = jrt_copymap_get(map, obj);
    if (existing) {
        jrt_retain(existing);
        return existing;
    }
    void **hdr = (void **)obj; /* hdr[1] = vtable (word 1 = offset 8) */
    void **vt = (void **)hdr[1];
    JCopyFn fn = (JCopyFn)vt[3]; /* vtable slot 3 = deep-copy */
    return fn(obj, map);
}

/* Copy fn for a PRIMITIVE array (vtable slot 3 of @vt.array.int): duplicate the
 * length*elem_size bytes; nothing is shared with the source. */
void *jrt_copy_array_prim(void *src, void *map) {
    JArray *s = (JArray *)src;
    JArray *d = (JArray *)jrt_alloc_array(s->length, s->elem_size, s->vtable);
    jrt_copymap_put(map, src, d);
    jrt_memcpy((JArray *)d + 1, (JArray *)s + 1, (size_t)s->length * (size_t)s->elem_size);
    return d;
}

/* Copy fn for a REF array (vtable slot 3 of @vt.array.ref): each element is deep
 * copied through its own vtable (recursion + sharing via the map). */
void *jrt_copy_array_ref(void *src, void *map) {
    JArray *s = (JArray *)src;
    JArray *d = (JArray *)jrt_alloc_array(s->length, s->elem_size, s->vtable);
    jrt_copymap_put(map, src, d); /* before recursion → cycles terminate */
    void **se = (void **)((JArray *)s + 1);
    void **de = (void **)((JArray *)d + 1);
    for (int64_t i = 0; i < s->length; i++) {
        de[i] = jrt_deep_copy_ref(se[i], map);
    }
    return d;
}

/* Copy fn for a String (vtable slot 3 of @vt.java_lang_String): duplicate the
 * bytes into a fresh RC string, keeping the source's vtable. */
void *jrt_copy_string(void *src, void *map) {
    JStr *s = (JStr *)src;
    JStr *d = (JStr *)jrt_alloc((int64_t)sizeof(JStr) + s->len);
    d->vtable = s->vtable;
    d->len = s->len;
    jrt_memcpy(d->bytes, s->bytes, (size_t)s->len);
    jrt_copymap_put(map, src, d);
    return d;
}

/* Deep-copy driver: build the map, dispatch on the root (vtable slot 3), free the
 * map, return the root's copy. `_arena` keeps the active arena as the destination
 * (capsule copy-IN); `_heap` bypasses it so the copy lands on the RC heap and
 * survives the pop (copy-OUT). */
void *jrt_deep_copy_arena(void *root) {
    if (!root) return NULL;
    void *m = jrt_copymap_new();
    void *r = jrt_deep_copy_ref(root, m);
    jrt_copymap_free(m);
    return r;
}
void *jrt_deep_copy_heap(void *root) {
    if (!root) return NULL;
    void *m = jrt_copymap_new();
#ifndef FASTLLVM_FREESTANDING
    Arena *saved = arena_top;
    arena_top = NULL; /* destination = RC heap, not the arena */
    void *r = jrt_deep_copy_ref(root, m);
    arena_top = saved;
#else
    void *r = jrt_deep_copy_ref(root, m);
#endif
    jrt_copymap_free(m);
    return r;
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
#elif defined(_WIN32)
/* Windows has no POSIX clock_gettime (mingw would need clock_gettime64):
 * use QueryPerformanceCounter (monotonic) + the FILETIME epoch (wall clock). */
#include <windows.h>
int64_t jrt_nano_time(void) {
    LARGE_INTEGER freq, ctr;
    QueryPerformanceFrequency(&freq);
    QueryPerformanceCounter(&ctr);
    return (int64_t)((ctr.QuadPart / freq.QuadPart) * 1000000000LL
                     + ((ctr.QuadPart % freq.QuadPart) * 1000000000LL) / freq.QuadPart);
}
int64_t jrt_current_time_millis(void) {
    FILETIME ft;
    GetSystemTimeAsFileTime(&ft);
    uint64_t t = ((uint64_t)ft.dwHighDateTime << 32) | ft.dwLowDateTime; /* 100ns since 1601 */
    return (int64_t)((t - 116444736000000000ULL) / 10000ULL);           /* → ms since 1970 */
}
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

/* Checked integer division/remainder (variable divisor). Constant divisors never
 * reach here — the backend emits native srem/magic-multiply, and constprop turns
 * `mut n = <const>` divisors into literals — so only genuine runtime divisors pay
 * the call. */
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
