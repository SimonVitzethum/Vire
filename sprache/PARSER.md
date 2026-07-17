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

### 1.1 Das Namens-Gesetz — **drei** lexikalische Klassen (Grammatik, nicht Konvention)
Die Groß-/Kleinschreibung trägt einen großen Teil der Disambiguierung (Typ-
Applikation `List[Int]` vs. Indexing `xs[i]`, Produkt- vs. Summenvariante im
`type`-Body). „Großschreibung = Typ" **allein bricht aber an Konstanten**:
`const MAX = 1024` ist ein großgeschriebener *Wert*, `MAX[0]` wäre fälschlich
Typ-Applikation. Deshalb **drei** rein lexikalisch unterscheidbare Klassen:

| Klasse | Form | Bedeutung | Beispiel |
|---|---|---|---|
| `UpperCamel` | Großbuchstabe, danach **mind. ein Kleinbuchstabe** | **Typ / Konstruktor** | `List`, `Point`, `Circle` |
| `SCREAMING_SNAKE` | nur Großbuchstaben/Ziffern/`_` | **const-Wert** | `MAX`, `PI`, `CRC_TABLE` |
| `lower_snake` | beginnt klein | **Wert / Funktion / Variable** | `xs`, `parse`, `count` |

Damit ist `List[Int]` (UpperCamel `[`) Typargument, `MAX[0]`/`xs[i]`
(SCREAMING/lower `[`) Indexing — **ohne Namensauflösung** entscheidbar. Das ist ein
**verbindliches** Stil-Korsett (ein falsch benannter Bezeichner ist ein Fehler,
kein Lint) und Teil der Grammatik.

### 1.2 `[]` — vier Rollen, alle über Position + Namensklasse aufgelöst
`[]` ist Generics, Indexing, Listen-Literal und Fixarray `[T; N]`:
- Nach `UpperCamel`/Fn-Name in Typ-/Deklarationsposition → **Generics** (`List[Int]`, `fn max[T]`).
- Nach einem Wert-Ausdruck → **Indexing** (`xs[i]`).
- In Ausdrucksposition, öffnend → **Listen-/Map-Literal** (§1.3).
- `[T; N]` in Typposition → **Fixarray** (Semikolon trennt).
- **Keine Typargumente an Werten**: `wert[Typ]` ist *immer* Indexing. Explizite
  Fn-Typargumente nur über **Turbofish `#[T]`** (`collect#[List[Int]]()`) für die
  seltenen rückgabetyp-getriebenen Fälle; sonst Inferenz. (Rusts `::<>` existiert
  aus genau diesem Grund.)

### 1.3 `{}` = **nur Block.** Map/Set sind markiert (Entscheidung)
`{}` ist **ausschließlich** ein Block (Anweisungen; letzter Ausdruck = Wert). Die
Python-Zweideutigkeit `{}` = dict-oder-set-oder-block wird **nicht** übernommen —
sie lässt den Leser erst nach dem `{` wissen, was aufgemacht wird. Stattdessen
(Swift-Modell, `:` trennt eindeutig):
- **Liste:** `[a, b, c]`, leer `[]`.
- **Map:** `[k: v, k2: v2]`, leer `[:]`.
- **Set:** `Set[a, b]` (Konstruktor-Literal, `UpperCamel[` → eindeutig).
So ist `{` **immer** Block — kein Lookahead, kein später Aha-Effekt für den Leser.

### 1.4 Bindung vs. Zuweisung ohne `let` (Entscheidung)
Kein `let`: `x = 5` bindet, `x = 6` weist zu. Die Zweideutigkeit wird durch **eine
Regel** aufgelöst (in `resolve`, nicht im Parser): das **erste** `x =` in einem
Scope ist eine **Bindung** (unveränderlich, außer `mut x`); jedes **weitere** `x =`
im selben Scope ist eine **Zuweisung** und verlangt, dass `x` `mut` ist — sonst
**Fehler** (kein stilles Rebind). **Shadowing** existiert nur über *innere* Scopes.
Damit ist Absicht ausdrückbar und der Tippfehler gefangen. (Bewusster Tausch: ein
Keyword weniger als Rust, dafür die Scope-Regel als Ersatz.)

### 1.5 Sonstige harte Regeln
- **`{ }`-Blöcke sind Ausdrücke** (`if`/`match`/Block liefern Werte).
- **`->` an drei klaren Stellen**, per Position getrennt: Rückgabetyp
  (`fn f() -> T`), Lambda (`x -> e`), match-Arm (`Pat -> e`).
