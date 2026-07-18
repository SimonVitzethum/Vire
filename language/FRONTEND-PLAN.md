# Vire — Front-End Build Plan (complete)

The overall plan of the Vire compiler: from `.vr` source text to the existing
mid-level IR (`crates/ir`) in **SSA**. From there on, solver + backend take over unchanged.
Parser details in [PARSER.md](PARSER.md); this plan spans all phases,
defines data structures, milestones, and the **Java replacement criteria**.

## Why now (result of M0)
[M0-MEASUREMENT.md](M0-MEASUREMENT.md) proved: the path to ~1.1–1.5× in the
shared/cyclic case runs through **region-borrow inference**, and that is
**blocked by slot reuse** on the javac IR (`Local(3)` = owner *and* borrow in the same
slot). A dedicated front-end that produces **SSA from the start** makes exactly this
analysis trivial — that is the one lever the bootstrap does not provide.

## Reuse boundary (what stays, what is new)
```
.vr ─► [ VIRE FRONT-END (new) ] ─► crates/ir (SSA) ─► [ Solver + Backend (exists) ] ─► Binary
        Lexer→Parser→Resolve→
        Infer→Comptime→Lower
```
- **New:** `crates/vire` (lexer, parser, AST, name resolution, type inference,
  `comptime`/macros, SSA lowering).
- **Reused unchanged:** `crates/ir`, `crates/solver`, `crates/backend`,
  `crates/driver` (clang invocation, runtime.c). The Java path (`classfile`,
  `frontend`) is **bootstrap** and will be removed per the criterion below.

## Pipeline phases & data structures

### F1 — Lexer (`vire::lexer`) — **in progress**
`&str → Vec<Token>`. Token = `{ kind: TokKind, span: Span }`. Newline-as-
terminator like Go (PARSER.md §2.3), string interpolation, nestable
comments, `[]` generics (no `<>`). **Done + unit tests in this step.**

### F2 — Parser (`vire::parser`) — **started**
`Vec<Token> → ast::Module`. Recursive descent (items/statements) + Pratt
(expressions, precedence table PARSER.md §4.1). Error recovery (panic mode),
multiple diagnostics per run. AST carries spans (for diagnostics + debug info Feature 8).

### F3 — Name resolution (`vire::resolve`)
Whole-program: binds identifiers to declarations. One module = file, one package =
directory (`mod.vr`). Builds the **symbol table** (types, functions, traits,
impls) and the **trait/impl index** for resolution. Capitalization=type as an
enforced rule (PARSER.md §1) → already used here.

### F4 — Macro/`comptime` expansion (`vire::comptime`)
*Before* type checking of the expanded code. Hygienic macro expander
(typed parameters, PARSER-hygienic) + `comptime` evaluator (interpreter over
AST/type graph, recursion limit) + `@if`/`@when`. Produces an **expanded AST**.

### F5 — Type inference + trait resolution (`vire::infer`)
**Bidirectional HM inference** with local anchors (signatures at fn/module boundaries
keep errors close — EVALUATION §5). Trait resolution + **coherence** (the real
risk, not vanilla HM). Result: every AST node annotated with a `Ty` in
a side table. **Monomorphization requests** (which type combinations)
are collected here.

### F6 — Monomorphization (`vire::mono`)
One specialized AST instance per used type combination. Conceptually docks
onto the existing inliner; produces concrete, generic-free functions for the
lowering.

### F7 — Lowering to IR **in SSA** (`vire::lower`)
`ast (typed, mono) → ir::Program`. Core points:
- Value types → struct layout; sum types → tagged union; `match` → `Switch` +
  field access; closures → function + environment; `?` → early return.
- **SSA generation directly** (no slot reuse like javac!) → the entire
  GVN-versus-slot-reuse effort of the bootstrap is eliminated, and **region-borrow becomes
  trivial** (every value has its own number; owner vs. borrow never in the same slot).
- **Iterator mutation check** (REFERENCE §9a) at this point.
Then: `solver::run` → `elide_bounds`/`fuse_long_compares`/`elide_redundant_ref_copies`
→ `elide_pending_checks` → `inline_program` → `stack_allocate` → `backend::emit`
→ clang. **All unchanged.**

### F8 — Region-borrow (the M0 payoff driver, on SSA)
On the SSA IR: prove loop-stable containers as a borrowable region → strike
loop RC (M0.1b: 4.4×→1.5×) and the collector does not trigger (108×→gone). *On
SSA* this is the simple analysis that was impossible on the Java IR. Produced as a new
solver pass **or** in `lower`.

## Crate layout
```
crates/vire/
  Cargo.toml            # dep: fastllvm-ir (lowering target)
  src/
    lib.rs              # pub fn compile(src) / parse(src)
    lexer.rs   ast.rs   parser.rs
    resolve.rs infer.rs comptime.rs mono.rs lower.rs
    diag.rs             # Diagnostics (Span, message, fix suggestion)
  tests/                # Lexer/parser snapshots, example corpus
```
A `vire` binary (in `driver` or `crates/vire/src/main.rs`): `vire build|run|
parse|fmt file.vr`.

## Milestones
- **M1 (this step):** Lexer complete + parser for functions/expressions/types/
  `match`/control flow; `vire parse` dumps AST; example corpus parses.
- **M2:** Resolve + simple monomorphic type checking; `vire build` for non-
  generic code → IR → binary (first runnable `.vr` programs, e.g. `sieve.vr`).
- **M3:** HM inference + traits + monomorphization; `shapes.vr`/`tree.vr` run.
- **M4:** `comptime`/reflection/macros (Features 2–4).
- **M5:** Region-borrow on SSA → **measure M0.1 again** (target ~1.5×, then bounds/
  layout ~1.1×). Stdlib + FFI (F phases P5).

## Java replacement criteria (when the bootstrap is deleted)
The Java path (`crates/classfile`, `crates/frontend`, `examples/*.java`, `tests/`)
remains **only** as a backend-soundness guard (0-live suite) and M0 measurement baseline, until:
1. `vire build` runs the **ported** regression tests (from `tests/` to `.vr`) green
   (incl. 0-live heap balance), **and**
2. Vire brings the M0 graph to ≤~1.5× (evidence that SSA+region-borrow takes effect).
Then: remove `classfile` + `frontend` + Java `examples`/`tests`; the benchmark
Java (`benchmarks/`) remains as **comparison examples** (Rust/C++ baselines).
**Deleting earlier = unproven backend + unbuildable project** — hence staged.

## Non-goals (front-end)
No more bytecode import (the bootstrap purpose is fulfilled), no Java semantics
(boxing, everything-is-Object), no `<>` generics, no slot reuse.
