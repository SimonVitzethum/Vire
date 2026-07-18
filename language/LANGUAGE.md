# Vire — Language Design & Syntax (Tour)

*Goal: the ergonomics of Python, the performance & reach of C/C++/Rust,
memory safety without annotations, AOT via FastLLVM's backend. This is the
**quick tour**; the full reference is in [REFERENCE.md](REFERENCE.md),
the feasibility in [EVALUATION.md](EVALUATION.md), the assessment of the eight
additional features (multithreading, templates, comptime reflection, macros, Meson,
logger, Go-style error handling, debug crash paths) in
[FEATURES-EVALUATION.md](FEATURES-EVALUATION.md).*

## Guiding Principles

1. **No harder than Python.** No type annotations needed, no semicolons,
   no memory management. What you can do in Python you can do here — only with
   `{ }` blocks instead of meaning-bearing indentation (clear for editors, tools,
   pasting; no indentation traps).
2. **Statically typed by inference.** No type is written, but every one is known
   (Hindley-Milner + local bidirectionality). Errors at compile time.
3. **Memory is invisible.** No `new`/`free`, no lifetimes, no `&mut`.
   The solver decides stack/heap/RC and proves safety (bounds, null,
   use-after-free). You write logic, not bookkeeping.
4. **Safety by construction.** No `null` (→ `Option`), no
   uninitialized values, bounds checked (and optimized away where provable). `unsafe` only
   opt-in at the C boundary.
5. **One core, three worlds.** Value types + C layout + freestanding = C/Zig territory.
   Traits + generics + sum types + pattern matching = Rust territory. Inference +
   automatic memory = Python/Go territory.

File extension `.vr`. Entry point: the free function `main`. Statements end
at the end of line (semicolon optional); blocks are enclosed in `{ }`. The **last expression
of a block is its value** (like Rust) — `return` only for early exit.

---

## 1. Values, Bindings, Functions

```vire
x = 5                 // immutable binding (like `let`), type inferred: Int
mut total = 0         // mutable, explicit
total = total + x     // ok, because `mut`
name = "Welt"         // Str
pi = 3.14159          // Float (= F64)

fn add(a, b) = a + b              // expression function; a,b,return inferred
fn greet(name) {                  // block; last expression is the value
    print("Hallo, {name}")        // string interpolation with { }
}

fn main() {
    print(add(2, 3))              // 5
    greet("Vire")
}
```

Immutable by default (one `mut` more than Python, but safer and
more optimizable). Rebinding without `mut` is a compile-time error.

## 2. Base Types

| Category | Types |
|---|---|
| Integer | `Int` (=I64), `I8 I16 I32 I64`, `UInt` (=U64), `U8 U16 U32 U64`, `Byte` (=U8) |
| Floating point | `Float` (=F64), `F32` |
| Other | `Bool`, `Str`, `Char`, `Unit` (empty, like `()`), `Ptr[T]` (`unsafe` only) |

Integers are fixed-width and overflow-checked (debug) / wrapping (release,
configurable). `Int` is the ergonomic default, the exact widths are for systems code
and FFI.

## 3. Composite Types — `type`

```vire
type Point {                      // product (struct), value type, no header
    x: Float
    y: Float

    fn dist(self) = sqrt(self.x*self.x + self.y*self.y)   // method
    fn scaled(self, k) = Point(self.x*k, self.y*k)
}

p = Point(1.0, 2.0)               // positional
q = Point(x: 3.0, y: 4.0)         // named
print(q.dist())                   // 5.0
```

Sum types (algebraic, replacing enums **and** `null`):

```vire
type Shape {
    Circle(radius: Float)
    Rect(w: Float, h: Float)
    Point                          // variant without data
}

fn area(s: Shape) -> Float {
    match s {
        Circle(r)  -> 3.14159 * r * r
        Rect(w, h) -> w * h
        Point      -> 0.0
    }
}
```

`Option` and `Result` are ordinary sum types from the stdlib, not a special case:

```vire
type Option[T] { Some(T)  None }
type Result[T, E] { Ok(T)  Err(E) }
```

## 4. No `null` — `Option` + `?`

```vire
fn find(xs: List[Int], target: Int) -> Option[Int] {
    for i, x in enumerate(xs) {
        if x == target { return Some(i) }
    }
    None
}

match find([3, 7, 9], 7) {
    Some(i) -> print("Index {i}")     // Index 1
    None    -> print("fehlt")
}

// `?` unwraps Some/Ok or exits early (None/Err propagate):
fn first_plus_one(xs: List[Int]) -> Option[Int] {
    head = xs.first()?               // on None: immediately return None
    Some(head + 1)
}
```

## 5. Errors — `Result` + `?`

```vire
fn read_config(path: Str) -> Result[Config, Error] {
    text  = read_file(path)?         // propagates Err
    lines = text.split("\n")
    parse(lines)                     // Result as return value
}

fn main() {
    match read_config("app.cfg") {
        Ok(cfg) -> run(cfg)
        Err(e)  -> print("Fehler: {e}")
    }
}
```

No exceptions, no `try/catch` — errors are values, and `?` makes them lightweight.

## 6. Generics & Traits (Type Classes)

```vire
trait Ord {
    fn cmp(self, other: Self) -> Int
    fn less(self, other: Self) = self.cmp(other) < 0   // default method
}

fn max[T: Ord](a: T, b: T) -> T {
    if a.less(b) { b } else { a }
}

// Implement a trait for your own type:
impl Ord for Point {
    fn cmp(self, other) = compare(self.dist(), other.dist())
}

biggest = max(Point(1,1), Point(3,4))   // T = Point, monomorphized
```

Generics are **monomorphized** (one specialized, inlined variant per
type combination) — zero-cost like C++ templates/Rust, without their syntactic burden.

