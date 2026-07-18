# RAM Usage — Measurement + Reduction Plan

*User request: "measure the RAM usage and look for ways to reduce it." Especially
relevant for the seL4 target (scarce memory).*

## Measurement (MaxRSS, getrusage wrapper)
| Workload | Vire | Comparison |
|---|---|---|
| pagerank 262144 (262144 nodes live) | **24.4 MB** | Rust (flat vecs) 8.0 MB → **3×** |
| esc (100000-node list live) | 7.1 MB | — |
| binary-trees (calloc/free) | 7.9 MB | auto-arena 7.7 MB (working set small) |

## Where the RAM goes — two contributors
Node = `{next, prev, rank}` = 24 B of payload. Vire places the **jrt header** in
front: `{ int64_t refcount; int64_t rcflags; void *vtable }` = **24 B**. So 48 B/object —
the header **doubles** the object size. Rust: 3 flat `i64` vectors, 24 B/node,
NO header.

On top of that, the **glibc malloc overhead** (measured with `malloc_usable_size`):
- 48 B requested → **56 B** allocated (8 B rounding/bookkeeping),
- 40 B requested → **40 B** (exact, no rounding),
- 24 B → 24 B.

→ A 48-B object really costs **56 B**. A 40-B object costs **40 B**.

## The lever: header 24 B → 16 B (pack rcflags into refcount)
`rcflags` uses **only 3 bits** (color bits 0-1, buffered bit 2 — Bacon-Rajan
collector), yet occupies a full 8-B word. Packing these 3 bits into the
`refcount` word shrinks the header to **16 B** → node **48→40 B**, and thanks to the
malloc size class **56→40 B real = −29 %/object**.

**pagerank projection:** 262144 × 56 B = 14.7 MB → 262144 × 40 B = 10.5 MB;
RSS ~24.4 → ~20 MB (**−17 % overall, −28 % object memory**). Directly noticeable
on the seL4 target.

### Encoding (worked out, sound)
A single `int64_t rc` word:
- **Bits 0-1:** color, **bit 2:** buffered (as before, `rcflags & 7`).
- **Bits 3-62:** reference counter (up to 2^60 — practically unbounded).
- **Bit 63 / `rc < 0`:** immortal (stack/literals) — unchanged as the fast test.
- `retain`: `rc += 8`; `release`: `rc -= 8`, then null test `(rc >> 3) == 0`
  (equivalent to `rc < 8 && rc >= 0`).
- `COLOR(h) = rc & 3`, `BUFFERED(h) = (rc >> 2) & 1` — unchanged, cheap.
- `jrt_alloc`: refcount=1 → `rc = 8`; immortal → `rc = -1`.

The immortal fast test (`rc < 0`), which forms the hot path in retain/release/collector,
stays identical. retain/release go from `++/--` to `+=8/-=8` — same cost. The
null test becomes `>>3`.

### Affected sites (coordinated refactor, ~40 sites)
- **Backend:** `HEADER_SLOTS 3→2`, `VTABLE_WORD 2→1`; struct emission
  `{i64,i64,ptr,…}` → `{i64,ptr,…}` (classes, `%arr.int/ref`, `@jstr.*`,
  `@jclass*`, string constants); metadata offsets (typedesc/name 24/32 → 16/24).
- **Runtime:** 11 header struct defs `{refcount, rcflags, vtable}` → `{rc, vtable}`;
  RC macros (COLOR/SET_COLOR/BUFFERED); `jrt_retain/release` (+=8/-=8, null test);
  collector (color ops read/write `rc`); `jrt_alloc` (rc=8/-1); array/string/
  boxing/SB header.

### Risk + validation
Memory-safety-critical (GC hot path + all layouts). **Soundness oracle: the
Java regression suite checks heap balance = 0 live** — every RC/layout error
surfaces there. Additionally the Vire suite + benchmark correctness + `HEAPSTATS`.
Therefore: implement it **as a focused, deliberately executed step** (don't rush it
in a multi-topic turn) — the same gate discipline as with the arena.

## RAM levers already in effect (built)
- **Auto-arena** (escape→arena, `ESCAPE-ARENA.md`): allocation-heavy `while` loops
  use bump allocation instead of malloc-per-node → no malloc rounding overhead,
  en-bloc freeing. RAM working set of the iteration instead of the total sum.
- **Immortal objects** (stack/literals, refcount=-1): no RC bookkeeping.

## Further options (secondary, measured/estimated)
- **Remove the vtable pointer** for types without RTTI need (no getClass/instanceof)
  AND without ref fields (no drop/trace needed): −8 B. But the collector `trace`
  needs the vtable when there are ref fields → only for pure scalar structs, layout-invasive,
  small gain. **Not a priority.**
