# Vire — Evaluation of the Eight Requested Features

An honest assessment of each requirement: does it fit the philosophy (Python-easy,
safe, AOT, ~runtime-free, Closed World)? What would the design look like? What does
it cost, and where are the limits? Order = your list. A brief verdict per item
first, then the rationale.

Feasibility legend: 🟢 clearly feasible & fits · 🟡 feasible with tailoring · 🔴
problematic as literally stated, a better substitute is recommended.

---

## 1. Multithreading, very easy to use, with race-condition safety 🟡

**Verdict:** The *ergonomics* are easy and the runtime already exists (FastLLVM
`--threads`: atomic refcounts, pthreads, global monitor). **Fully guaranteed**
data-race freedom *without* ownership annotations is, however, precisely the point
that is free in no language — this needs an honest tailoring.

**Why the tension is real.** Rust guarantees race freedom via `Send`/`Sync` +
borrow checker — that is, via *exactly the annotations that Vire wants to avoid*. Go
is ergonomic but guarantees **nothing** (data races are possible, only a runtime
detector exists). Vire wants Go's lightness **and** more safety than Go, without
Rust's burden. This is achievable, but not as a total guarantee on the first
attempt.

**Design — safety by construction, not by annotation:**

```vire
// 1. Standard path: share ONLY over channels (CSP). Values that pass through a
//    channel change owner (move) — no shared mutable state.
ch = Channel[Task]()
spawn { for t in tasks { ch.send(t) } }          // Producer
for t in ch { handle(t) }                         // Consumer

// 2. Shared mutable state only when explicitly encapsulated — the type enforces the lock:
counter = Mutex(0)
spawn { counter.lock(|n| n + 1) }                 // access only inside the lock closure
total = counter.get()

// 3. Fork-join for data parallelism, without channels:
results = parallel_map(items, |x| heavy(x))       // library primitive
```

**What Vire can guarantee (and how):**
- **No data race on shared mutable value types**, if sharing is *only* permitted via
  `Channel`/`Mutex`/`Atomic`. The solver has whole-program visibility and can verify
  that a value passed to `spawn` is either (a) copied/moved or (b) carries one of
  these sync types — otherwise a **compile-time error**. This is a *lightweight*
  Send/Sync inference (not a lifetime system): "may this value cross a thread
  boundary?" is yes/no, not an annotation calculus.
- **Safe RC under threads** (atomic refcounts) — already exists.
- **Deadlock freedom: no** (Rust does not guarantee this either).

**Honest limit:** The Send/Sync inference without annotations is conservative at
module and FFI boundaries (there, possibly a `@shared`/`@threadsafe` marker —
optional, as with the public type annotations). Fully automatic race freedom for
*arbitrary* alias graphs is open; for the channel/mutex style (99% of real
concurrent code) it is feasible. **Recommendation: advertise "safe by
construction", not "race-free for everything".**

---

## 2. Template programming 🟢 (as generics + traits + `comptime`)

**Verdict:** Feasible and stronger than C++ templates — without their syntactic
burden and error-message hell. The monomorphizer already exists (today as an
inlining pass).

Vire separates two things that C++ conflates into "templates":
- **Parametric polymorphism** → generics with trait bounds (§6 in
  [LANGUAGE.md](LANGUAGE.md)). Monomorphized = zero-cost, but with *checked* bounds
  (`[T: Ord]`), i.e. clear errors instead of pages of template spew.
- **Value/type metaprogramming** → `comptime` (see item 3), which computes over
  types at compile time and generates code. This covers the "template
  metaprogramming" cases (type lists, conditional instantiation, `if constexpr`) —
  but as *normal code that runs at compile time*, not as a separate template
  language.

```vire
// Generic with a bound — checked, monomorphized:
fn max[T: Ord](a: T, b: T) -> T { if a.less(b) { b } else { a } }

// Value generics (like C++ non-type parameters), for fixed sizes:
type Matrix[T, comptime R: Int, comptime C: Int] {
    data: [T; R * C]                  // size known at compile time → stack
    fn get(self, r: Int, c: Int) -> T = self.data[r * C + c]
}
m = Matrix[Float, 3, 3]()
```

**Limit:** No Turing-complete instantiation chaos like C++ (deliberately).
Recursive `comptime` computation yes, but with a recursion limit and clear
diagnostics.

---

