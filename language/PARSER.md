# Vire — Parser and Front-End Plan

Concrete blueprint for lexer → parser → AST (pipeline phase P1 in
[../TODO.md](../TODO.md)). Goal: a **hand-written** parser (recursive
descent + Pratt for expressions) — predictable, good error messages, no
parser generator. Crate: `crates/vire_frontend` (lexer+parser+AST).

Why hand-written: The ergonomics ("as simple as Python") depend half on
**error messages** ([EVALUATION.md](EVALUATION.md) §5). An LR generator yields
generic "unexpected token" errors; a recursive-descent parser can give an
*explanatory* error + fix suggestion at any point.

---

## 1. Design Pre-Decisions (keeping the grammar simple)

Vire is deliberately designed so that the parser needs **hardly any backtracking**:

### 1.1 The Naming Law — **three** lexical classes (grammar, not convention)
Capitalization carries a large part of the disambiguation (type
application `List[Int]` vs. indexing `xs[i]`, product vs. sum variant in the
`type` body). "Capitalization = type" **alone breaks on constants**, however:
`const MAX = 1024` is an uppercase *value*, and `MAX[0]` would be incorrectly
type application. Therefore **three** purely lexically distinguishable classes:

| Class | Form | Meaning | Example |
|---|---|---|---|
| `UpperCamel` | uppercase letter, then **at least one lowercase letter** | **type / constructor** | `List`, `Point`, `Circle` |
| `SCREAMING_SNAKE` | only uppercase letters/digits/`_` | **const value** | `MAX`, `PI`, `CRC_TABLE` |
| `lower_snake` | begins lowercase | **value / function / variable** | `xs`, `parse`, `count` |

With this, `List[Int]` (UpperCamel `[`) is a type argument, `MAX[0]`/`xs[i]`
(SCREAMING/lower `[`) is indexing — decidable **without name resolution**. This is a
**binding** style corset (a wrongly named identifier is an error,
not a lint) and part of the grammar.

### 1.2 `[]` — four roles, all resolved via position + name class
`[]` is generics, indexing, list literal, and fixed array `[T; N]`:
- After `UpperCamel`/fn name in type/declaration position → **generics** (`List[Int]`, `fn max[T]`).
- After a value expression → **indexing** (`xs[i]`).
- In expression position, opening → **list/map literal** (§1.3).
- `[T; N]` in type position → **fixed array** (the semicolon separates).
- **No type arguments on values**: `value[Type]` is *always* indexing. Explicit
  fn type arguments only via **turbofish `#[T]`** (`collect#[List[Int]]()`) for the
  rare return-type-driven cases; otherwise inference. (Rust's `::<>` exists
  for exactly this reason.)

### 1.3 `{}` = **block only.** Map/Set are marked (decision)
`{}` is **exclusively** a block (statements; last expression = value). The
Python ambiguity `{}` = dict-or-set-or-block is **not** adopted —
it only lets the reader know after the `{` what is being opened. Instead
(the Swift model, `:` separates unambiguously):
- **List:** `[a, b, c]`, empty `[]`.
- **Map:** `[k: v, k2: v2]`, empty `[:]`.
- **Set:** `Set[a, b]` (constructor literal, `UpperCamel[` → unambiguous).
So `{` is **always** a block — no lookahead, no late aha moment for the reader.

### 1.4 Binding vs. assignment without `let` (decision)
No `let`: `x = 5` binds, `x = 6` assigns. The ambiguity is resolved by **one
rule** (in `resolve`, not in the parser): the **first** `x =` in a
scope is a **binding** (immutable, unless `mut x`); every **further** `x =`
in the same scope is an **assignment** and requires `x` to be `mut` — otherwise
an **error** (no silent rebind). **Shadowing** exists only via *inner* scopes.
This makes intent expressible and catches the typo. (A deliberate trade: one
keyword fewer than Rust, but the scope rule as a replacement.)

### 1.5 Other Hard Rules
- **`{ }` blocks are expressions** (`if`/`match`/block yield values).
- **`->` in three clear places**, separated by position: return type
  (`fn f() -> T`), lambda (`x -> e`), match arm (`Pat -> e`).
- **Keyword-led items** (`fn type trait impl use const macro extern`) → one
  token lookahead.
- **String interpolation** is active on *every* string (`"{name}"`, no `f`
  prefix). Literal braces are **doubled**: `{{` → `{`, `}}` → `}`.

