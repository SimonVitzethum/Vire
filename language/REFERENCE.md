# Vire — Language and Feature Reference

Precise description of syntax and semantics. Complements the tour in
[LANGUAGE.md](LANGUAGE.md) with completeness and the eight features from
[FEATURES-EVALUATION.md](FEATURES-EVALUATION.md). Target vision: **no harder than
Python, statically inferred, memory-safe, AOT via FastLLVM.**

Status: **design specification** (front-end not yet implemented; backend/
solver exist). Examples show the target semantics.

---

## 1. Lexical structure

- **Encoding:** UTF-8. Identifiers: Unicode letters + `_`, then additionally digits.
- **Comments:** `// line`, `/* block */` (nestable).
- **Statement end:** newline. Semicolon `;` optional (for separating multiple
  statements on one line).
- **Blocks:** `{ … }`. The **last expression** of a block is its value.
- **Literals:**
  - Integer: `42`, `0xFF`, `0b1010`, `0o17`, `1_000_000`, suffix `42i32`, `7u8`.
  - Floating-point: `3.14`, `1e-9`, `6.022e23`, `2.0f32`.
  - Bool: `true`, `false`. Char: `'a'`, `'\n'`, `'\u{1F600}'`.
  - String: `"…"` with interpolation `{expression}` (on **every** string, no `f`
    prefix) and format `{x:6}`, `{x:.2}`, `{x:x}` (hex). **Literal braces
    double:** `{{` → `{`, `}}` → `}`. Raw: `r"C:\path"`. Multi-line: `"""…"""`.
- **Name classes (grammar, [PARSER.md](PARSER.md) §1.1):** `UpperCamel` = type/
  constructor (`Point`), `SCREAMING_SNAKE` = const value (`MAX`), `lower_snake` =
  value/fn/variable (`xs`). Purely lexical — carries the `[]` disambiguation.
- **Keywords:** `fn type trait impl mut const use pub extern unsafe
  match if elif else while for in break continue return spawn macro comptime
  and or not self Self as`.

## 2. Bindings and mutability

```vire
x = 5              // binding (first `x` in scope), immutable, type inferred
mut y = 0          // mutable binding
y = y + 1          // assignment (y is `mut`) — ok
x = 6              // ERROR: `x` immutable, no silent rebind
const MAX = 1024   // compile-time constant (SCREAMING_SNAKE = const value)
mut n: Node = pick()   // optional `: Type` — the inference escape hatch
```

The `: Type` annotation is optional and rarely needed (types are inferred), but it is
the **escape hatch** for the cases the monomorphic unifier can't reach — e.g. binding an
object whose class the right-hand side doesn't carry (an `if` with a `null` branch), so
a later `n.field` still resolves instead of erroring "type of the object unknown".

**Binding vs. assignment without `let`** ([PARSER.md](PARSER.md) §1.4): the **first**
`name =` in a scope **binds**; every **further** one in the same scope is an
**assignment** and requires `mut` — otherwise an error. **Shadowing** only via *inner*
scopes (an inner `x = …` shadows the outer one, does not change it). This makes
intent expressible and catches the typo — the deliberate price for one fewer
keyword than Rust.

## 3. Types

### 3.1 Base types
| Category | Types |
|---|---|
| Integer (signed) | `Int`(=`I64`), `I8 I16 I32 I64` |
| Integer (unsigned) | `UInt`(=`U64`), `U8 U16 U32 U64`, `Byte`(=`U8`) |
| Floating-point | `Float`(=`F64`), `F32` |
| Others | `Bool`, `Char`, `Str`, `Unit`(`()`), `Ptr[T]` (only `unsafe`) |

Integer semantics: **overflow-checked by default — also in release** (panic or
`Result` per operator). This is deliberately *not* Rust's "debug checked, release
wrapping": a program correct in debug that silently wraps in release is exactly
the footgun that a safety-oriented language must not hide as a configuration detail.
Whoever *wants* wrapping says so explicitly — via wrap operators
(`a +% b`, `a *% b`, Zig-style) or the type `Wrapping[T]`. `checked_add`/
`saturating_add` return `Option`/clamped value. Unchecked wrapping in release
only globally disableable (`--unchecked-arith`) — a documented, deliberate hazard, not
the default. No implicit numeric conversions — explicit with `as`.