## 7. Collections & Iteration

```vire
xs = [1, 2, 3, 4]                 // List[Int]
m  = ["a": 1, "b": 2]             // Map[Str, Int] (`:` → Map; `[:]` = empty)
s  = Set[1, 2, 3]                 // Set[Int] (`{}` is ONLY a block)

for x in xs { print(x) }
for k, v in m { print("{k}={v}") }
for i in 0..10 { }                // Range 0..9
for i in 0..=10 { }               // inclusive

// Functional, but without hidden costs (iterators are inlined):
evens   = xs.filter(x -> x % 2 == 0)
doubled = xs.map(x -> x * 2)
sum     = xs.fold(0, (acc, x) -> acc + x)

// Comprehensions (Python-familiar):
squares = [x*x for x in xs if x > 1]
```

Lambdas: `x -> expression` (one argument), `(a, b) -> expression` (multiple).

## 8. Control Flow

```vire
if x > 0 { print("pos") } elif x == 0 { print("null") } else { print("neg") }

// `if` is an expression:
label = if score >= 50 { "bestanden" } else { "durchgefallen" }

while running { tick() }

for x in xs {
    if x < 0 { break }
    if x == 0 { continue }
    process(x)
}
```

## 9. Memory: invisible, but controllable

Default: **do nothing.** The solver decides.

```vire
p = Point(1.0, 2.0)     // does not escape → stack, no RC
node = Node(value: 5)   // hooked into a list → heap + RC, automatic
q = p                   // move/copy/share — inferred, always safe, zero-cost where possible
```

- Small value types: copied (like `int`).
- Non-escaping objects: stack (`alloca`), no RC.
- Shared/cyclic objects: heap + RC + cycle collector — automatic.

For hot paths, *optional* explicit borrowing (not required, no lifetimes):

```vire
fn sum(xs: &List[Int]) -> Int {   // `&` = borrowed, no RC touch
    mut acc = 0
    for x in xs { acc = acc + x }
    acc
}
```

`&` is an *optimization assurance*, not a mandatory annotation system: it can be omitted,
and the solver derives borrows anyway (as it already does today for `this`/parameters).

## 10. C Interop (the universal glue)

```vire
extern "C" {
    fn sqrt(x: F64) -> F64
    fn write(fd: I32, buf: Ptr[Byte], n: UInt) -> Int
}

// Bind entire headers (the generator produces the signatures):
use c "sqlite3.h" as sql

fn main() {
    unsafe {                         // only the FFI line is unsafe
        db = sql.sqlite3_open("app.db")
    }
}
```

The C ABI is direct and complete; C++/Rust libraries with a C surface likewise.
Pure C++ templates / idiomatic Rust: via generated bindings or not at all
(see [EVALUATION.md](EVALUATION.md) §1.C — the same boundary as for any language).

## 11. Concurrency (CSP, like Go — lightweight)

```vire
ch = Channel[Int]()

spawn {                              // lightweight thread
    for i in 0..100 { ch.send(i) }
}

mut total = 0
for x in ch.take(100) { total = total + x }
print(total)
```

Under threads, refcounts automatically become atomic (FastLLVM `--threads` already
does this today). No data race on safe types (shared mutation only via `Channel`
or `Atomic[T]`/`Mutex[T]`).

## 12. Modules & Visibility

```vire
use std.io                           // standard library
use math.{sin, cos}                  // selective

pub fn api_call() { }                // public (part of the stable boundary)
fn helper() { }                      // module-private
```

Public functions at the module boundary *may* carry type annotations (documentation +
inference anchor); inside, everything stays inferred.

## 13. Complete Mini-Program

```vire
// Word frequency — shows inference, Map, iteration, Option, errors in ~10 lines
use std.io

fn word_counts(text: Str) -> Map[Str, Int] {
    mut counts = [:]                                 // empty Map[Str, Int]
    for word in text.lower().split_whitespace() {
        counts[word] = counts.get(word).or(0) + 1    // Option.or → default
    }
    counts
}

fn main() -> Result[Unit, Error] {
    text   = read_file("buch.txt")?
    counts = word_counts(text)
    for word, n in counts.items().sorted_by(pair -> -pair.1).take(10) {
        print("{n:5}  {word}")
    }
    Ok(())
}
```

Reads like Python, compiles to a native, memory-safe, RC-eliminated
binary.

---

## What is deliberately *missing* (simplicity through omission)

- No lifetimes, no `&mut`/`&` requirement, no borrow checker in the way (the solver
  infers).
- No inheritance tree (traits/composition instead of a class hierarchy).
- No headers/declarations (whole-program, single pass).
- No macros/`unsafe` in everyday use (only at the FFI boundary).
- No runtime reflection/`eval` (AOT, closed world).
- No null, no exceptions, no implicit conversions.

## Mapping onto FastLLVM (why it lowers "simply")

| Vire construct | FastLLVM IR / solver |
|---|---|
| `type` (product) | struct layout, value type; escape analysis → stack/heap |
| `type` (sum) | tagged union; `match` → `switch` + field access |
| Generics | monomorphization before the IR (like inlining today) |
| Traits | static resolution → direct calls (today: devirt) |
| `Option`/no null | null checks eliminated by construction (today: null elision) |
| Bounds | bounds-check elision (GVN, built today) |
| Memory | RC + escape + acyclicity (built today) |
| `&`/borrow | borrow-slot / parameter-RC elision (built today) |
| SSA from the start | **not needed: no GVN fight against javac slot reuse** |

The entire backend and solver stack stays unchanged; new is only the front end +
type inference + lowering. See [examples/](examples/) for programs across all
target areas (systems-level, functional, concurrent, FFI, numeric).