### 1.6 Two Expression Ambiguities (detailed rules below)
1. Lambda `x -> e` / `(a,b) -> e` **or** parenthesized expression → §4.3.
2. Struct construction vs. block after `if`/`for`/`while` scrutinee → §4.4
   (construction is `Point(…)` anyway, not `Point { … }` — no conflict).

---

## 2. Lexer (`lexer.rs`)

### 2.1 Token Kinds
```
// Literals
Int(i128) Float(f64) Str(Vec<StrPart>) Char(char) True False
// Identifiers & keywords
Ident(sym)  Keyword(kw)
// Brackets/separators
( ) [ ] { }  , :  ;  ->  =>  .  ..  ..=  @
// Operators (Pratt-relevant)
+ - * / %  +% -% *%      // wrap variants
== != < <= > >=
and or not  &  |  ^  <<  >>
= += -= *= /=            // assignment (only as a statement)
?                        // error propagation (postfix)
// control
Newline  Eof
```

### 2.2 Lexical Details
- **Idents:** Unicode-XID; keywords are reserved idents (table).
- **Numbers:** `42 0xFF 0b1010 0o17 1_000 3.14 1e-9 42i32 7u8 2.0f32`.
- **Strings with interpolation:** `"a{expr}b"` becomes `Str([Lit("a"), Expr(tokens),
  Lit("b")])`. The lexer balances `{}` within the string and lexes the inner
  expression recursively. Format spec: `{x:6}`/`{x:.2}` → `Expr` + `FormatSpec`.
  Raw: `r"…"`; multi-line: `"""…"""`.
- **Comments:** `//` to end of line; `/* … */` **nestable** (count depth).

