# Vire — Sprach- und Feature-Referenz

Präzise Beschreibung von Syntax und Semantik. Ergänzt die Tour in
[SPRACHE.md](SPRACHE.md) um Vollständigkeit und die acht Features aus
[FEATURES-BEWERTUNG.md](FEATURES-BEWERTUNG.md). Zielbild: **nicht schwerer als
Python, statisch inferiert, speichersicher, AOT über FastLLVM.**

Status: **Design-Spezifikation** (Front-End noch nicht implementiert; Backend/
Solver existieren). Beispiele zeigen die Zielsemantik.

---

## 1. Lexikalische Struktur

- **Kodierung:** UTF-8. Bezeichner: Unicode-Buchstaben + `_`, dann zusätzlich Ziffern.
- **Kommentare:** `// Zeile`, `/* Block */` (schachtelbar).
- **Anweisungsende:** Zeilenumbruch. Semikolon `;` optional (zum Trennen mehrerer
  Anweisungen in einer Zeile).
- **Blöcke:** `{ … }`. Der **letzte Ausdruck** eines Blocks ist sein Wert.
- **Literale:**
  - Ganzzahl: `42`, `0xFF`, `0b1010`, `0o17`, `1_000_000`, Suffix `42i32`, `7u8`.
  - Gleitkomma: `3.14`, `1e-9`, `6.022e23`, `2.0f32`.
  - Bool: `true`, `false`. Char: `'a'`, `'\n'`, `'\u{1F600}'`.
  - String: `"…"` mit Interpolation `{ausdruck}` und Format `{x:6}`, `{x:.2}`,
    `{x:x}` (hex). Roh: `r"C:\pfad"`. Mehrzeilig: `"""…"""`.
- **Schlüsselwörter:** `fn type trait impl mut const use pub extern unsafe
  match if elif else while for in break continue return spawn macro comptime
  and or not self Self as`.

## 2. Bindungen und Veränderlichkeit

```vire
x = 5              // unveränderlich (Default), Typ inferiert
mut y = 0          // veränderlich
y = y + 1          // ok
x = 6              // FEHLER: x ist unveränderlich
const MAX = 1024   // Compilezeit-Konstante (comptime-Wert)
```

Bindungen sind block-skopiert; Schatten (`x` in innerem Block neu binden) erlaubt.

## 3. Typen

### 3.1 Basistypen
| Kategorie | Typen |
|---|---|
| Ganzzahl (vorzeichenbehaftet) | `Int`(=`I64`), `I8 I16 I32 I64` |
| Ganzzahl (vorzeichenlos) | `UInt`(=`U64`), `U8 U16 U32 U64`, `Byte`(=`U8`) |
| Gleitkomma | `Float`(=`F64`), `F32` |
| Weitere | `Bool`, `Char`, `Str`, `Unit`(`()`), `Ptr[T]` (nur `unsafe`) |

Ganzzahl-Semantik: **overflow-geprüft per Default — auch in Release** (Panic bzw.
`Result` je Operator). Das ist bewusst *nicht* Rusts „Debug geprüft, Release
wrapping": ein in Debug korrektes Programm, das in Release still wrappt, ist genau
der Footgun, den eine sicherheits-orientierte Sprache nicht als Konfigurationsdetail
verstecken darf. Wer Wrapping *will*, sagt es explizit — über Wrap-Operatoren
(`a +% b`, `a *% b`, Zig-Stil) oder den Typ `Wrapping[T]`. `checked_add`/
`saturating_add` liefern `Option`/geklemmten Wert. Ungeprüftes Wrapping in Release
nur global abschaltbar (`--unchecked-arith`) — dokumentierte, bewusste Gefahr, nicht
Default. Keine impliziten numerischen Konversionen — explizit mit `as`.

### 3.2 Zusammengesetzte Typen — `type`
**Produkttyp** (struct, Werttyp, kein Objekt-Header):
```vire
type Point { x: Float, y: Float }
p = Point(1.0, 2.0)            // positional
q = Point(x: 3.0, y: 4.0)     // benannt; Feldreihenfolge egal
```
**Summentyp** (getaggte Union; ersetzt Enums und `null`):
```vire
type Shape {
    Circle(radius: Float)     // Variante mit benannten/positionalen Feldern
    Rect(w: Float, h: Float)
    Empty                     // datenlose Variante
}
```
**Methoden** stehen im `type`-Block; `self` ist der Empfänger:
```vire
type Vec2 {
    x: Float, y: Float
    fn len(self) = sqrt(self.x*self.x + self.y*self.y)
    fn add(self, o: Vec2) = Vec2(self.x + o.x, self.y + o.y)
}
```