- **Keyword-geführte Items** (`fn type trait impl use const macro extern`) → ein
  Token Lookahead.
- **String-Interpolation** ist auf *jedem* String aktiv (`"{name}"`, kein `f`-
  Präfix). Literale Klammern werden **verdoppelt**: `{{` → `{`, `}}` → `}`.

### 1.6 Zwei Ausdrucks-Ambiguitäten (Detailregeln unten)
1. Lambda `x -> e` / `(a,b) -> e` **oder** geklammerter Ausdruck → §4.3.
2. Struktur-Konstruktion vs. Block nach `if`/`for`/`while`-Skrutinee → §4.4
   (Konstruktion ist ohnehin `Point(…)`, nicht `Point { … }` — kein Konflikt).

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

### 2.3 Newline-Handling — das **volle** Go-Fortsetzungsmodell
Einrückungs-*un*empfindlich, newline-*empfindlich* (Gos Modell, nicht „whitespace-
insensitiv"). Zwei Regeln, beide gebraucht — Chains sind real mehrzeilig:

**(a) Zeilenende — wann *kein* Terminator (Fortsetzung).** Ein `Newline` wird
**nicht** emittiert, wenn das *letzte signifikante Token der Zeile* eine Anweisung
nicht abschließen kann: binärer/unärer Operator, `,`, `.`, offene Klammer
`(` `[` `{`, `->`, `=`/`+=`/…. Emittiert wird nach: Ident, Literal, `)` `]` `}`, `?`,
`self`, `true`/`false`, `return`/`break`/`continue`.

**(b) Zeilenanfang — führendes Token unterdrückt den Terminator.** Auch wenn (a)
einen Terminator setzen würde: beginnt die *nächste* Zeile mit `.` (Method-Chain),
wird der Terminator **unterdrückt** (Fortsetzung). Umgekehrt sind führendes
`(`/`[`/`-` **kein** Fortsetzungssignal — sonst würde `g()\n(x)` zu `g()(x)` und
`a\n-b` zu `a-b` verschmelzen; sie starten eine neue Anweisung.
```vire
x = foo
    .bar()      // führendes `.` → Fortsetzung
    .baz()
y = g()
(x)             // führendes `(` → NEUE Anweisung (nicht g()(x))
```
Umsetzung: Regel (a) im Lexer (letztes Token merken); Regel (b) als Lookahead —
vor dem Emittieren eines Terminators das nächste signifikante Zeichen prüfen, bei
`.` unterdrücken. Der Parser behandelt `Newline`/`;` als *StmtEnd* und erlaubt in
der Postfix-Position ein `Newline` vor `.` (redundant abgesichert). *(Führendes `[`
ist der fieseste Fall — kollidiert mit Indexing; deshalb bewusst **kein**
Fortsetzungssignal: Zeilenanfang-`[` ist immer ein neues Listen-/Map-Literal-
Statement.)*

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

### 4.2 `{` ist **immer** Block (Entscheidung §1.3)
Kein Block-vs-Map/Set-Lookahead: `{` öffnet ausschließlich einen Block. Map/Set
stehen in `[]` (`[k: v]`, `Set[…]`, §1.3). Damit ist `{` an *jeder* Stelle
eindeutig — kein später Aha-Effekt.

### 4.3 Listen- vs. Map-Literal (`[…]`)
In Ausdrucksposition öffnet `[` ein Literal; der Inhalt entscheidet (Swift-Modell):
- `[]` → leere Liste, `[:]` → leere Map.
- `[a, b, …]` (Kommas, kein Top-`:`) → Liste.
- `[k: v, …]` (erstes Element `expr : expr`) → Map.
Lookahead: nach dem ersten Ausdruck auf `:` (Map) vs. `,`/`]` (Liste) prüfen.

### 4.4 Lambda vs. Klammerung
`x -> e` (Ident direkt gefolgt von `->`) → Lambda mit einem Param. `(a, b) -> e`
(Klammer-Param-Liste gefolgt von `->`) → Lambda. `(e)` ohne folgendes `->` →
geklammerter Ausdruck. Entscheidung beim `->`-Lookahead nach der schließenden `)`.

### 4.5 Konstruktion (kein `Name { … }`-Struct-Literal)
Vire konstruiert mit **Klammern**: `Point(1.0, 2.0)` / `Point(x: 3.0, y: 4.0)` —
nicht `Point { … }`. Damit entfällt Rusts Ambiguität „Struct-Literal vs. Block nach
`if`-Skrutinee" komplett: nach `if`/`for`/`while`/`match` ist `{` immer der Body.

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
