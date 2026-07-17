# Lume — Sprachdesign & Syntax (Skizze)

*Arbeitsname, provisorisch. Ziel: Ergonomie von Python, Leistung & Reichweite von
C/C++/Rust, Speichersicherheit ohne Annotationen, AOT über FastLLVMs Backend.
Begründung & Machbarkeit in [BEWERTUNG.md](BEWERTUNG.md).*

## Leitprinzipien

1. **Nicht schwerer als Python.** Keine Typannotationen nötig, keine Semikolons,
   keine Speicher-Verwaltung. Was man aus Python kann, kann man hier — nur mit
   `{ }`-Blöcken statt bedeutungstragender Einrückung (klar für Editoren, Tools,
   Einfügen; keine Einrück-Fallen).
2. **Statisch getypt durch Inferenz.** Kein Typ steht da, aber jeder ist bekannt
   (Hindley-Milner + lokale Bidirektionalität). Fehler zur Compilezeit.
3. **Speicher ist unsichtbar.** Kein `new`/`free`, keine Lifetimes, kein `&mut`.
   Der Solver entscheidet Stack/Heap/RC und beweist Sicherheit (Bounds, null,
   use-after-free). Man schreibt Logik, nicht Buchhaltung.
4. **Sicherheit per Konstruktion.** Kein `null` (→ `Option`), kein
   Uninitialisiert, Bounds geprüft (und wegoptimiert, wo beweisbar). `unsafe` nur
   opt-in an der C-Grenze.
5. **Ein Kern, drei Welten.** Werttypen + C-Layout + freestanding = C/Zig-Bereich.
   Traits + Generics + Summentypen + Pattern-Matching = Rust-Bereich. Inferenz +
   automatischer Speicher = Python/Go-Bereich.

Dateiendung `.lm`. Einstiegspunkt: die freie Funktion `main`. Anweisungen enden
am Zeilenende (Semikolon optional); Blöcke stehen in `{ }`. Der **letzte Ausdruck
eines Blocks ist sein Wert** (wie Rust) — `return` nur zum frühen Aussteigen.

---

## 1. Werte, Bindungen, Funktionen

```lume
x = 5                 // unveränderliche Bindung (wie `let`), Typ inferiert: Int
mut total = 0         // veränderlich, explizit
total = total + x     // ok, weil `mut`
name = "Welt"         // Str
pi = 3.14159          // Float (= F64)

fn add(a, b) = a + b              // Ausdrucksfunktion; a,b,Rückgabe inferiert
fn greet(name) {                  // Block; letzter Ausdruck ist der Wert
    print("Hallo, {name}")        // String-Interpolation mit { }
}

fn main() {
    print(add(2, 3))              // 5
    greet("Lume")
}
```

Unveränderlich per Vorgabe (ein `mut` mehr als Python, dafür sicherer und
optimierbarer). Rebinding ohne `mut` ist ein Compilezeit-Fehler.

## 2. Basistypen

| Kategorie | Typen |
|---|---|
| Ganzzahl | `Int` (=I64), `I8 I16 I32 I64`, `UInt` (=U64), `U8 U16 U32 U64`, `Byte` (=U8) |
| Gleitkomma | `Float` (=F64), `F32` |
| Sonst | `Bool`, `Str`, `Char`, `Unit` (leer, wie `()`), `Ptr[T]` (nur `unsafe`) |

Ganzzahlen sind fix breit und überlaufgeprüft (Debug) / wrapping (Release,
konfigurierbar). `Int` ist der ergonomische Standard, die exakten Breiten für
Systemcode und FFI.

## 3. Zusammengesetzte Typen — `type`

```lume
type Point {                      // Produkt (struct), Werttyp, kein Header
    x: Float
    y: Float

    fn dist(self) = sqrt(self.x*self.x + self.y*self.y)   // Methode
    fn scaled(self, k) = Point(self.x*k, self.y*k)
}

p = Point(1.0, 2.0)               // positional
q = Point(x: 3.0, y: 4.0)         // benannt
print(q.dist())                   // 5.0
```

Summentypen (algebraisch, ersetzen Enums **und** `null`):

```lume
type Shape {
    Circle(radius: Float)
    Rect(w: Float, h: Float)
    Point                          // Variante ohne Daten
}

fn area(s: Shape) -> Float {
    match s {
        Circle(r)  -> 3.14159 * r * r
        Rect(w, h) -> w * h
        Point      -> 0.0
    }
}
```

