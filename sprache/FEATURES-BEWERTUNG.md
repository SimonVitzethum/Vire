# Vire — Bewertung der acht gewünschten Features

Ehrliche Einordnung jeder Anforderung: Passt sie zur Philosophie (Python-leicht,
sicher, AOT, ~runtime-frei, Closed World)? Wie sähe das Design aus? Was kostet es,
wo sind die Grenzen? Reihenfolge = deine Liste. Kurzurteil je Punkt zuerst, dann
Begründung.

Legende Machbarkeit: 🟢 klar machbar & passt · 🟡 machbar mit Zuschnitt · 🔴 im
Wortlaut problematisch, besserer Ersatz empfohlen.

---

## 1. Multithreading, sehr einfach nutzbar, mit Race-Condition-Sicherheit 🟡

**Urteil:** Die *Ergonomie* ist leicht und die Runtime steht schon (FastLLVM
`--threads`: atomare Refcounts, pthreads, globaler Monitor). **Voll garantierte**
Data-Race-Freiheit *ohne* Ownership-Annotationen ist aber genau der Punkt, der in
keiner Sprache gratis ist — hier braucht es einen ehrlichen Zuschnitt.

**Warum die Spannung real ist.** Rust garantiert Race-Freiheit über `Send`/`Sync`
+ Borrow-Checker — also über *genau die Annotationen, die Vire vermeiden will*. Go
ist ergonomisch, garantiert aber **nichts** (Data Races sind möglich, nur ein
Detektor zur Laufzeit). Vire will Gos Leichtigkeit **und** mehr Sicherheit als Go,
ohne Rusts Last. Das ist erreichbar, aber nicht als Totalgarantie im ersten Wurf.

**Design — Sicherheit durch Konstruktion, nicht durch Annotation:**

```vire
// 1. Standardweg: teilen NUR über Kanäle (CSP). Werte, die durch einen Kanal
//    gehen, wechseln den Besitzer (move) — kein gemeinsamer mutabler Zustand.
ch = Channel[Task]()
spawn { for t in tasks { ch.send(t) } }          // Producer
for t in ch { handle(t) }                         // Consumer

// 2. Geteilter mutabler Zustand nur explizit gekapselt — der Typ erzwingt den Lock:
counter = Mutex(0)
spawn { counter.lock(|n| n + 1) }                 // Zugriff nur im Lock-Closure
total = counter.get()

// 3. Fork-Join für Datenparallelität, ohne Kanäle:
results = parallel_map(items, |x| heavy(x))       // Bibliotheks-Primitive
```

**Was Vire garantieren kann (und wie):**
- **Kein Data Race auf gemeinsamen mutablen Werttypen**, wenn Teilen *nur* über
  `Channel`/`Mutex`/`Atomic` erlaubt ist. Der Solver hat Whole-Program-Sicht und
  kann prüfen, dass ein an `spawn` übergebener Wert entweder (a) kopiert/gemoved
  wird oder (b) einen dieser Sync-Typen trägt — sonst **Compilezeit-Fehler**. Das
  ist eine *leichte* Send/Sync-Inferenz (kein Lifetime-System): „darf dieser Wert
  eine Thread-Grenze überqueren?" ist ja/nein, nicht ein Annotationskalkül.
- **Sichere RC unter Threads** (atomare Refcounts) — steht schon.
- **Deadlock-Freiheit: nein** (das garantiert auch Rust nicht).

**Ehrliche Grenze:** Die Send/Sync-Inferenz ohne Annotationen ist an Modul- und
FFI-Grenzen konservativ (dort ggf. eine `@shared`/`@threadsafe`-Markierung —
optional, wie bei den öffentlichen Typannotationen). Voll-automatische
Race-Freiheit für *beliebige* Alias-Graphen ist offen; für den Kanal-/Mutex-Stil
(99 % des realen nebenläufigen Codes) ist sie machbar. **Empfehlung: „safe by
construction" bewerben, nicht „race-free für alles".**

---

## 2. Template-Programmierung 🟢 (als Generics + Traits + `comptime`)

