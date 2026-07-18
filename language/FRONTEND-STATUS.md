# Vire front-end — status of the 7 lowering points

Session goal: close the 7 "parsed but not lowered" gaps (after FFI +
syntactic sugar). Status:

| # | Point | Status | Evidence |
|---|---|---|---|
| 1 | Methods / `impl` blocks | ✅ **done** | `Class.method`, self=Ref, `p.sum()`→70 |
| 2 | Sum types + pattern `match` | ✅ **done** | tagged class, `match`→tag cascade; area()=300/20/0 |
| 6 | Collections (`List`) | ✅ **done** | list literal/index/`.len()`/`for x in xs` over arrays |
| 4 | Comprehensions | ✅ **done** | `[x*x for x in xs if c]` (map+filter), two-pass |
| 3 | Generics / Traits / Monomorph. | 🟡 **open** | mono engine needed (see below) |
| 5 | Closures / Lambdas | 🟡 **open** | closure conversion + indirect calls needed |
| 7 | Option/Result + `?`, comptime/macros | 🟡 **partial** | Option/Result trivial after generics; comptime large |

Additionally this session: **FFI (C/C++/Python)**, **syntactic sugar** (script `main`,
multi-arg `print`, trailing commas), **map/set literals** (part of #4) still open.

## The 3 open ones — the honest implementation path for each
### 3. Generics / monomorphization
Needs an **instance worklist**: do NOT lower generic `fn f[T]` directly;
at each call site bind the type arguments from the argument types, substitute `T` in
the FnDef, generate an instance `f$Int`/`f$Point` on demand (cache),
until fixpoint. Architectural change: the lowering must be able to create new
functions during lowering (currently each function is lowered independently).
Substitution: `Type{name:"T"}` → concrete type in param/return/body. Traits
(bounded generics + dispatch) are a further layer on top.
**Trivial afterwards:** `Option[T]`/`Result[T,E]` as built-in sum types (the
match machinery already stands), `?` desugars to `match … { Err(e) -> return Err(e); Ok(v) -> v }`.
Generic `List[T]` also falls out.

### 5. Closures / lambdas
Non-capturing lambdas: lift to top-level functions, pass function pointers —
needs **indirect calls** (CallVirtual-like on a function pointer) and
higher-order infrastructure (`xs.map(f)`). Capturing closures: **closure conversion**
(box captured variables into a heap environment, as a hidden parameter).

### 7b. comptime / macros
`comptime <expr>` needs a **compile-time evaluator** (const-folding interpreter
over the AST) + reflection API. Hygienic macros: AST→AST transformation with
hygiene context. Both are self-contained subparts.

## Why stopped here (instead of rushing)
The 4 completed points are sound, tested (41 Vire tests), Java 65/65. The 3
open ones each need an architectural extension that — rushed — risks
unsoundness (the discipline of this entire thread: better to honestly measure/mark than
momentum). Each path above is concrete; generics is the lever that also unlocks #7 (Option/
Result) and is therefore the next sensible step.