`Option` und `Result` sind gewöhnliche Summentypen der Stdlib, kein Spezialfall:

```lume
type Option[T] { Some(T)  None }
type Result[T, E] { Ok(T)  Err(E) }
```

## 4. Kein `null` — `Option` + `?`

```lume
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

// `?` entpackt Some/Ok oder springt früh raus (None/Err propagieren):
fn first_plus_one(xs: List[Int]) -> Option[Int] {
    head = xs.first()?               // bei None: sofort None zurück
    Some(head + 1)
}
```

## 5. Fehler — `Result` + `?`

```lume
fn read_config(path: Str) -> Result[Config, Error] {
    text  = read_file(path)?         // propagiert Err
    lines = text.split("\n")
    parse(lines)                     // Result als Rückgabe
}

fn main() {
    match read_config("app.cfg") {
        Ok(cfg) -> run(cfg)
        Err(e)  -> print("Fehler: {e}")
    }
}
```

Keine Exceptions, kein `try/catch` — Fehler sind Werte, `?` macht sie leicht.

## 6. Generik & Traits (Typklassen)

```lume
trait Ord {
    fn cmp(self, other: Self) -> Int
    fn less(self, other: Self) = self.cmp(other) < 0   // Default-Methode
}

fn max[T: Ord](a: T, b: T) -> T {
    if a.less(b) { b } else { a }
}

// Trait für eigenen Typ erfüllen:
impl Ord for Point {
    fn cmp(self, other) = compare(self.dist(), other.dist())
}

biggest = max(Point(1,1), Point(3,4))   // T = Point, monomorphisiert
```

Generics werden **monomorphisiert** (eine spezialisierte, geinlinete Variante pro
Typkombination) — zero-cost wie C++-Templates/Rust, ohne deren Syntaxlast.

## 7. Sammlungen & Iteration

```lume
xs = [1, 2, 3, 4]                 // List[Int]
m  = {"a": 1, "b": 2}             // Map[Str, Int]
s  = {1, 2, 3}                    // Set[Int]

for x in xs { print(x) }
for k, v in m { print("{k}={v}") }
for i in 0..10 { }                // Range 0..9
for i in 0..=10 { }               // inklusiv

// Funktional, aber ohne versteckte Kosten (Iteratoren werden geinlinet):
evens   = xs.filter(x -> x % 2 == 0)
doubled = xs.map(x -> x * 2)
sum     = xs.fold(0, (acc, x) -> acc + x)

// Comprehensions (Python-vertraut):
squares = [x*x for x in xs if x > 1]
```

Lambdas: `x -> ausdruck` (ein Argument), `(a, b) -> ausdruck` (mehrere).

## 8. Kontrollfluss

```lume
if x > 0 { print("pos") } elif x == 0 { print("null") } else { print("neg") }

// `if` ist ein Ausdruck:
label = if score >= 50 { "bestanden" } else { "durchgefallen" }

while running { tick() }

for x in xs {
    if x < 0 { break }
    if x == 0 { continue }
    process(x)
}
```

## 9. Speicher: unsichtbar, aber steuerbar

Standard: **nichts tun.** Der Solver entscheidet.

```lume
p = Point(1.0, 2.0)     // entkommt nicht → Stack, kein RC
node = Node(value: 5)   // in eine Liste gehängt → Heap + RC, automatisch
q = p                   // move/copy/share — inferiert, immer sicher, zero-cost wo möglich
```

- Kleine Werttypen: kopiert (wie `int`).
- Nicht-entkommende Objekte: Stack (`alloca`), null RC.
- Geteilte/zyklische Objekte: Heap + RC + Zyklen-Kollektor — automatisch.

Für heiße Pfade *optional* explizite Ausleihe (kein Muss, keine Lifetimes):

```lume
fn sum(xs: &List[Int]) -> Int {   // `&` = geborgt, keine RC-Berührung
    mut acc = 0
    for x in xs { acc = acc + x }
    acc
}
```

`&` ist eine *Optimierungs-Zusicherung*, kein Pflicht-Annotationssystem: weglassbar,
der Solver leitet Borrows ohnehin her (wie schon heute `this`/Parameter).