> **Performance note (measured, [M0-MEASUREMENT.md](M0-MEASUREMENT.md)):** overflow checks
> are branches that **inhibit autovectorization** — on a hot
> arithmetic loop empirically **4.6x slower**, AVX2 path broken. The
> checked default is the safe choice; **hot numeric kernels opt out
> explicitly** via `+%`/`Wrapping[T]` to keep the vector path. This `+%` culture in
> kernels is part of the design, not a loophole.

### 3.2 Composite types — `type`
**Product type** (struct, value type, no object header):
```vire
type Point { x: Float, y: Float }
p = Point(1.0, 2.0)            // positional
q = Point(x: 3.0, y: 4.0)     // named; field order irrelevant
```
**Sum type** (tagged union; replaces enums and `null`):
```vire
type Shape {
    Circle(radius: Float)     // variant with named/positional fields
    Rect(w: Float, h: Float)
    Empty                     // dataless variant
}
```
**Methods** live in the `type` block; `self` is the receiver:
```vire
type Vec2 {
    x: Float, y: Float
    fn len(self) = sqrt(self.x*self.x + self.y*self.y)
    fn add(self, o: Vec2) = Vec2(self.x + o.x, self.y + o.y)
}
```

### 3.3 Built-in generic types
`List[T]` (`[1,2,3]`), `Map[K,V]` (`["a": 1]`, empty `[:]`), `Set[T]` (`Set[1, 2]`),
`Option[T]` (`Some(x)`/`None`), `Result[T,E]` (`Ok`/`Err`), tuple `(A, B)`,
fixed array `[T; N]` (N comptime int, stack-resident).

## 4. Expressions and control flow

Everything is an expression where it makes sense:
```vire
label = if s >= 50 { "ok" } else { "fail" }     // if expression
sign  = match n { 0 -> "0", _ if n > 0 -> "+", _ -> "-" }   // match with guard
```
- `if c { } elif d { } else { }`
- `while c { }`, `for x in iter { }`, `for i, x in enumerate(xs) { }`
- `break`/`continue`, both with optional label: `break :outer`
- `match` is exhaustive (non-exhaustive = compile error); patterns:
  literals, variants `Circle(r)`, tuples `(a, b)`, binding `x`, wildcard `_`,
  guards `_ if cond`, or-patterns `A | B`.

## 5. Functions and closures

```vire
fn add(a, b) = a + b                      // expression form
fn norm(v: Vec2) -> Float {               // block form, optional annotations
    d = v.len()
    if d == 0.0 { 0.0 } else { d }
}
inc = x -> x + 1                          // closure (one arg)
sum = (a, b) -> a + b                     // closure (multiple)
xs.map(x -> x * 2).filter(x -> x > 3)     // closures as arguments
```
Arguments are immutable (like bindings); `mut` parameters for local
mutation. Default arguments: `fn open(path, mode = "r") { … }`. Named arguments
at the call: `open(path, mode: "w")`.

## 6. Generics and traits (type classes) — *Point 2*

```vire
trait Ord {
    fn cmp(self, o: Self) -> Int
    fn less(self, o: Self) = self.cmp(o) < 0     // default method
}
impl Ord for Int { fn cmp(self, o) = self - o }

fn max[T: Ord](a: T, b: T) -> T { if a.less(b) { b } else { a } }
fn sort[T: Ord](xs: List[T]) -> List[T] { … }

// Multiple bounds:
fn dedup[T: Ord + Hash](xs: List[T]) -> List[T] { … }
```
Monomorphization: one specialized, inlinable variant per used type combination
(zero-cost). Trait resolution is static → direct calls (present today in the solver
as devirtualization).

**Value generics** (comptime parameters, like C++ non-type parameters):
```vire
type Matrix[T, comptime R: Int, comptime C: Int] {
    data: [T; R * C]
    fn get(self, r: Int, c: Int) -> T = self.data[r * C + c]
}
```

## 7. `comptime` and compile-time reflection — *Points 2, 3*

`comptime` marks code that is executed **in the compiler**. No separate
macro dialect — it is the same language, just at compile time.

