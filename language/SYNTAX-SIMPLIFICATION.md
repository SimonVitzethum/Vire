# Making syntax lighter — exploration (without losing performance/power)

All simplifications here are **pure frontend sugar**: they only change lexer/
parser, produce the same IR → **zero runtime cost**, and are **additive** (no
existing capability is dropped). Criterion for "powerful enough": each must reduce to
the existing core (desugaring), not restrict it.

## Implemented
1. **Script style / implicit `main`.** Top-level statements are combined into `fn main()`
   — Python-like, no boilerplate:
   ```vire
   mut s = 0
   for i in 0..10 { s = s + i }
   print(s)          // no fn main() needed
   ```
   `fn main` AND top-level statements at the same time = error (unambiguity).
2. **Multi-argument `print`.** `print(a, b, "c")` prints each argument on its own
   line. No format string needed for the normal case.
3. **Trailing commas** in calls/lists (`f(a, b,)`) — diff-friendly.
4. **Expression functions** `fn quad(x) = x * x` (was already there, now confirmed).
5. **Line comments `//`** and nestable `/* */` (was already there).
6. **Newline-as-terminator** (Go style, no `;`), full continuation after operators.
7. **Full type inference for locals/parameters** — annotations optional (scalar).

## Analyzed, deliberately NOT (yet) implemented — with justification
- **String interpolation `"sum = {x}"`** — the largest remaining ergonomics gain.
  Needs lexer splitting into parts + `str_concat` + number→string at runtime
  (exists in the runtime string path). **Value high, effort medium** → earmarked as the next
  sugar step (the design reserves `{{` as escape).
- **Chained comparisons `0 < x < 10`** — Python-like; desugars to `0<x and x<10`.
  Small, but risk of collision with generic `[]`/comparison readings → only after
  interpolation.
- **Optional parentheses on single-argument calls** (`print x`) — DELIBERATELY NOT:
  creates grammar ambiguity (`f -x` = call or subtraction?), costs
  unambiguity without real gain. Power ≠ fewer parentheses.
- **Significant indentation** (Python blocks) — DELIBERATELY NOT: the user decided
  early for `{}` blocks; indentation brings known tooling/refactoring
  costs without a gain in expressiveness.

## Guideline
Sugar yes, as long as (a) it reduces to the core, (b) it does not make the grammar
ambiguous, (c) it has zero runtime cost. "Lighter" means less
boilerplate and more inference — not less precision.

## `->` → `>` for the return type (implemented) + analysis of the other `->` locations
Shorter return type: `fn add(a: Int, b: Int) > Int { a + b }`. The old `-> Int`
still applies (nothing breaks); `>` is the additional short form. Safe, because after the
parameter list `)` NO expression context follows in which `>` could be a comparison.

**Where `->` CANNOT become `>` (grammar would be ambiguous — guideline (b)):**
- **Lambda** `x -> x*2`: `x > x*2` IS a valid comparison → not distinguishable.
  (Proof: `mut f = 3 > 5` yields `false`, not a lambda.)
- **Match arm** `pat if guard -> body`: the guard is an expression and often itself
  a `>` comparison (`if x > 0`); a `>` as arm separator would be swallowed by the
  guard parser (`x > 0 > body`). For arms WITHOUT a guard `>` would be unambiguous, but the
  mixture would be inconsistent → keep it at `->`.
- **Range** `a..b`: `.` is already the field access → `a.b` would be ambiguous.

**Dead token:** `=>` (FatArrow) is defined in the lexer, but is used nowhere in the parser
(cleanup candidate, no semantics).

**Conclusion:** The return type is the ONLY `->` location at which the shortening to `>` is
ambiguity-free. All other shortenings (`->`, `..`) collide with
existing operators/contexts — therefore deliberately not done (guideline (b)).