**Urteil:** Machbar und stärker als C++-Templates — ohne deren Syntaxlast und
Fehlermeldungs-Hölle. Der Monomorphisierer steht schon (heute als Inlining-Pass).

Vire trennt zwei Dinge, die C++ in „Templates" vermischt:
- **Parametrischer Polymorphismus** → Generics mit Trait-Schranken (§6 in
  [SPRACHE.md](SPRACHE.md)). Monomorphisiert = zero-cost, aber mit *geprüften*
  Schranken (`[T: Ord]`), also klare Fehler statt seitenlanger Template-Spew.
- **Wert-/Typ-Metaprogrammierung** → `comptime` (siehe Punkt 3), das zur
  Compilezeit über Typen rechnet und Code erzeugt. Das deckt die „Template-
  Metaprogramming"-Fälle (Typlisten, bedingte Instanziierung, `if constexpr`) ab —
  aber als *normaler Code, der zur Compilezeit läuft*, nicht als eigene
  Template-Sprache.

```vire
// Generisch mit Schranke — geprüft, monomorphisiert:
fn max[T: Ord](a: T, b: T) -> T { if a.less(b) { b } else { a } }

// Wert-Generics (wie C++ Nicht-Typ-Parameter), für feste Größen:
type Matrix[T, comptime R: Int, comptime C: Int] {
    data: [T; R * C]                  // Größe zur Compilezeit bekannt → Stack
    fn get(self, r: Int, c: Int) -> T = self.data[r * C + c]
}
m = Matrix[Float, 3, 3]()
```

**Grenze:** Kein Turing-vollständiges Instanziierungs-Chaos wie C++ (bewusst).
Rekursive `comptime`-Rechnung ja, aber mit Rekursionslimit und klarer Diagnose.

---

## 3. Compile-Time-Reflection 🟢 (starker Fit)

**Urteil:** Der natürlichste Fit von allen. Closed-World-AOT heißt: der Compiler
*hat den ganzen Typ-/Programm-Graphen*. Reflection zur Compilezeit ist damit
mächtig **und** zero-cost (kein Laufzeit-Metadaten-Ballast — anders als Java/C#).

```vire
// @typeinfo(T) liefert eine comptime-durchlaufbare Beschreibung des Typs.
fn to_json[T](value: T) -> Str {
    comptime for field in @typeinfo(T).fields {      // Schleife läuft im Compiler
        // generiert pro Feld Serialisierungscode — nichts davon zur Laufzeit
        emit("\"{field.name}\": {to_json(value.@field(field.name))}")
    }
}

// Ableitungen ohne Makro-Magie: @derive nutzt Reflection.
type User { name: Str, age: Int }
@derive(Json, Eq, Hash)                    // generiert Methoden via @typeinfo
```

**Fähigkeiten:** Felder/Varianten/Methoden eines Typs aufzählen, Attribute lesen,
Typen vergleichen, zur Compilezeit Code erzeugen (`emit`), statische Assertions
(`comptime assert`). Das ersetzt zugleich **Ableitungs-Makros** (`@derive`),
**Serialisierung**, **ORM-Mapping**, **Schema-Generierung** — alles ohne Runtime.

**Bewusste Grenze:** **Keine** Laufzeit-Reflection (`getClass().getFields()` zur
Runtime). Das wäre gegen „AOT/kein Runtime-Ballast". Wer echtes dynamisches
Verhalten braucht, generiert es zur Compilezeit oder nutzt einen Summentyp.

---

## 4. Präprozessor-Makros 🔴 → besser: hygienische Makros + `comptime`

**Urteil (ehrlich, gegen den Wortlaut):** Einen **C-artigen Text-Präprozessor
(`#define`, `#ifdef`, Token-Einfügung) sollte Vire *nicht* haben.** Er ist die
Quelle halber Sprach-Katastrophen: unhygienisch (fängt Namen ein), typ-blind,
debugger-feindlich, bricht Werkzeuge. Fast alles, wofür man historisch den
Präprozessor nutzt, löst Vire sauberer:

| Präprozessor-Zweck | Vire-Ersatz |
|---|---|
| Konstanten (`#define N 10`) | `const N = 10` (getypt) |
| Bedingte Compilierung (`#ifdef`) | `@if(cfg.debug) { … }` / `comptime if` |
| Plattform-Weichen | `@when(os == .linux)` (comptime) |
| Code-Erzeugung/DRY (`X-Macros`) | `comptime`-Reflection + `emit` (Punkt 3) |
| Include-Guards | Modulsystem (kein Include) |
| Inline-Funktionen | normale Funktionen (Inliner entscheidet) |

Wo echte **syntaktische Abstraktion** nötig ist (eigene Kontrollkonstrukte, DSL-
artige Blöcke), bietet Vire **hygienische Makros** (Rust-/Scheme-Stil): sie
operieren auf dem AST, fangen keine Namen ein, sind typgeprüft nach Expansion:

```vire
macro unless(cond, body) { if not (cond) { body } }   // hygienisch, AST-basiert
unless(done) { retry() }

macro timed(label, body) {                            // misst einen Block
    t0 = now(); body; log.debug("{label}: {now() - t0}")
}
```

**Verdict:** „Präprozessor-Makros" als *Fähigkeit* (Metaprogrammierung, bedingte
Compilierung) — **ja, voll**. Als *C-Präprozessor-Mechanik* — **nein**, das wäre
ein Rückschritt hinter Rust/Zig. Das ist eine bewusste Design-Empfehlung, kein
Weglassen von Funktionalität.