## 10. C-Interop (der universelle Klebstoff)

```lume
extern "C" {
    fn sqrt(x: F64) -> F64
    fn write(fd: I32, buf: Ptr[Byte], n: UInt) -> Int
}

// Ganze Header binden (Generator erzeugt die Signaturen):
use c "sqlite3.h" as sql

fn main() {
    unsafe {                         // nur die FFI-Zeile ist unsafe
        db = sql.sqlite3_open("app.db")
    }
}
```

C-ABI ist direkt und vollständig; C++/Rust-Bibliotheken mit C-Oberfläche genauso.
Reine C++-Templates / idiomatisches Rust: über generierte Bindings bzw. gar nicht
(s. [BEWERTUNG.md](BEWERTUNG.md) §1.C — dieselbe Grenze wie für jede Sprache).

## 11. Nebenläufigkeit (CSP, wie Go — leicht)

```lume
ch = Channel[Int]()

spawn {                              // leichter Thread
    for i in 0..100 { ch.send(i) }
}

mut total = 0
for x in ch.take(100) { total = total + x }
print(total)
```

Unter Threads werden Refcounts automatisch atomar (FastLLVM `--threads` heute
schon). Kein Data-Race auf sicheren Typen (geteilte Mutation nur über `Channel`
oder `Atomic[T]`/`Mutex[T]`).

## 12. Module & Sichtbarkeit

```lume
use std.io                           // Standardbibliothek
use math.{sin, cos}                  // selektiv

pub fn api_call() { }                // öffentlich (Teil der stabilen Grenze)
fn helper() { }                      // modul-privat
```

Öffentliche Funktionen an der Modulgrenze *dürfen* Typannotationen tragen (Doku +
Inferenz-Anker); innen bleibt alles inferiert.

## 13. Vollständiges Mini-Programm

```lume
// Wortfrequenz — zeigt Inferenz, Map, Iteration, Option, Fehler in ~10 Zeilen
use std.io

fn word_counts(text: Str) -> Map[Str, Int] {
    mut counts = {}                                  // Map[Str, Int] inferiert
    for word in text.lower().split_whitespace() {
        counts[word] = counts.get(word).or(0) + 1    // Option.or → Default
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

Liest wie Python, kompiliert zu einer nativen, speichersicheren, RC-eliminierten
Binary.

---

## Was bewusst *fehlt* (Einfachheit durch Weglassen)

- Keine Lifetimes, kein `&mut`/`&`-Zwang, kein Borrow-Checker im Weg (Solver
  inferiert).
- Kein Vererbungs-Baum (Traits/Komposition statt Klassen-Hierarchie).
- Keine Header/Deklarationen (Whole-Program, ein Durchlauf).
- Keine Makros/`unsafe` im Alltag (nur an der FFI-Grenze).
- Keine Runtime-Reflexion/`eval` (AOT, Closed World).
- Keine Null, keine Exceptions, keine impliziten Konversionen.

## Abbildung auf FastLLVM (warum es „einfach" absenkt)

| Lume-Konstrukt | FastLLVM-IR / Solver |
|---|---|
| `type` (Produkt) | Struct-Layout, Werttyp; Escape-Analyse → Stack/Heap |
| `type` (Summe) | getaggte Union; `match` → `switch` + Feldzugriff |
| Generics | Monomorphisierung vor der IR (wie heute Inlining) |
| Traits | statische Auflösung → Direkt-Calls (heute: Devirt) |
| `Option`/kein null | Null-Checks entfallen per Konstruktion (heute: Null-Elision) |
| Bounds | Bounds-Check-Elision (GVN, heute gebaut) |
| Speicher | RC + Escape + Azyklizität (heute gebaut) |
| `&`/Borrow | Borrow-Slot-/Parameter-RC-Elision (heute gebaut) |
| `extern "C"` | direkte LLVM-Deklaration + Call |
| SSA von Anfang an | **entfällt: kein GVN-Kampf gegen javac-Slot-Reuse** |

Der gesamte Backend- und Solver-Stack bleibt unverändert; neu ist nur Front-End +
Typinferenz + Absenkung. Siehe [beispiele/](beispiele/) für Programme über alle
Zielbereiche (Systemnah, funktional, nebenläufig, FFI, numerisch).