- **Pool/slab allocator** (instead of calloc) for same-size objects: eliminates
  malloc bookkeeping entirely + better locality. Larger refactor; the auto-arena
  already covers the hot case. **Later.**
- **Field packing** (i32 instead of i64 where the value range fits): needs value-range
  analysis; the IR is i64-centric today. **Later.**

## Recommendation
The **header pack (24→16 B)** is the clear, universal RAM lever (−28 %
object memory, hits the malloc size class, helps seL4). The encoding is worked
out and sound; implement it as a deliberate, focused step with the
heap-balance suite as the oracle.

## BUILT + MEASURED: header pack (24→16 B) + the path to Rust level
The header pack is implemented (encoding A, see commit). Measured effect:

| Workload | before | after | Rust |
|---|---|---|---|
| pagerank OBJECTS (262144 nodes) | 24.4 MB | **19.9 MB (−18%)** | — |
| **pagerank ARRAYS** (`array(n)`, flat data) | — | **7.76 MB** | **8.0 MB** |

**Key finding — RAM at Rust level is ACHIEVED as soon as the data structure is flat:**
Vire's array-based pagerank (7.76 MB) **undercuts Rust (8.0 MB)** — arrays have
NO per-object header (only a single array header once), exactly like Rust's flat
`Vec`s. The 3× gap was the **data-structure choice** (pointer-linked node objects vs
flat index arrays), NOT the compiler. Rust's pagerank itself uses flat vecs;
if you write Vire pagerank the same way (`array(n)` + indices), it is at Rust parity.

**Object-based residual gap (19.9 MB vs 8 MB):** inherent, because RC needs a header.
Node = 16 B header + 24 B data = 40 B (vs Rust 24 B flat). On top of that the
**glibc malloc chunk overhead** (~8–16 B of metadata per allocation BEFORE the pointer,
despite `usable_size`=40): 262144 × ~48 B real. The next lever for this is a
**slab/pool allocator** (fixed size classes, no per-chunk glibc header, dense
packing) — the auto-arena already does this for transient loops; for persistent
graphs a slab allocator would be the complement. Estimate: ~19.9 → ~13 MB.

**Conclusion:** (1) flat data → **Rust parity today** (built/measured). (2) object
graphs → header pack −18% (built), slab allocator as the next lever (~−35%).
Rust level for object graphs is only partially reachable (the RC header is the price
of automatic memory safety on cyclic graphs).

## BUILT: slab allocator (next lever, implemented)
Small objects (≤256 B) now come from **segregated size-class pools**
(8-B granularity, hits 40-B nodes exactly) instead of individually via `calloc` — saves the
glibc chunk overhead (~8–16 B/allocation) + dense packing. Slabs are
256-KB-aligned; `free` finds the slab via `ptr & MASK` and checks a hash set
of slab bases (safe — no false positive against calloc'd large objects/arrays),
free cells in intrusive per-class free lists. Large objects → still `plat_alloc`.

**Important — 8-B granularity:** the first 16-B version rounded 40 B → 48 B and was
worse than calloc; switched to 8-B classes (40 B → exactly 40 B).

**Measured (with header pack):**
| Workload | without slab (calloc) | with slab | |
|---|---|---|---|
| esc (100k acyclic nodes) | 7.20 MB | **5.85 MB** | **−19%** |
| pagerank (262k cyclic nodes) | 20.4 MB | **18.1 MB** | −11% (collector dominates) |

Sound: Java 65/65 (heap balance), Vire suite green, correctness across objects/arrays/
collections/arena/generics/C++ bridge verified. **Total RAM reduction this session:
pagerank 24.4 → 18.1 MB = −26%** (header pack + slab). The cyclic residual gap to
Rust is the Bacon-Rajan collector (mark/scan buffer over the large cycle) +
the inherent 16-B RC header; acyclic/array cases are at/below Rust level.

## BUILT: field packing (opt-in `I32`, now fully usable)
`I32`/`Bool` fields already packed to 4 bytes in the struct, but were NOT usable:
a packed `I32` field in i64 arithmetic (`t.big + t.small`) produced a backend
type error. Fix: the binary lowering widens the narrower i32 sign-correctly to
i64 (`widen_i32`), so that mixed int widths are type-correct. With this,
**user-driven field packing is fully usable** (declare `I32` where values fit).

**Measured** (1M linked records, live set): `Rec{prev, 4× Int}`=56 B →
`Rec{prev, 4× I32}`=40 B, **RAM 65.8 → 49.8 MB = −24%**, identical output.
**Alignment note:** only worthwhile with MULTIPLE narrow fields (a single i32
after pointers gets padded to 8 → no gain; the pagerank node does not benefit,
multi-int structs ~24%). **Open (subsystem):** auto-narrowing (infer `Int`→i32
when values provably fit) needs a value-range analysis
(whole-program interval fixpoint over the field stores) — its own focused step.