### 2.3 Newline Handling — the **full** Go continuation model
Indentation-*in*sensitive, newline-*sensitive* (Go's model, not "whitespace-
insensitive"). Two rules, both needed — chains really are multi-line:

**(a) End of line — when there is *no* terminator (continuation).** A `Newline` is
**not** emitted if the *last significant token of the line* cannot
terminate a statement: binary/unary operator, `,`, `.`, opening bracket
`(` `[` `{`, `->`, `=`/`+=`/…. It is emitted after: ident, literal, `)` `]` `}`, `?`,
`self`, `true`/`false`, `return`/`break`/`continue`.

**(b) Start of line — the leading token suppresses the terminator.** Even if (a)
would set a terminator: if the *next* line begins with `.` (method chain),
the terminator is **suppressed** (continuation). Conversely, a leading
`(`/`[`/`-` is **not** a continuation signal — otherwise `g()\n(x)` would become `g()(x)` and
`a\n-b` would merge into `a-b`; they start a new statement.
```vire
x = foo
    .bar()      // leading `.` → continuation
    .baz()
y = g()
(x)             // leading `(` → NEW statement (not g()(x))
```
Implementation: rule (a) in the lexer (remember the last token); rule (b) as lookahead —
before emitting a terminator, check the next significant character, and on
`.` suppress it. The parser treats `Newline`/`;` as a *StmtEnd* and allows a
`Newline` before `.` in postfix position (redundantly safeguarded). *(A leading `[`
is the nastiest case — it collides with indexing; therefore deliberately **not** a
continuation signal: a start-of-line `[` is always a new list/map literal
statement.)*

---

## 3. AST (`ast.rs`)

```
Module   = { Item }
Item     = FnDef | TypeDef | TraitDef | ImplDef | Use | ConstDef | MacroDef | Extern
FnDef    = "fn" name [Generics] "(" Params ")" ["->" Type] (Block | "=" Expr)
TypeDef  = "type" name [Generics] "{" (Field | Variant | Method)* "}"
Generics = "[" GenericParam { "," GenericParam } "]"      // T | T: Bound | comptime N: Type
Type     = Name [ "[" Type {"," Type} "]" ]               // List[Int], Map[K,V]
         | "(" Type {"," Type} ")"                        // tuple
         | "[" Type ";" Expr "]"                          // fixed array
         | "&" Type | "Ptr" "[" Type "]"

Stmt     = Let | Assign | ExprStmt | Return | Break | Continue | While | For
Let      = ["mut"] Pattern ["=" Expr]                     // binding
Expr     = (Pratt expression, §4)
Pattern  = "_" | Literal | Name | Path "(" Pattern,* ")" | "(" Pattern,* ")"
         | Pattern "|" Pattern | Pattern "if" Expr        // in match
```
Every node carries a **span** (byte range) for diagnostics and debug info (feature
8). No type in the AST — types are attached by P2 (inference) to a parallel table.

---

## 4. Expression Parser (Pratt / precedence-climbing, `expr.rs`)

Expressions via **Pratt parsing**: each token kind has a `prefix` and/or
`infix` binding strength. One pass, no backtracking.

### 4.1 Precedence (low → high)
| Level | Operators | Assoc. |
|---|---|---|
| 1 | `or` | left |
| 2 | `and` | left |
| 3 | `== != < <= > >=` | none (chains forbidden) |
| 4 | `\| ^` | left |
| 5 | `&` (bit) `<< >>` | left |
| 6 | `+ - +% -%` | left |
| 7 | `* / % *%` | left |
| 8 | `not -` (unary, prefix) | — |
| 9 | postfix: `?` `.` `f(...)` `[...]` `as T` | left |
| 10 | primary: literal, ident, `(…)`, `{…}`, `if`, `match`, lambda, `comptime` | — |

`?` is postfix (error propagation). Comparisons are **not** chainable
(`a < b < c` = error) — this avoids bugs and grammar ambiguity.

### 4.2 `{` is **always** a block (decision §1.3)
No block-vs-map/set lookahead: `{` opens exclusively a block. Map/Set
live in `[]` (`[k: v]`, `Set[…]`, §1.3). This makes `{` unambiguous at *every*
position — no late aha moment.

### 4.3 List vs. Map Literal (`[…]`)
In expression position, `[` opens a literal; the contents decide (Swift model):
- `[]` → empty list, `[:]` → empty map.
- `[a, b, …]` (commas, no top-level `:`) → list.
- `[k: v, …]` (first element `expr : expr`) → map.
Lookahead: after the first expression, check for `:` (map) vs. `,`/`]` (list).

### 4.4 Lambda vs. Parenthesization
`x -> e` (ident directly followed by `->`) → lambda with one param. `(a, b) -> e`
(parenthesized param list followed by `->`) → lambda. `(e)` without a following `->` →
parenthesized expression. The decision is made at the `->` lookahead after the closing `)`.

### 4.5 Construction (no `Name { … }` struct literal)
Vire constructs with **parentheses**: `Point(1.0, 2.0)` / `Point(x: 3.0, y: 4.0)` —
not `Point { … }`. This entirely eliminates Rust's ambiguity "struct literal vs. block after
an `if` scrutinee": after `if`/`for`/`while`/`match`, `{` is always the body.

### 4.5 `comptime`/`@` Forms
`comptime <expr|block>`, `@typeinfo(T)`, `@field(x, name)`, `@derive(...)`,
`@if(cond) { … }` — as prefix forms in the primary parser; `@name` is a
compiler-intrinsic namespace (not a user ident).

---

## 5. Error Recovery

- **Panic mode with sync tokens:** on a parse error, skip to the next *StmtEnd*
  (`Newline`/`;`) or `}`, then continue parsing → **multiple errors per run**.
- **Expectation-based messages:** every `expect(tok)` knows the context →
  "expected `}` to close the block starting at line N" instead of "unexpected token".
- **Fix suggestions** where cheap: missing `{`, `=` instead of `==`, comparison chain →
  a concrete hint (matches the ergonomics requirement).
- **Balanced brackets** are pre-checked during lexing (an earlier, clearer error on
  imbalance).

---

## 6. Test Strategy

- **Roundtrip property:** `parse` → `fmt` (pretty printer) → `parse` yields the same
  AST. `vire fmt` is thereby also the parser fuzz harness.
- **Snapshot tests** for diagnostics (error text + span stable).
- **Corpus:** the programs in [examples/](examples/) must parse without errors
  once the parser is in place — they are the first acceptance suite.
- **Fuzzing** (later): random token streams must never panic (only diagnose).

---

## 7. Interface to the Subsequent Phases

The parser delivers a `Module` (AST + spans + parsed, not-yet-evaluated
`comptime`/macro nodes). After that:
1. **Name resolution** (P2) — binds idents to declarations (whole-program).
2. **Macro/`comptime` expansion** (P3) — *before* type-checking the expanded code.
3. **Inference + trait resolution** (P2) — annotates types on a side table.
4. **Lowering** (P4) → `crates/ir` in SSA (incl. iterator-mutation check §9a).

The parser itself stays **purely syntactic** (no type knowledge), so that it is fast,
testable, and reusable for `fmt`/LSP.

---

## 8. Effort (rough estimate)
Lexer ~1 week · Pratt expression parser + items ~2–3 weeks · error recovery +
diagnostics ~1 week · `fmt`/roundtrip tests ongoing. This is the "weeks" claim from
[EVALUATION.md](EVALUATION.md) §5 — the *honest* effort lies afterwards in P2/P3
(inference, traits, `comptime`), not in the parser.