```vire
const TABLE = comptime {                  // computed at compile time → constant
    mut t = [0; 256]
    for i in 0..256 { t[i] = crc_byte(i) }
    t
}

comptime if cfg.os == .linux { use_epoll() } else { use_kqueue() }   // conditional
```

**Reflection** via `@typeinfo(T)` (comptime-traversable):
```vire
fn to_json[T](v: T) -> Str {
    info = @typeinfo(T)
    comptime match info.kind {
        .Struct -> {
            mut parts = []
            comptime for f in info.fields {
                parts.push("\"{f.name}\":" + to_json(v.@field(f.name)))
            }
            "{{" + parts.join(",") + "}}"
        }
        .Sum   -> …
        .Int   -> int_to_str(v)
    }
}

@derive(Json, Eq, Hash, Ord)              // derivation = comptime-generated impls
type User { id: Int, name: Str }
```
Reflection is purely static — **no** runtime metadata, no RTTI overhead.

## 8. Macros (hygienic **and type-safe**) — *Point 4*

No C preprocessor. `macro` operates on the AST — and is type-safe at **every**
point. Exactly this separates Vire's "preprocessor" from the C text-replacer, which is
type-blind. The guarantees:

1. **Typed parameters.** A macro parameter has a kind (`expr`, `type`,
   `ident`, `pat`, `block`) *or* a concrete type. If the macro is called with the
   wrong fragment, that is a **compile error at the call site** — not
   somewhere deep in the expansion.
2. **Type-checked after expansion.** The *expanded* result undergoes the full
   type check like normal code. A macro can **not** produce an ill-typed or
   ill-formed program (unlike `#define`).
3. **Hygienic.** Names bound in the macro (`t0`, `r` below) never collide with
   names at the call site; referenced free names bind at the definition site.