### 3.3 Eingebaute Generische Typen
`List[T]` (`[1,2,3]`), `Map[K,V]` (`{"a":1}`), `Set[T]` (`{1,2}`),
`Option[T]` (`Some(x)`/`None`), `Result[T,E]` (`Ok`/`Err`), Tupel `(A, B)`,
Fixarray `[T; N]` (N comptime-Int, Stack-liegend).

## 4. Ausdrücke und Kontrollfluss

Alles ist Ausdruck, wo sinnvoll:
```vire
label = if s >= 50 { "ok" } else { "fail" }     // if-Ausdruck
sign  = match n { 0 -> "0", _ if n > 0 -> "+", _ -> "-" }   // match mit Guard
```
- `if c { } elif d { } else { }`
- `while c { }`, `for x in iter { }`, `for i, x in enumerate(xs) { }`
- `break`/`continue`, beide mit optionalem Label: `break :outer`
- `match` ist erschöpfend (nicht-erschöpfend = Compilefehler); Muster:
  Literale, Varianten `Circle(r)`, Tupel `(a, b)`, Bindung `x`, Wildcard `_`,
  Guards `_ if cond`, Oder-Muster `A | B`.

## 5. Funktionen und Closures

```vire
fn add(a, b) = a + b                      // Ausdrucksform
fn norm(v: Vec2) -> Float {               // Blockform, optionale Annotationen
    d = v.len()
    if d == 0.0 { 0.0 } else { d }
}
inc = x -> x + 1                          // Closure (ein Arg)
sum = (a, b) -> a + b                     // Closure (mehrere)
xs.map(x -> x * 2).filter(x -> x > 3)     // Closures als Argumente
```
Argumente sind unveränderlich (wie Bindungen); `mut`-Parameter für lokale
Mutation. Standardargumente: `fn open(path, mode = "r") { … }`. Benannte Argumente
am Aufruf: `open(path, mode: "w")`.

## 6. Generics und Traits (Typklassen) — *Punkt 2*

```vire
trait Ord {
    fn cmp(self, o: Self) -> Int
    fn less(self, o: Self) = self.cmp(o) < 0     // Default-Methode
}
impl Ord for Int { fn cmp(self, o) = self - o }

fn max[T: Ord](a: T, b: T) -> T { if a.less(b) { b } else { a } }
fn sort[T: Ord](xs: List[T]) -> List[T] { … }

// Mehrfach-Schranken:
fn dedup[T: Ord + Hash](xs: List[T]) -> List[T] { … }
```
Monomorphisierung: pro benutzter Typkombination eine spezialisierte, inline-fähige
Variante (zero-cost). Trait-Auflösung ist statisch → Direktaufrufe (heute im Solver
als Devirtualisierung vorhanden).

**Wert-Generics** (comptime-Parameter, wie C++-Nicht-Typ-Parameter):
```vire
type Matrix[T, comptime R: Int, comptime C: Int] {
    data: [T; R * C]
    fn get(self, r: Int, c: Int) -> T = self.data[r * C + c]
}
```

## 7. `comptime` und Compile-Time-Reflection — *Punkte 2, 3*

`comptime` markiert Code, der **im Compiler** ausgeführt wird. Kein separater
Makro-Dialekt — es ist dieselbe Sprache, nur zur Compilezeit.

```vire
const TABLE = comptime {                  // zur Compilezeit berechnet → Konstante
    mut t = [0; 256]
    for i in 0..256 { t[i] = crc_byte(i) }
    t
}

comptime if cfg.os == .linux { use_epoll() } else { use_kqueue() }   // bedingt
```

**Reflection** über `@typeinfo(T)` (comptime-durchlaufbar):
```vire
fn to_json[T](v: T) -> Str {
    info = @typeinfo(T)
    comptime match info.kind {
        .Struct -> {
            mut parts = []
            comptime for f in info.fields {
                parts.push("\"{f.name}\":" + to_json(v.@field(f.name)))
            }
            "{" + parts.join(",") + "}"
        }
        .Sum   -> …
        .Int   -> int_to_str(v)
    }
}

@derive(Json, Eq, Hash, Ord)              // Ableitung = comptime-generierte impls
type User { id: Int, name: Str }
```
Reflection ist rein statisch — **keine** Laufzeit-Metadaten, kein RTTI-Overhead.

