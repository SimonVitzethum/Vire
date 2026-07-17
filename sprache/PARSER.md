# Vire — Parser- und Front-End-Plan

Konkreter Bauplan für Lexer → Parser → AST (Pipeline-Phase P1 in
[../TODO.md](../TODO.md)). Ziel: ein **handgeschriebener** Parser (rekursiver
Abstieg + Pratt für Ausdrücke) — vorhersagbar, gute Fehlermeldungen, kein
Parser-Generator. Crate: `crates/vire_frontend` (Lexer+Parser+AST).

Warum handgeschrieben: Die Ergonomie („so einfach wie Python") hängt zur Hälfte an
**Fehlermeldungen** ([BEWERTUNG.md](BEWERTUNG.md) §5). Ein LR-Generator liefert
generische „unexpected token"-Fehler; ein Rekursiv-Abstieg-Parser kann an jeder
Stelle einen *erklärenden* Fehler + Fix-Vorschlag geben.

---

## 1. Design-Vorentscheidungen (die Grammatik einfach halten)

Vire ist bewusst so entworfen, dass der Parser **kaum Rückverfolgung** braucht:

- **Generics mit `[]`, nicht `<>`.** `List[Int]`, `max[T](…)`. Damit entfällt die
  berüchtigte C++/Rust-Mehrdeutigkeit `a < b > c`. `[` nach einem Typ-/Fn-Namen =
  Generics; `[` nach einem Ausdruck = Indexierung/Literal — per Kontext eindeutig.
- **Blöcke immer `{ }`, Ausdruck-orientiert.** Der letzte Ausdruck eines Blocks ist
  sein Wert. `if`/`match`/`{}` sind Ausdrücke.
- **Zeilenumbruch trennt Anweisungen** (Semikolon optional). Der Lexer erzeugt
  „weiche" Statement-Terminatoren (s. §2.3), sodass der Parser nicht raten muss.
- **Keyword-geführte Items.** Jede Top-Level-/Block-Deklaration beginnt mit einem
  Schlüsselwort (`fn type trait impl use const macro extern`) → der Parser
  verzweigt mit einem Token Lookahead, nie mehr.
- **`->` nur an zwei klaren Stellen**: Rückgabetyp (`fn f() -> T`) und Lambda/
  match-Arm (`x -> e`, `Pat -> e`). Kontext (nach `)`/Param-Liste vs. in `match`/
  Ausdrucksposition) trennt sie.

Mehrdeutigkeiten, die *explizit* aufgelöst werden (Regeln unten):
1. `{` = Block **oder** Map/Set-Literal → §4.2.
2. Lambda `x -> e` **oder** geklammerter Ausdruck → §4.3.
3. Struktur-Literal `Point { … }` **oder** Block nach Bedingung → §4.4.

---

## 2. Lexer (`lexer.rs`)

### 2.1 Token-Kinds
```
// Literale
Int(i128) Float(f64) Str(Vec<StrPart>) Char(char) True False
// Bezeichner & Keywords
Ident(sym)  Keyword(kw)
// Klammern/Trenner
( ) [ ] { }  , :  ;  ->  =>  .  ..  ..=  @
// Operatoren (Pratt-relevant)
+ - * / %  +% -% *%      // wrap-Varianten
== != < <= > >=
and or not  &  |  ^  <<  >>
= += -= *= /=            // Zuweisung (nur als Statement)
?                        // Fehler-Propagation (postfix)
// gesteuert
Newline  Eof
```

### 2.2 Lexikalisches
- **Idents:** Unicode-XID; Keywords sind reservierte Idents (Tabelle).
- **Zahlen:** `42 0xFF 0b1010 0o17 1_000 3.14 1e-9 42i32 7u8 2.0f32`.
- **Strings mit Interpolation:** `"a{expr}b"` wird zu `Str([Lit("a"), Expr(tokens),
  Lit("b")])`. Der Lexer balanciert `{}` innerhalb des Strings und lext den inneren
  Ausdruck rekursiv. Format-Spez: `{x:6}`/`{x:.2}` → `Expr` + `FormatSpec`.
  Roh: `r"…"`; mehrzeilig: `"""…"""`.
- **Kommentare:** `//` bis Zeilenende; `/* … */` **schachtelbar** (Tiefe zählen).

### 2.3 Newline-Handling (der einzige Trick)
Python-leicht ohne Einrück-Regeln: **ein `Newline`-Token wird nur dort emittiert, wo
es eine Anweisung beenden kann.** Regel (wie Go): Newline zählt als Terminator, wenn
das *letzte signifikante Token der Zeile* einen Ausdruck/eine Anweisung abschließen
kann — also nach Ident, Literal, `)` `]` `}`, `?`, `return`/`break`/`continue`.
Nicht nach binärem Operator, `,`, `.`, `(` `[` `{`, `->`, `=`. So darf man Ketten
umbrechen:
```vire
xs.map(x -> x*2)      // kein Terminator nach `.map(` …
  .filter(x -> x>3)   // … Fortsetzung erlaubt
```
Der Parser behandelt `Newline` und `;` gleich als *StmtEnd* und ignoriert
überzählige.

---

## 3. AST (`ast.rs`)

```
Module   = { Item }
Item     = FnDef | TypeDef | TraitDef | ImplDef | Use | ConstDef | MacroDef | Extern
FnDef    = "fn" name [Generics] "(" Params ")" ["->" Type] (Block | "=" Expr)
TypeDef  = "type" name [Generics] "{" (Field | Variant | Method)* "}"
Generics = "[" GenericParam { "," GenericParam } "]"      // T | T: Bound | comptime N: Type
Type     = Name [ "[" Type {"," Type} "]" ]               // List[Int], Map[K,V]
         | "(" Type {"," Type} ")"                        // Tupel
         | "[" Type ";" Expr "]"                          // Fixarray
         | "&" Type | "Ptr" "[" Type "]"

Stmt     = Let | Assign | ExprStmt | Return | Break | Continue | While | For
Let      = ["mut"] Pattern ["=" Expr]                     // Bindung
Expr     = (Pratt-Ausdruck, §4)
Pattern  = "_" | Literal | Name | Path "(" Pattern,* ")" | "(" Pattern,* ")"
         | Pattern "|" Pattern | Pattern "if" Expr        // in match
```
Jeder Knoten trägt eine **Span** (Byte-Range) für Diagnosen und Debug-Info (Feature
8). Kein Typ im AST — Typen setzt P2 (Inferenz) an einen parallelen Table an.

---

## 4. Ausdrucks-Parser (Pratt / Precedence-Climbing, `expr.rs`)

Ausdrücke über **Pratt-Parsing**: jede Token-Art hat eine `prefix`- und/oder
`infix`-Bindungsstärke. Ein Durchlauf, keine Rückverfolgung.

### 4.1 Präzedenz (niedrig → hoch)
| Stufe | Operatoren | Assoz. |
|---|---|---|
| 1 | `or` | links |
| 2 | `and` | links |
| 3 | `== != < <= > >=` | keine (Ketten verboten) |
| 4 | `\| ^` | links |
| 5 | `&` (bit) `<< >>` | links |
| 6 | `+ - +% -%` | links |
| 7 | `* / % *%` | links |
| 8 | `not -` (unär, prefix) | — |
| 9 | postfix: `?` `.` `f(...)` `[...]` `as T` | links |
| 10 | primär: Literal, Ident, `(…)`, `{…}`, `if`, `match`, Lambda, `comptime` | — |

`?` ist postfix (Fehler-Propagation). Vergleiche sind **nicht** verkettbar
(`a < b < c` = Fehler) — vermeidet Bugs und Grammatik-Ambiguität.

### 4.2 `{` — Block oder Map/Set?
Nach einem Token, das einen **Wert erwartet** (Ausdrucksposition), entscheidet der
erste Inhalt:
- `{}` → leere Map. `{ a: b, … }` (Ident/Expr **gefolgt von `:`**) → Map.
  `{ x, y, … }` (Kommas, kein `:`) → Set.
- sonst (Anweisung/Deklaration als erstes) → **Block**.
Ein Lookahead von 2 (`{` Ident `:`) reicht. In *Statement*-Position ist `{` immer
Block.

### 4.3 Lambda vs. Klammerung
`x -> e` (Ident direkt gefolgt von `->`) → Lambda mit einem Param. `(a, b) -> e`
(Klammer-Param-Liste gefolgt von `->`) → Lambda. `(e)` ohne folgendes `->` →
geklammerter Ausdruck. Entscheidung beim `->`-Lookahead nach der schließenden `)`.

### 4.4 Struktur-Literal vs. Block nach `if`/`for`/`while`
`Name { … }` ist ein Struktur-Literal **nur in reiner Ausdrucksposition**. Direkt
nach `if`/`while`/`for`/`match`-Skrutinee ist `{` **immer** der Body-Block (wie
Rust). Wer dort ein Struct-Literal braucht, klammert: `if (P { x: 1 }).ok { … }`.

### 4.5 `comptime`/`@`-Formen
`comptime <expr|block>`, `@typeinfo(T)`, `@field(x, name)`, `@derive(...)`,
`@if(cond) { … }` — als Prefix-Formen im primären Parser; `@name` ist ein
Compiler-Intrinsic-Namespace (kein User-Ident).

---

## 5. Fehler-Recovery

- **Panic-Mode mit Sync-Tokens:** bei Parse-Fehler bis zum nächsten *StmtEnd*
  (`Newline`/`;`) oder `}` skippen, dann weiterparsen → **mehrere Fehler pro Lauf**.
- **Erwartungs-basierte Meldungen:** jeder `expect(tok)` kennt den Kontext →
  „erwartete `}` zum Schließen des Blocks ab Zeile N" statt „unexpected token".
- **Fix-Vorschläge** wo billig: fehlendes `{`, `=` statt `==`, Vergleichs-Kette →
  konkreter Hinweis (deckt sich mit der Ergonomie-Anforderung).
- **Balancierte Klammern** werden beim Lexen vorgeprüft (früher, klarer Fehler bei
  Ungleichgewicht).

---

## 6. Teststrategie

- **Roundtrip-Property:** `parse` → `fmt` (Pretty-Printer) → `parse` ergibt denselben
  AST. `vire fmt` ist damit zugleich der Parser-Fuzz-Harness.
- **Snapshot-Tests** für Diagnosen (Fehlertext + Span stabil).
- **Korpus:** die Programme in [beispiele/](beispiele/) müssen fehlerfrei parsen,
  sobald der Parser steht — sie sind die erste Akzeptanz-Suite.
- **Fuzzing** (später): zufällige Token-Ströme dürfen nie panicen (nur Diagnose).

---

## 7. Schnittstelle zu den Folgephasen

Parser liefert `Module` (AST + Spans + geparste, noch nicht ausgewertete
`comptime`/Makro-Knoten). Danach:
1. **Namensauflösung** (P2) — bindet Idents an Deklarationen (Whole-Program).
2. **Makro-/`comptime`-Expansion** (P3) — *vor* der Typprüfung des expandierten Codes.
3. **Inferenz + Trait-Auflösung** (P2) — annotiert Typen an einer Seitentabelle.
4. **Lowering** (P4) → `crates/ir` in SSA (inkl. Iterator-Mutation-Check §9a).

Der Parser selbst bleibt **rein syntaktisch** (kein Typwissen), damit er schnell,
testbar und für `fmt`/LSP wiederverwendbar ist.

---

## 8. Aufwand (grobe Schätzung)
Lexer ~1 Woche · Pratt-Ausdrucksparser + Items ~2–3 Wochen · Fehler-Recovery +
Diagnosen ~1 Woche · `fmt`/Roundtrip-Tests laufend. Das ist die „Wochen"-Aussage aus
[BEWERTUNG.md](BEWERTUNG.md) §5 — der *ehrliche* Aufwand steckt danach in P2/P3
(Inferenz, Traits, `comptime`), nicht im Parser.
