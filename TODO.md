# Vire — open work

Only **open** items. Everything else is done or deleted (history in git).
Legend: `[ ]` open · `[~]` partial. Binding basis: [AGENTS.md](AGENTS.md).

Current state: the Gabbro path is green on the bounded subset
(`tests/vire_gab.sh`: integers, counted loops incl. affine shifts, strings,
const arrays, int-payload sums, `region(max)` arenas, static object rows,
scalar `extern` — plus decline tests for every gap). The LLVM oracle
(`tests/vire_heap.sh`) is green. Solver infers; Gabbro verifies.

Soundness floor (never waived): `cargo build -p vire` + `tests/vire_heap.sh`
green after every step; every emitted `.gab` passes `gabbro check` with 0
errors; every rejection has a code + test (promote **and** decline).

---

## Full-coverage plan (every valid Vire program → valid Gabbro)

Rule: the solver finds or proves every bound/cost/contract (never the human);
Gabbro holds each one. What has no static source stays declined with a code
(K003 costs, O29 unbounded dynamics). Per phase: probe the Gabbro form at the
oracle first, then promote + decline tests, 0 errors, run parity.

## Phase 1 — M4b signatures (in work)

- [x] **Reference parameters:** `option index into T` params from body-use
  classes + call-site unions (getters, predicates, builders taking objects).
  Use-site classes cover inlined null args; dotted methods map into the
  reserved `vire_method_` namespace (collision-free by construction).
- [ ] **Method calls:** direct fns after devirt/inline (inlined ones already work;
  surviving calls need mangled-name + self-class mapping).
- [ ] **`?` as match** and **monomorphized sum signatures** across functions
  (Phase 1b — needs a multi-value return form).

## Phase 2 — rows + table-bounded traversal

- [ ] **Row linearization:** `New` in loops → one row per trip (E501 today).
- [ ] **Traversal loops:** `while c != null` over `count K` tables →
  `retry bounded K until (c == None)` (bound from the declaration).

## Phase 3 — scalar completion

- [ ] Floats (`f64` ranges, narrow or proven), division/modulo (narrow-guard),
  `Bool`/`Char` slots, mixed int widths, explicit overflow semantics.

## Phase 4 — dynamic bounds via provenance

- [ ] `for i in 0..n` compiles iff the solver finds a static ceiling
  (const array lengths, region ceilings, constant call chains). Else honest E501.

## Phase 5 — collections, closures, tuples

- [ ] `List`/`Map`/`Set` with static caps → tables + length, over-cap → `refuse`.
  Iterator adapters → loop fusion. Lambdas → defunctionalized direct calls.
  Tuples/fixed arrays → locals/tables. Full `Str` methods via max arithmetic.

## Phase 6 — errors, hardware, threads, capsules

- [~] **Syscalls + machine intrinsics (H1, done):** `syscall NAME(..) = NUM`
  items and the `__vire_*` family (MMIO/port/fence from `Mmio`/`Port`/`fence`)
  become per-site `extern fn` heads (number rides as data); `$mmio` handles are
  compile-time descriptors; user shadowing of the intrinsic namespace is E500.
  Full `device`/`reg` decls and `syscall ... via ...` gates need target ABI
  knowledge — later step, never faked. MMIO/port are compile-only (the host
  cannot execute them); `fence` + `getpid`-class syscalls carry run parity.
- [ ] `Result` across functions, panic paths → `abort`/refuse.
- [ ] `hw`/`syscall` → `device`/`reg` + syscall gates (solver-built ABI/costs).
- [ ] `spawn` → Gabbro concurrency (probe 05 first: locks + `shared` guards).
- [ ] `capsule` → arenas with copy in/out loops.

## Phase 7 — costs + link units

- [ ] Inferred costs/contracts instead of flat rates. Recursion `decreases`.
- [ ] Dynamic modules/`open fn` as `gabbro link` units (research, not promised).

## Honestly out

- [ ] Deckenlose Unbeschränktheit ohne statische Quelle, `Rc`, Probabilistik.

---

## M4b — object signatures (biggest scope lever)

- [ ] **Row linearization:** `New` inside loops → one row per iteration (E501 today,
  `gab_objloop_e501`). Unlocks loop-allocated graphs.
- [ ] **Reference parameters:** monomorphized signatures for `Ref` params/returns
  (E500 `M4b` today, `gab_objparam_e500`).
- [ ] **Traversal loops:** `while c != null` with exact-trip reasoning (E501 today,
  `gab_traverse_e501`).
- [ ] **Method dispatch:** `CallVirtual`/`CallPoly`/`CallGuarded` on the Gabbro path
  (E500 today) — devirtualize first, then emit.
- [ ] **Wider fields:** `Bool`/`Float`/non-`Int` slots, string payloads in sums,
  nested sums, sum parameters (all E500/E501 today).

## Bound solver — loop shapes

- [ ] Multiple counters, compound conditions (`&&`/`||`), equality loops
  (exact-trip), dynamic bounds (correctly E501 per K003 — needs cost-model work,
  not just inference).
- [ ] Conversions/negation/effectful calls in conditions, header temps escaping
  into the body, irreducible bodies, `switch` inside loops, nested loop shapes.
- [ ] Non-constant `region(max)` ceilings and array lengths (E501 today).
- [ ] String methods, non-`i64` elements, dynamic indices, array rebinds.

## Cost / contract solver (incl. `extern "C"`)

- [ ] Inferred costs/contracts instead of flat rates (calls 64, field ops 96,
  region 16/64) — the solver proposes, Gabbro holds.
- [ ] `hw`/`syscall` → `device`/`reg` declarations and `syscall` gates
  (AGENTS.md §3: the solver builds the ABI mapping, costs, and contracts).
- [ ] `extern` structs with layout; real C contract signatures instead of
  `writes ausgabe`.

## Inference completeness

- [ ] Hindley–Milner + trait resolution, coherence across generic impls.
- [ ] Value generics in return position (defaults to `Int` today).
- [ ] `?`-payload class propagation, iterator invalidation, `Str.split → List[Str]`.

## Scaling (millions of LOC) — low priority

The compiler may be slow (many solvers, minutes are fine); the binary must be
fast (Gabbro is the target). So: termination guarantees yes, speed no. Still
open, but behind binary performance.

- [ ] Cap the uncapped fixpoints (`bounds.rs` GVN/upper/lower, RTA `O(F²)`,
  `own.rs` `O(B²)`), intern string keys, worklists instead of rescan loops.
- [ ] True incrementality / analysis caching (the Gabbro path already skips the
  proof passes it never reads; the LLVM oracle still pays them).
- [ ] Intra-function parallelism (measured: the wall is one huge inlined function,
  per-function threads buy nothing).

## Runtime endgame

- [ ] Delete `crates/runtime/runtime.c` once Gabbro validates (oracle-only until then).
- [ ] Kernel `Rc[T]` as a pure Vire library type (counting as ordinary emitted
  code, no collector, cycles only via `weak`/ceiling).
- [ ] E300 default-deny; GUARD_FREE-style temporal check for new emitter paths.

## Measurement & proofs

- [ ] No valid performance numbers exist (they count only through the validating
  Gabbro compiler — best-of-N, bit-identical, RSS alongside).
- [ ] Differential fuzzer coverage for new emitter paths (arena/extern/affine/M4a).
- [ ] Pre-existing (not a regression): if-expression values
  (`mut x = if c { 10 } else { 20 }`) emit code `gabbro check` rejects (M119) —
  diamond value joining is its own step.