## 8. Makros (hygienisch **und typsicher**) — *Punkt 4*

Kein C-Präprozessor. `macro` operiert auf dem AST — und ist an **jeder** Stelle
typsicher. Genau das trennt Vires „Präprozessor" vom C-Textersetzer, der typ-blind
ist. Die Garantien:

1. **Typisierte Parameter.** Ein Makro-Parameter hat eine Art (`expr`, `type`,
   `ident`, `pat`, `block`) *oder* einen konkreten Typ. Wird das Makro mit dem
   falschen Fragment aufgerufen, ist das ein **Compilefehler am Aufrufort** — nicht
   irgendwo tief in der Expansion.
2. **Typgeprüft nach Expansion.** Das *expandierte* Ergebnis durchläuft die volle
   Typprüfung wie normaler Code. Ein Makro kann **kein** ill-typisiertes oder
   ill-geformtes Programm erzeugen (anders als `#define`).
3. **Hygienisch.** Im Makro gebundene Namen (`t0`, `r` unten) kollidieren nie mit
   Namen am Aufrufort; referenzierte freie Namen binden am Definitionsort.
4. **Diagnosen am Aufrufort** mit Span bis in die Expansion (kein „Fehler in
   generiertem Code, Zeile ???"); volle Debug-Info (Feature 8).

```vire
// Parameter mit Art: cond ist ein Ausdruck, body ein Block.
macro unless(cond: expr, body: block) {
    if not (cond) { body }
}

macro timed(label: expr, body: block) {
    t0 = now()                                   // hygienisch: kollidiert nie mit
    r  = body                                    // Namen am Aufrufort
    log.debug("{label}", ms: now() - t0)
    r
}

unless(ready) { wait() }
x = timed("compute") { heavy() }                 // x erbt den geprüften Typ von heavy()

// Falsche Verwendung wird GEPRÜFT:
// unless(42) { … }        // FEHLER am Aufruf: `cond: expr` muss Bool sein
// timed("l", 5)           // FEHLER am Aufruf: `body: block` erwartet, `5` ist Ausdruck
```

Für 95 % der „Makro"-Fälle (Konstanten, bedingte Compilierung, Ableitungen,
Code-Erzeugung) nimmt man `const`/`comptime`/`@derive` (Punkt 7) — die ebenfalls
voll typgeprüft sind. Makros bleiben für echte **syntaktische** Abstraktion. In
*keinem* dieser Fälle gibt es untypisierte Token-Suppe wie beim C-Präprozessor.

## 9. Speichermodell

Unsichtbar per Default; der Whole-Program-Solver entscheidet und beweist:
- **Werttypen** (klein) → kopiert, im Register/Stack.
- **Nicht entkommende** Objekte → Stack (`alloca`), kein RC (Escape-Analyse).
- **Geteilte/entkommende** Objekte → Heap + RC; **zyklenfähige** Typen zusätzlich
  Zyklen-Kollektor (automatisch, ~2 KB; entfällt für azyklische Programme).
- **`&x`** (optional) = geborgte Referenz ohne RC-Berührung; Lifetimes werden
  *inferiert*, nicht geschrieben. Weglassbar.

Keine `new`/`free`, keine Lifetime-Syntax, kein `&mut`-Zwang. Details der
Machbarkeit: [BEWERTUNG.md](BEWERTUNG.md) §1.A.

## 9a. Mutation während Iteration (die Alias-Regel)

Das Problem: `for x in xs { xs.push(x) }` — RC hält das *Objekt*, aber der `push`
setzt den Backing-Puffer um, während der Iterator hineinzeigt. Iteration kann nicht
*zugleich* ein zero-cost Pointer-Walk sein **und** Mutation erlauben (siehe
[BEWERTUNG.md](BEWERTUNG.md) §7.2).

**Regel:** Der Compiler prüft *gezielt und lokal*, ob der Schleifenkörper die
iterierte Sammlung (oder einen lokalen Alias) mutiert:
- **beweisbar nicht-mutierend** → zero-cost Inline-Iteration (der Normalfall);
- **nicht beweisbar** → **Compilezeit-Fehler**. Keine stille langsame RC-Iteration —
  explizite Absicht ist verlangt:
  ```vire
  for x in xs.snapshot() { xs.push(x) }   // iteriere eine Kopie, mutiere das Original
  for i in 0..xs.len() { xs[i] = f(xs[i]) } // Index-Zugriff, bounds-geprüft
  ```