4. **Diagnostics at the call site** with a span reaching into the expansion (no "error in
   generated code, line ???"); full debug info (Feature 8).

```vire
// Parameter with kind: cond is an expression, body a block.
macro unless(cond: expr, body: block) {
    if not (cond) { body }
}

macro timed(label: expr, body: block) {
    t0 = now()                                   // hygienic: never collides with
    r  = body                                    // names at the call site
    log.debug("{label}", ms: now() - t0)
    r
}

unless(ready) { wait() }
x = timed("compute") { heavy() }                 // x inherits the checked type of heavy()

// Incorrect use is CHECKED:
// unless(42) { … }        // ERROR at call: `cond: expr` must be Bool
// timed("l", 5)           // ERROR at call: `body: block` expected, `5` is expression
```

For 95% of the "macro" cases (constants, conditional compilation, derivations,
code generation) one uses `const`/`comptime`/`@derive` (Point 7) — which are likewise
fully type-checked. Macros remain for genuine **syntactic** abstraction. In
*none* of these cases is there untyped token soup as with the C preprocessor.

## 9. Memory model

Invisible by default; the whole-program solver decides and proves:
- **Value types** (small) → copied, in register/on stack.
- **Non-escaping** objects → stack (`alloca`), no RC (escape analysis).
- **Shared/escaping** objects → heap + RC; **cycle-capable** types additionally
  a cycle collector (automatic, ~2 KB; omitted for acyclic programs).
- **`&x`** (optional) = borrowed reference without RC touch; lifetimes are
  *inferred*, not written. Omittable.

No `new`/`free`, no lifetime syntax, no `&mut` coercion. Feasibility details:
[EVALUATION.md](EVALUATION.md) §1.A.

## 9a. Mutation during iteration (the alias rule)

The problem: `for x in xs { xs.push(x) }` — RC holds the *object*, but the `push`
relocates the backing buffer while the iterator points into it. Iteration cannot
*at the same time* be a zero-cost pointer walk **and** allow mutation (see
[EVALUATION.md](EVALUATION.md) §7.2).

**Rule:** The compiler checks *specifically and locally* whether the loop body mutates
the iterated collection (or a local alias):
- **provably non-mutating** → zero-cost inline iteration (the normal case);
- **not provable** → **compile-time error**. No silent slow RC iteration —
  explicit intent is required:
  ```vire
  for x in xs.snapshot() { xs.push(x) }   // iterate a copy, mutate the original
  for i in 0..xs.len() { xs[i] = f(xs[i]) } // index access, bounds-checked
  ```

**The hard case is not local.** `for x in xs { xs.push(x) }` is seen immediately.
`for x in xs { f(xs) }` — where `f` mutates `xs` (or an alias) — is **not**: that is
interprocedural mutation info, so again whole-program. "Local" merely hides it.
Explicit decision (soundly chosen, cost accepted):
> **A call in the loop body that can reach the iterated collection (or an alias)
> forces a proof of non-mutation — otherwise a compile error
> (`snapshot()` required).** The proof comes from the **interprocedural
> mutation summary** that the solver builds for escape anyway ("does `f` write through
> parameter p?"). If it is missing (opaque/external call) → conservatively an error.

This is deliberately *sound-but-conservative* (better one false alarm + `snapshot()` than a
hole) and **the same load-bearing alias precision from §7** that reappears at the
iteration site — and the same analysis as "may this value go to `spawn`" (§10). The
quality of this summary determines how often the ergonomics tax (`snapshot()`) accrues;
that is part of the M0 risk, not separate from it.

## 10. Concurrency — *Point 1*

```vire
ch = Channel[Int]()                       // typed channel
spawn { for i in 0..100 { ch.send(i) } }  // lightweight thread; move values
for x in ch.take(100) { use(x) }

counter = Mutex(0)                         // shared mutable state only encapsulated
spawn { counter.lock(|n| n + 1) }         // access only in the lock closure
n = counter.get()

results = parallel_map(items, |x| heavy(x))   // fork-join data parallelism
a = Atomic(0); a.fetch_add(1)                 // atomic scalars
```
Rules (checked by the solver, **compile-time error** on violation): a value passed to
`spawn` must be (a) moved/copied **or** (b) a `Channel`/`Mutex`/
`Atomic`. Refcounts are atomic across threads. Guarantee: no data race on
safe types in the channel/mutex style; **no** deadlock-freedom promise.

## 11. Error handling — *Point 7*

**Go spirit, but without `null`.** Errors are **values**, explicit in the return type,
no exceptions, no hidden non-local control flow — that is Go's core. But
Vire does **not** use Go's `(T, error)` tuple with `nil`: a `nil` error would be a
`null` through the back door and violates guiding principle 4 (no `null`). Instead
**one** consistent model: `Result[T, E]` (E typed, often a sum type or the
`Error` interface). "Fallible" thus stands visibly in the signature (Go principle),
but typed and without null.
```vire
type ConfigError { NotFound(path: Str), BadSyntax(path: Str, line: Int) }

fn load(path: Str) -> Result[Config, ConfigError] {
    raw = read_file(path).wrap("Config {path}")?   // `?`: on Err return early + context
    parse(raw)                                       // Result as return
}

// Handling: explicit via match (that is Go's "val, err" branching, typed) …
match load("app.cfg") {
    Ok(cfg)               -> run(cfg)
    Err(NotFound(p))      -> run(default_at(p))
    Err(e)                -> return Err(e)
}
```
- `?` propagates `Err`/`None` (replaces Go's `if err != nil` cascade), *without* losing
  the explicitness — the signature still shows the fallibility.
- `.wrap(msg)` attaches context, keeps the chain; in debug the creation path
  (Point 8).
- **No `nil`, no `(T, Error)` tuple** — historical versions showed that; it
  is removed because it reintroduces `null`.
- `panic(msg)`/`assert(cond)` only for **programmer errors**, not for expected
  errors; abort with the crash path (Point 8).

## 12. Modules, visibility, packages

```vire
use std.io                    // standard library
use std.collections.{Map, Set}
use app.model as m            // alias

pub fn api() { }              // public (stable boundary; annotations recommended)
fn helper() { }              // module-private (default)
```
One module = one file; one package = one directory with `mod.vr`. No include,
no headers, no ordering dependency (whole-program, one pass).

## 13. FFI / interop

```vire
extern "C" {
    fn sqrt(x: F64) -> F64
    fn write(fd: I32, buf: Ptr[Byte], n: UInt) -> Int
}
use c "sqlite3.h" as sql       // header binding generator
unsafe { db = sql.sqlite3_open("app.db") }   // FFI call is unsafe
```
C native/complete; C++/Rust via the C ABI (see [EVALUATION.md](EVALUATION.md) §1.C).
`unsafe` blocks only at the boundary; within them `Ptr[T]`, `null_ptr()`, `x.addr()`.

## 14. Standard library: logger — *Point 6*

```vire
log.info("order", id: order.id, amount: order.total)   // structured
log.debug("cache", key: k, hit: found)                       // removed in release

with log.span("http", method: "GET", path: p) {              // context span
    log.info("start")                                         // inherits method/path
}
```
Levels are filtered **at compile time** (disabled calls = 0 instructions,
arguments not evaluated). Sinks (console/JSON/file) chosen at build. Source+line
automatic from the debug info.

## 15. Build and tooling — *Point 5*

- Compiler CLI (stable): `vire build`, `vire run`, `--emit=obj|llvm|asm`,
  `-O0|-O2|-O3`, `--release`, `--debug`, `--target=…`, `--deps` (Ninja `.d`).
- **Meson first-class:** Meson module `vire` (`vire.executable/static_library`);
  Vire targets link with C/C++/Rust targets (shared C-ABI objects). Recommendation:
  adopt Meson as the primary build system (saves a subsystem).
- Formatter `vire fmt`, test runner `vire test`, LSP for editors.

## 16. Debug info and crash paths — *Point 8*

- **Debug profile** (`--debug`): DWARF metadata (`!DILocation`) → gdb/lldb,
  breakpoints, source lines as with C.
- **Crash path**: `panic`, unhandled `Error`, bounds/null in debug print
  a stack trace `file:line:function`:
  ```
  panic: index 7 out of bounds for length 5
    at matrix.vr:42:14  in Matrix.get
    at main.vr:12:5     in main
  ```
- **Release**: off by default (0 overhead), optional `--release --backtrace`.
- **freestanding**: compact symbol table instead of libc `backtrace`.

---

## Appendix A — Mapping Vire → FastLLVM

| Vire | FastLLVM mechanism (status) |
|---|---|
| value type/struct | struct layout + escape analysis (✅) |
| sum type + `match` | tagged union → `switch` (✅ patterns in backend) |
| generics/value generics | monomorphization before the IR (✅ as inliner) |
| traits | static resolution → direct call (✅ devirt) |
| no `null`, bounds | null-/bounds-check elision (✅ GVN) |
| memory/`&` | RC + escape + borrow-slot elision + acyclicity (✅) |
| threads/`Atomic`/`Mutex` | atomic RC + pthreads + monitor (✅ `--threads`) |
| `comptime`/reflection | **new:** front-end evaluator over the type graph |
| macros | **new:** AST transformation in the front-end |
| errors + `?` | pending/value model (✅ backend), `?` as lowering |
| debug/backtrace | LLVM debug metadata (⚙️ backend build-out) |
| FFI `extern "C"` | direct LLVM declaration (✅) |

To build new: **front-end** (lexer, parser, Hindley-Milner inference,
`comptime` evaluator, macro expander) and the lowering to `crates/ir` **in
SSA**. Solver + backend remain.

## Appendix B — Example overview

See [examples/](examples/): `sieve`, `shapes` (traits/generics), `tree`
(recursive generics), `wordcount` (maps/iterators), `concurrent` (threads/channels),
`ffi`, as well as the feature demos `reflection`, `macros`, `error`, `logger`,
`comptime_matrix`.

## 9b. `capsule` — isolated arena scope (for hot/risky things)

`capsule(a, b) { … }` runs the body in its **own arena** (pure form):
the `()` inputs are **deep-copied into the arena** (no `&`, no move — only
the deep copy makes them region-local), only the block value leaves the capsule (deep-
copied into the outer heap). Objects allocated in the body are **arena-local → no
RC, no cycle collector**; on exit the arena is freed en bloc (also
on panic — fault containment).

Purpose: (a) **isolation + fault containment** (the hard guarantee) — risky/
untrusted code can by construction only reach the arena, because the body owns no
outer pointer. (b) **performance** — RC-/collector-free in the body; the
net gain depends however on copy-in+copy-out and pays off only when much work funnels
into a **small** block value (not large-graph-in/large-graph-out). The
copy-free variant `capsule(&x)` breaks isolation and is open research.
Full rationale + design: [CAPSULE-EVALUATION.md](CAPSULE-EVALUATION.md).