---

## 5. Build-System-Interop, first-class Meson 🟢🟡

**Urteil:** Machbar und strategisch klug (passt zum C-Interop-Kern). „First-class
Meson" heißt konkret zwei Dinge, das erste leicht, das zweite etwas Arbeit:

1. **Meson kann Vire-Quellen nativ bauen.** Meson unterstützt Sprachen über ein
   Compiler-Interface (wie für C/C++/Rust/D). Vire liefert dazu:
   - einen Compiler mit **stabilen CLI-Flags** (`--emit=obj|llvm|ir`, `-c`, `-o`,
     `-I`, Abhängigkeits-Ausgabe `--deps` im Ninja/`.d`-Format),
   - ein **Meson-Modul** `vire` (`vire.executable(...)`, `vire.static_library(...)`),
   - saubere **C-ABI-Ausgabe** (`.o`/`.a`), sodass Vire-Ziele mit C/C++/Rust-Zielen
     im selben Meson-Projekt gelinkt werden.
   Das ist geradlinig, weil Vire ohnehin über clang zu Objektdateien geht.
2. **Vire-Projekte konsumieren Meson/pkg-config-Abhängigkeiten first-class:**
   ```vire
   // build.vr — deklarativer Build, liest pkg-config/Meson-Deps
   project("app", deps: ["sqlite3", "openssl"])   // via pkg-config aufgelöst
   exe("app", src: ["main.vr"], link: deps)
   ```
   Der C-Header-Binding-Generator (Punkt aus BEWERTUNG.md §1) zieht die Header der
   Meson-Deps automatisch.