Dieser *eine-Sammlung-eine-Schleife*-Check ist weit einfacher als allgemeine
Alias-/Borrow-Analyse (er betrachtet eine Sammlung und lokale Aliase in *einer*
Funktion), aber er ist echte Analyse. Er ist dieselbe Frage wie „darf dieser Wert an
`spawn`" (§10) — nur lokaler.

## 10. Nebenläufigkeit — *Punkt 1*

```vire
ch = Channel[Int]()                       // typisierter Kanal
spawn { for i in 0..100 { ch.send(i) } }  // leichter Thread; Werte moven
for x in ch.take(100) { use(x) }

counter = Mutex(0)                         // geteilter mutabler Zustand nur gekapselt
spawn { counter.lock(|n| n + 1) }         // Zugriff nur im Lock-Closure
n = counter.get()

results = parallel_map(items, |x| heavy(x))   // Fork-Join-Datenparallelität
a = Atomic(0); a.fetch_add(1)                 // atomare Skalare
```
Regeln (vom Solver geprüft, **Compilezeit-Fehler** bei Verstoß): ein an `spawn`
übergebener Wert muss (a) gemoved/kopiert sein **oder** (b) `Channel`/`Mutex`/
`Atomic` sein. Refcounts sind unter Threads atomar. Garantie: kein Data Race auf
sicheren Typen im Kanal-/Mutex-Stil; **kein** Deadlock-Freiheits-Versprechen.

## 11. Fehlerbehandlung — *Punkt 7*

**Go-Geist, aber ohne `null`.** Fehler sind **Werte**, explizit im Rückgabetyp,
keine Exceptions, kein verstecktes Non-Local-Control-Flow — das ist Gos Kern. Aber
Vire benutzt **nicht** Gos `(T, error)`-Tupel mit `nil`: ein `nil`-Fehler wäre ein
`null` durch die Hintertür und verletzt Leitprinzip 4 (kein `null`). Stattdessen
**ein** konsistentes Modell: `Result[T, E]` (E typisiert, oft ein Summentyp oder das
`Error`-Interface). „Fehlbar" steht damit sichtbar in der Signatur (Go-Prinzip),
aber getypt und ohne null.
```vire
type ConfigError { NotFound(path: Str), BadSyntax(path: Str, line: Int) }

fn load(path: Str) -> Result[Config, ConfigError] {
    raw = read_file(path).wrap("Config {path}")?   // `?`: bei Err früh zurück + Kontext
    parse(raw)                                       // Result als Rückgabe
}

// Behandlung: explizit per match (das ist Gos „val, err"-Verzweigung, getypt) …
match load("app.cfg") {
    Ok(cfg)               -> run(cfg)
    Err(NotFound(p))      -> run(default_at(p))
    Err(e)                -> return Err(e)
}
```
- `?` propagiert `Err`/`None` (ersetzt Gos `if err != nil`-Kaskade), *ohne* die
  Explizitheit zu verlieren — die Signatur zeigt weiter die Fehlbarkeit.
- `.wrap(msg)` hängt Kontext an, behält die Kette; in Debug den Erzeugungs-Pfad
  (Punkt 8).
- **Kein `nil`, kein `(T, Error)`-Tupel** — historische Fassungen zeigten das; es
  ist entfernt, weil es `null` reintroduziert.
- `panic(msg)`/`assert(cond)` nur für **Programmierfehler**, nicht für erwartbare
  Fehler; brechen mit Crash-Pfad ab (Punkt 8).

## 12. Module, Sichtbarkeit, Pakete

```vire
use std.io                    // Standardbibliothek
use std.collections.{Map, Set}
use app.model as m            // Alias

pub fn api() { }              // öffentlich (stabile Grenze; Annotationen empfohlen)
fn helper() { }              // modul-privat (Default)
```
Ein Modul = eine Datei; ein Paket = ein Verzeichnis mit `mod.vr`. Kein Include,
keine Header, keine Reihenfolge-Abhängigkeit (Whole-Program, ein Durchlauf).

## 13. FFI / Interop