## 3. Compile-time reflection 🟢 (strong fit)

**Verdict:** The most natural fit of all. Closed-World AOT means: the compiler *has
the entire type/program graph*. Reflection at compile time is therefore powerful
**and** zero-cost (no runtime metadata ballast — unlike Java/C#).

```vire
// @typeinfo(T) yields a comptime-traversable description of the type.
fn to_json[T](value: T) -> Str {
    comptime for field in @typeinfo(T).fields {      // loop runs in the compiler
        // generates per-field serialization code — none of it at runtime
        emit("\"{field.name}\": {to_json(value.@field(field.name))}")
    }
}

// Derivations without macro magic: @derive uses reflection.
type User { name: Str, age: Int }
@derive(Json, Eq, Hash)                    // generates methods via @typeinfo
```

**Capabilities:** enumerate a type's fields/variants/methods, read attributes,
compare types, generate code at compile time (`emit`), static assertions (`comptime
assert`). This simultaneously replaces **derivation macros** (`@derive`),
**serialization**, **ORM mapping**, **schema generation** — all without runtime.

**Deliberate limit:** **No** runtime reflection (`getClass().getFields()` at
runtime). That would go against "AOT/no runtime ballast". Anyone who needs genuine
dynamic behavior generates it at compile time or uses a sum type.

---

## 4. A custom (optional) preprocessor 🟢

**Clarification (user):** What is meant is **not** the C preprocessor, but a
**custom preprocessor usable on demand**. That makes the matter clear and positive:
Vire delivers exactly that — only the "preprocessor" is **not a text substituter**,
but the **AST/`comptime` layer**. This is the modern, safe form of a custom
preprocessor: opt-in, hygienic, type-checked, tooling-friendly.

Concretely it is three related, *optional* mechanisms:
- **`comptime`** — arbitrary code that runs *before* the actual compilation inside
  the compiler (constant tables, codegen from schemas, reflection). This *is* a
  preprocessor phase, only typed and in the same language.
- **`@if`/`@when`** — conditional compilation (platform/feature switches) as
  directives, replacing `#ifdef` — but expression-based and checked.
- **hygienic *and type-safe* macros** — syntactic abstraction on the AST (no name
  capture), with **typed parameters** (`cond: expr`, `body: block`, …) and **full
  type checking after expansion**. Incorrect use = a compile error *at the call
  site*, no ill-typed result (details: [REFERENCE.md](REFERENCE.md) §8).

**Deliberately *not* the C text preprocessor** (`#define` token-gluing):
unhygienic (captures names), **type-blind**, debugger-hostile, breaks tools. Vire's
preprocessor is the exact opposite: **type-safe at every point**. A *custom*
preprocessor yes — but as typed AST/`comptime`, not as text. What one classically
does with the preprocessor works more cleanly here:

| Preprocessor purpose | Vire substitute |
|---|---|
| Constants (`#define N 10`) | `const N = 10` (typed) |
| Conditional compilation (`#ifdef`) | `@if(cfg.debug) { … }` / `comptime if` |
| Platform switches | `@when(os == .linux)` (comptime) |
| Code generation/DRY (`X-Macros`) | `comptime` reflection + `emit` (item 3) |
| Include guards | module system (no include) |
| Inline functions | normal functions (the inliner decides) |

Where genuine **syntactic abstraction** is needed (custom control constructs,
DSL-like blocks), Vire offers **hygienic macros** (Rust/Scheme style): they operate
on the AST, capture no names, have **typed parameters**, and are **fully
type-checked after expansion** (no ill-typed result possible):

```vire
macro unless(cond, body) { if not (cond) { body } }   // hygienic, AST-based
unless(done) { retry() }

macro timed(label, body) {                            // times a block
    t0 = now(); body; log.debug("{label}: {now() - t0}")
}
```

**Verdict:** A custom, optional preprocessor — **yes, fully** (as `comptime` +
`@if` + hygienic macros). Only the *mechanics* are AST/typed instead of text
substitution — the same capability, safely realized. Whoever wants it, uses it;
whoever does not, never sees it.

---

## 5. Build-system interop, first-class Meson 🟢🟡

**Verdict:** Feasible and strategically smart (fits the C-interop core).
"First-class Meson" concretely means two things, the first easy, the second some
work:

1. **Meson can build Vire sources natively.** Meson supports languages via a
   compiler interface (as for C/C++/Rust/D). For this, Vire provides:
   - a compiler with **stable CLI flags** (`--emit=obj|llvm|ir`, `-c`, `-o`,
     `-I`, dependency output `--deps` in Ninja/`.d` format),
   - a **Meson module** `vire` (`vire.executable(...)`, `vire.static_library(...)`),
   - clean **C-ABI output** (`.o`/`.a`), so that Vire targets link with C/C++/Rust
     targets in the same Meson project.
   This is straightforward, because Vire goes through clang to object files anyway.
2. **Vire projects consume Meson/pkg-config dependencies first-class:**
   ```vire
   // build.vr — declarative build, reads pkg-config/Meson deps
   project("app", deps: ["sqlite3", "openssl"])   // resolved via pkg-config
   exe("app", src: ["main.vr"], link: deps)
   ```
   The C-header binding generator (item from EVALUATION.md §1) pulls in the headers
   of the Meson deps automatically.

**Limit/recommendation:** A *custom* build system (like Cargo) and *first-class
Meson* at the same time splits the effort. Recommendation: **adopt Meson as the
primary build system** (not just "interop") — this saves an entire subsystem and
docks immediately onto the C/C++ world, which Vire targets anyway. A thin `vire`
wrapper for onboarding (`vire build`) delegates to Meson.

---

## 6. A logger — but done well 🟢

**Verdict:** Clearly feasible and a real lever, because AOT + comptime allow a
logger that is **switched off at compile time** (disabled levels = **zero**
instructions, not just a runtime `if`).

"Done well" concretely means:
- **Structured fields**, not string-gluing: `log.info("login", user: id, ms: t)`.
- **Compile-time level filter:** below the built minimum level, the call is
  **optimized away** (comptime `if` around each call), including argument
  evaluation. → In release with `level=warn`, a `log.debug(...)` costs exactly 0.
- **Lazy arguments:** expensive fields evaluated only if the level is active.
- **Context/spans:** `with log.span("request", id: rid) { … }` attaches fields to
  all logs in the block (structured, no global statics).
- **Pluggable sinks:** console (colored, human), JSON (machine), file, syslog —
  chosen at build time, no reflection overhead.
- **Source+location** automatically via debug info (item 8): every line carries
  `file:line` without manual work.

```vire
log.info("order", id: order.id, amount: order.total)
log.debug("cache", key: k, hit: found)     // completely removed in release (warn)

with log.span("http", method: "GET", path: p) {
    log.info("start")                        // inherits method/path automatically
    handle()
}
```

Implementation: pure stdlib over `comptime` + structured sink traits. No language
feature needed — but shipped as *batteries included*.

---

## 7. Error handling inspired by Go 🟡 (Go philosophy, verbosity defused)

**Verdict:** Fits the philosophy (errors are values, **no** exceptions, no hidden
non-local control flow) — that was already the plan. "Inspired by Go" means:
**explicit** and **as a value**, but Vire fixes Go's two well-known weaknesses
(verbosity and missing stack traces).

**Go core adopted:**
- Errors are ordinary values of an `Error` interface, **explicit** in the return
  type — you *see* from the signature that something can go wrong.
- No `throw`/`catch`, no unwinding, no invisible error paths.

**Go's weaknesses fixed:**
- Instead of `if err != nil { return err }` on every line: the `?` operator
  propagates (early return with the error) — the *same* value-based flow, only
  without boilerplate. But you *can* always check explicitly (Go style) when you
  want to handle differently.
- **Error wrapping with context** (like Go's `fmt.Errorf("...: %w", err)`, only
  typed): `err.wrap("could not read {path}")` — the chain remains inspectable.
- **Stack traces**: in debug builds, every error carries the creation path (item 8)
  — the thing Go painfully lacks.

**Important — no `nil`, no `(T, error)` tuple:** Go's tuple with `nil` would bring
`null` back through the back door and violate guiding principle 4. Vire keeps Go's
*spirit* (errors as explicit values, visible in the signature, no exception flow)
in **one** typed model: `Result[T, E]`.
```vire
type ConfigError { NotFound(path: Str), Permission(path: Str) }

// Signature makes fallibility visible (Go principle), but typed & without null:
fn load(path: Str) -> Result[Config, ConfigError] {
    raw = read_file(path).wrap("Config {path}")?   // `?` instead of if-err cascade
    parse(raw)
}

// Explicit, typed handling via match (= Go's "val, err" branching):
match load("app.cfg") {
    Ok(cfg)             -> run(cfg)
    Err(NotFound(p))    -> run(create_default(p))
    Err(Permission(p))  -> fatal("no permission: {p}")
    Err(e)              -> { log.error("start", err: e); return Err(e) }
}
```

**Tailoring:** Vire keeps Go's *value explicitness* + `?` sugar, but typed
(`Result`, more than Go's bare `error`) and **without `nil`**. No panic-for-
everything. `panic`/`abort` remains for **programmer errors** (invariant
violations), not for expected errors — with a crash path (item 8).

---

## 8. Debug symbols with crash paths in debug builds 🟢

**Verdict:** Clearly feasible; LLVM provides the foundation. Two build-out stages:

1. **DWARF debug info** (`--debug`/debug profile): the backend emits
   `!DILocation`/`!DISubprogram` metadata (LLVM can do this natively) → `gdb`/`lldb`,
   breakpoints, variables, source lines work as with C. Pure backend work (passing
   line numbers through the front-end into the IR).
2. **Crash paths at runtime** (debug builds): on `panic`, an unhandled error, a
   bounds/null violation, the runtime prints a **stack trace with
   `file:line:function`** instead of just "abort". Implementation: `backtrace()`
   over the frame pointers + symbol resolution from the DWARF info (or a compact
   custom symbol-table format for the freestanding target).

```
panic: index 7 out of bounds for length 5
  at matrix.vr:42:14   in Matrix.get
  at solver.vr:88:9    in step
  at main.vr:12:5      in main
```

- In **release**, off by default (0 overhead, small binaries) — optionally
  switchable on (`--release --backtrace`) for production diagnostics.
- Fits item 7: created `Error` values attach the creation path in debug.
- For the **freestanding/seL4 target**: a lean symbol table + `plat_puts`, no
  libc `backtrace` needed.

---

## Overall picture

| # | Feature | Verdict | Core |
|---|---|---|---|
| 1 | Multithreading + race safety | 🟢* | easy + "safe by construction" (channels/mutex + Send inference) — confirmed by the user as **enough**; no total guarantee over arbitrary alias graphs |
| 2 | Template programming | 🟢 | generics+traits (monomorphized) + `comptime` instead of C++ templates |
| 3 | Compile-time reflection | 🟢 | strongest fit (Closed-World), zero-cost, replaces `@derive`/serialization |
| 4 | Custom optional preprocessor | 🟢 | as `comptime`/`@if`/hygienic macros (AST instead of text) — not the C preprocessor |
| 5 | Meson first-class | 🟢🟡 | Meson module + stable CLI; recommendation: *adopt* Meson instead of a custom build |
| 6 | Logger done well | 🟢 | structured, comptime-switched-off (0 cost), spans, sinks |
| 7 | Error handling à la Go | 🟢* | values+explicit (Go spirit), `?`, wrapping, typed — **`Result`, no `nil`** |
| 8 | Debug symbols + crash paths | 🟢 | DWARF via LLVM + runtime backtrace in debug; release 0 overhead |

*(1) and (7) `🟢*`: approved with the tailored scope — (1) easy +
safe-by-construction is **enough** (user confirmed), no race freedom for arbitrary
alias graphs; (7) Go spirit via typed `Result`, **without** Go's `nil`.*

**One point stays deliberately tailored:** "race-free for everything" cannot be
seriously promised without ownership annotations — "safe by construction for the
channel/mutex style" is the honest and **sufficient** commitment. The hard core
behind it (alias precision) is openly named in [EVALUATION.md](EVALUATION.md) §7;
the `spawn` Send check is the same analysis as the iterator rule
([REFERENCE.md](REFERENCE.md) §9a), only conservative (when in doubt, requires
mutex/move).

**All eight dock onto existing FastLLVM capabilities:** threads/atomic RC (1),
monomorphization/inliner (2), whole-program type graph (3, for reflection), the
solver (1, Send inference), clang→object (5), comptime as a front-end evaluator (2,3,
4,6), the value/panic error model (7,8), LLVM debug metadata (8). What is new to
build is the front-end (lexer/parser/inference/`comptime` evaluator) — the backend
stack stays. Implementation order & tasks: [../TODO.md](../TODO.md).