**Grenze/Empfehlung:** Ein *eigenes* Build-System (wie Cargo) und *first-class
Meson* zugleich splittet Aufwand. Empfehlung: **Meson als primäres Build-System
adoptieren** (nicht nur „interop") — spart ein ganzes Subsystem und dockt sofort an
die C/C++-Welt an, die Vire sowieso als Zielgruppe hat. Ein dünner `vire`-Wrapper
für den Einstieg (`vire build`) delegiert an Meson.

---

## 6. Logger — aber in gut 🟢

**Urteil:** Klar machbar und ein echter Hebel, weil AOT + comptime einen Logger
erlauben, der **zur Compilezeit weggeschaltet** wird (deaktivierte Level = **null**
Instruktionen, nicht nur ein Laufzeit-`if`).

„In gut" heißt konkret:
- **Strukturierte Felder**, nicht String-Kleben: `log.info("login", user: id, ms: t)`.
- **Compile-Zeit-Level-Filter:** unter dem gebauten Mindestlevel wird der Aufruf
  **wegoptimiert** (comptime-`if` um jeden Aufruf), inkl. der Argument-Auswertung.
  → In Release mit `level=warn` kostet ein `log.debug(...)` exakt 0.
- **Lazy-Argumente:** teure Felder nur ausgewertet, wenn das Level aktiv ist.
- **Kontext/Spans:** `with log.span("request", id: rid) { … }` hängt Felder an alle
  Logs im Block (strukturiert, keine globalen Statics).
- **Pluggable Sinks:** Konsole (farbig, human), JSON (maschinell), Datei, syslog —
  ausgewählt beim Build, kein Reflection-Overhead.
- **Quelle+Ort** automatisch via Debug-Info (Punkt 8): jede Zeile trägt
  `datei:zeile` ohne Handarbeit.

```vire
log.info("bestellung", id: order.id, betrag: order.total)
log.debug("cache", key: k, hit: found)     // in Release (warn) komplett entfernt

with log.span("http", method: "GET", path: p) {
    log.info("start")                        // erbt method/path automatisch
    handle()
}
```

Umsetzung: reine Stdlib über `comptime` + strukturierte Sink-Traits. Kein
Sprachfeature nötig — aber als *Batterie inklusive* mitgeliefert.

---

## 7. Fehlerbehandlung inspiriert von Go 🟡 (Go-Philosophie, entschärfte Verbosität)

**Urteil:** Passt zur Philosophie (Fehler sind Werte, **keine** Exceptions, kein
verstecktes Non-Local-Control-Flow) — das war schon der Plan. „Von Go inspiriert"
heißt: **explizit** und **als Wert**, aber Vire behebt Gos zwei bekannte Schwächen
(Verbosität und fehlende Stacktraces).

**Go-Kern übernommen:**
- Fehler sind gewöhnliche Werte eines `Error`-Interfaces, **explizit** im
  Rückgabetyp — man *sieht* an der Signatur, dass etwas schiefgehen kann.
- Kein `throw`/`catch`, kein Unwind, keine unsichtbaren Fehlerpfade.

**Gos Schwächen behoben:**
- Statt `if err != nil { return err }` in jeder Zeile: der `?`-Operator
  propagiert (früher Rückkehr mit dem Fehler) — *derselbe* Wert-basierte Fluss, nur
  ohne Boilerplate. Man *kann* aber jederzeit explizit prüfen (Go-Stil), wenn man
  differenziert behandeln will.
- **Fehler-Wrapping mit Kontext** (wie Gos `fmt.Errorf("...: %w", err)`, nur
  getypt): `err.wrap("konnte {path} nicht lesen")` — Kette bleibt inspizierbar.
- **Stacktraces**: In Debug-Builds trägt jeder Fehler den Erzeugungs-Pfad (Punkt 8)
  — das, was Go schmerzlich fehlt.

```vire
// Signatur macht Fehlbarkeit sichtbar (Go-Prinzip):
fn load(path: Str) -> (Config, Error) {          // Mehrfachrückgabe wie Go …
    raw = read_file(path)?                        // … aber `?` statt if-err-Kaskade
    parse(raw).wrap("Config {path}")              // Kontext an den Fehler
}

// Explizite Behandlung, wenn gewünscht (voll Go-Stil):
cfg, err = load("app.cfg")
if err != nil {
    log.error("start fehlgeschlagen", err: err)   // err trägt Kontext + Debug-Pfad
    return err
}

// Typisierte Fehler + Pattern-Matching, wenn man verzweigen will:
match err {
    NotFound(p)   -> create_default(p)
    Permission(p) -> fatal("keine Rechte: {p}")
    _             -> return err
}
```

**Zuschnitt:** Vire mischt Gos *Werte-Explizitheit* mit einem `?`-Zucker und
typisierten Fehlern (mehr als Gos nacktes `error`-Interface). Kein Panic-für-alles.
`panic`/`abort` bleibt für **Programmierfehler** (Invarianten-Bruch), nicht für
erwartbare Fehler — mit Crash-Pfad (Punkt 8).

---

## 8. Debug-Symbole mit Crash-Pfaden in Debug-Builds 🟢

**Urteil:** Klar machbar; LLVM liefert das Fundament. Zwei Ausbaustufen:

1. **DWARF-Debug-Info** (`--debug`/Debug-Profil): der Backend emittiert
   `!DILocation`/`!DISubprogram`-Metadaten (LLVM kann das nativ) → `gdb`/`lldb`,
   Breakpoints, Variablen, Quellzeilen funktionieren wie bei C. Reine Backend-
   Arbeit (Zeilennummern durchs Front-End bis in die IR reichen).
2. **Crash-Pfade zur Laufzeit** (Debug-Builds): bei `panic`, unbehandeltem Fehler,
   Bounds-/Null-Verletzung druckt die Runtime einen **Stacktrace mit
   `datei:zeile:funktion`** statt nur „abort". Umsetzung: `backtrace()` über die
   Frame-Pointer + Symbolauflösung aus der DWARF-Info (oder ein kompaktes
   eigenes Symboltabellen-Format fürs freestanding-Ziel).

```
panic: index 7 out of bounds for length 5
  at matrix.vr:42:14   in Matrix.get
  at solver.vr:88:9    in step
  at main.vr:12:5      in main
```

- In **Release** standardmäßig aus (0 Overhead, kleine Binaries) — optional
  einschaltbar (`--release --backtrace`) für Produktions-Diagnose.
- Passt zu Punkt 7: erzeugte `Error`-Werte hängen in Debug den Erzeugungs-Pfad an.
- Fürs **freestanding/seL4-Ziel**: eine schlanke Symboltabelle + `plat_puts`, kein
  libc-`backtrace` nötig.

---

## Gesamtbild

| # | Feature | Urteil | Kern |
|---|---|---|---|
| 1 | Multithreading + Race-Sicherheit | 🟡 | Ergonomie leicht; Race-Freiheit „by construction" (Kanäle/Mutex + leichte Send-Inferenz), keine Totalgarantie |
| 2 | Template-Programmierung | 🟢 | Generics+Traits (monomorphisiert) + `comptime` statt C++-Templates |
| 3 | Compile-Time-Reflection | 🟢 | stärkster Fit (Closed-World), zero-cost, ersetzt `@derive`/Serialisierung |
| 4 | Präprozessor-Makros | 🔴→ | **kein** C-Präprozessor; hygienische Makros + `comptime`-`@if` |
| 5 | Meson first-class | 🟢🟡 | Meson-Modul + stabile CLI; Empfehlung: Meson *adoptieren* statt eigenes Build |
| 6 | Logger in gut | 🟢 | strukturiert, comptime-weggeschaltet (0 Kosten), Spans, Sinks |
| 7 | Error handling à la Go | 🟡 | Werte+explizit (Go), aber `?`-Zucker, Wrapping, Typen, Debug-Pfade |
| 8 | Debug-Symbole + Crash-Pfade | 🟢 | DWARF via LLVM + Laufzeit-Backtrace in Debug; Release 0 Overhead |

**Zwei Punkte gegen den Wortlaut, mit Begründung:** (4) C-Präprozessor ist ein
Rückschritt — hygienische Makros/`comptime` liefern dasselbe sicher; (1)
„race-frei für alles" ist ohne Ownership-Annotationen nicht seriös versprechbar —
„sicher by construction für den Kanal-/Mutex-Stil" schon. Beides ist *mehr*
Ehrlichkeit, nicht weniger Feature.

**Alle acht docken an vorhandene FastLLVM-Fähigkeiten an:** Threads/atomare RC (1),
Monomorphisierung/Inliner (2), Whole-Program-Typgraph (3, für Reflection), der
Solver (1, Send-Inferenz), clang→Objekt (5), comptime als Front-End-Auswerter (2,3,
4,6), das pending-/Panic-Modell (7,8), LLVM-Debug-Metadaten (8). Neu zu bauen ist
das Front-End (Lexer/Parser/Inferenz/`comptime`-Auswerter) — der Backend-Stack
bleibt.