```vire
extern "C" {
    fn sqrt(x: F64) -> F64
    fn write(fd: I32, buf: Ptr[Byte], n: UInt) -> Int
}
use c "sqlite3.h" as sql       // Header-Binding-Generator
unsafe { db = sql.sqlite3_open("app.db") }   // FFI-Aufruf ist unsafe
```
C nativ/vollständig; C++/Rust über C-ABI (siehe [BEWERTUNG.md](BEWERTUNG.md) §1.C).
`unsafe`-Blöcke nur an der Grenze; darin `Ptr[T]`, `null_ptr()`, `x.addr()`.

## 14. Standardbibliothek: Logger — *Punkt 6*

```vire
log.info("bestellung", id: order.id, betrag: order.total)   // strukturiert
log.debug("cache", key: k, hit: found)                       // in Release entfernt

with log.span("http", method: "GET", path: p) {              // Kontext-Span
    log.info("start")                                         // erbt method/path
}
```
Level werden **zur Compilezeit** gefiltert (deaktivierte Aufrufe = 0 Instruktionen,
Argumente nicht ausgewertet). Sinks (Konsole/JSON/Datei) beim Build gewählt.
Quelle+Zeile automatisch aus der Debug-Info.

## 15. Build und Werkzeuge — *Punkt 5*

- Compiler-CLI (stabil): `vire build`, `vire run`, `--emit=obj|llvm|asm`,
  `-O0|-O2|-O3`, `--release`, `--debug`, `--target=…`, `--deps` (Ninja-`.d`).
- **Meson first-class:** Meson-Modul `vire` (`vire.executable/static_library`);
  Vire-Ziele linken mit C/C++/Rust-Zielen (gemeinsame C-ABI-Objekte). Empfehlung:
  Meson als primäres Build-System adoptieren (spart ein Subsystem).
- Formatierer `vire fmt`, Test-Runner `vire test`, LSP für Editoren.

## 16. Debug-Info und Crash-Pfade — *Punkt 8*

- **Debug-Profil** (`--debug`): DWARF-Metadaten (`!DILocation`) → gdb/lldb,
  Breakpoints, Quellzeilen wie bei C.
- **Crash-Pfad**: `panic`, unbehandelter `Error`, Bounds/Null in Debug drucken
  einen Stacktrace `datei:zeile:funktion`:
  ```
  panic: index 7 out of bounds for length 5
    at matrix.vr:42:14  in Matrix.get
    at main.vr:12:5     in main
  ```
- **Release**: standardmäßig aus (0 Overhead), optional `--release --backtrace`.
- **freestanding**: kompakte Symboltabelle statt libc-`backtrace`.

---

## Anhang A — Abbildung Vire → FastLLVM

| Vire | FastLLVM-Mechanismus (Status) |
|---|---|
| Werttyp/Struct | Struct-Layout + Escape-Analyse (✅) |
| Summentyp + `match` | getaggte Union → `switch` (✅ Muster im Backend) |
| Generics/Wert-Generics | Monomorphisierung vor der IR (✅ als Inliner) |
| Traits | statische Auflösung → Direktaufruf (✅ Devirt) |
| kein `null`, Bounds | Null-/Bounds-Check-Elision (✅ GVN) |
| Speicher/`&` | RC + Escape + Borrow-Slot-Elision + Azyklizität (✅) |
| Threads/`Atomic`/`Mutex` | atomare RC + pthreads + Monitor (✅ `--threads`) |
| `comptime`/Reflection | **neu:** Front-End-Auswerter über den Typgraphen |
| Makros | **neu:** AST-Transformation im Front-End |
| Fehler + `?` | pending-/Wertmodell (✅ Backend), `?` als Absenkung |
| Debug/Backtrace | LLVM-Debug-Metadaten (⚙️ Backend-Ausbau) |
| FFI `extern "C"` | direkte LLVM-Deklaration (✅) |

Neu zu bauen: **Front-End** (Lexer, Parser, Hindley-Milner-Inferenz,
`comptime`-Auswerter, Makro-Expander) und die Absenkung nach `crates/ir` **in
SSA**. Solver + Backend bleiben.

## Anhang B — Beispielübersicht

Siehe [beispiele/](beispiele/): `sieb`, `formen` (Traits/Generics), `baum`
(rekursive Generics), `wortzahl` (Maps/Iteratoren), `nebenlaeufig` (Threads/Kanäle),
`ffi`, sowie die Feature-Demos `reflektion`, `makros`, `fehler`, `logger`,
`comptime_matrix`.
