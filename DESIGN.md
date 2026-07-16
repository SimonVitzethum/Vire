# FastLLVM — Design-Dokument

Java-zu-Native-Compiler (AOT, ohne JVM/JIT) mit Whole-Program-Solver als erster Pipeline-Phase und LLVM als Backend.

Stand: 2026-07-13. Konsolidiert aus der Machbarkeitsanalyse (rustc-Backend-Frage) und der Solver-Architektur-Bewertung.

---

## 1. Grundsatzentscheidungen

### 1.1 Eingabe: Java-Bytecode, nicht Java-Quelltext

javac bleibt das Frontend. Damit sind Syntax-Kompatibilität, Generics-Erasure, Überladungsauflösung (JLS §15.12) und Typinferenz geschenkt — deren Nachbau wäre mehrere Personenjahre ohne fachlichen Gewinn. Eingabe der Pipeline sind JARs/Classfiles.

### 1.2 rustc ist kein verwendbares Backend

Der Teil-Checkout in `rustc-src/` (`rustc_abi`, `rustc_middle`, `rustc_mir_transform`, `rustc_ty_utils`) ist **Referenzlektüre, keine Abhängigkeit**. Gründe:

- Der MIR-Pass-Trait (`rustc_mir_transform/src/pass_manager.rs`) verlangt `TyCtxt` — den Query-Kontext eines *Rust-Crates*, gekoppelt an `Definitions`/DefIds aus HIR, internierte `ty::Ty`, Trait-Solver und `layout_of`. Java-Klassen müssten als synthetische Rust-`AdtDef`s eingeschleust werden; es gibt keine MIR-*Eingabe*-API (StableMIR ist bewusst nur Export).
- Alles ist `rustc_private`, nightly-only, ohne Stabilitätsgarantie.

**Mitnehmen als Vorlage:** Layout-Algorithmus aus `rustc_abi/src/layout.rs` (Feldanordnung, Nischen, ABI-Klassifizierung) und die MIR-Struktur (CFG aus Basic Blocks, Locals, Places/Rvalues, expliziter Drop) als Muster für die eigene Mittel-IR. Abschreiben statt anlinken.

Verworfene Alternative „Java → unsafe-Rust-Quelltext → rustc": schneller Prototyp, aber kein Zugang zu `gc.statepoint`/Stackmaps, Kampf gegen den Borrow-Checker bei Vererbung/Zyklen/null, Sicherheitsgarantien durch `unsafe` ohnehin verloren.

**Entscheidung:** Bytecode → eigene IR → LLVM direkt (via `inkwell` o. ä.).

### 1.3 Closed World als Kontrakt

Alle Klassen sind die zur Build-Zeit gegebenen JARs; kein dynamisches Nachladen. Das ist der Hebel, der aus heuristischen Analysen *sounde* Beweisverfahren macht (insb. CHA-Devirtualisierung, Dean/Grove/Chambers 1995) — derselbe Zuschnitt wie GraalVM Native Image. Verletzungen (unauflösbare Reflection, `Class.forName` mit dynamischem String) sind **Build-Fehler oder Nutzerdeklaration** (Konfigurationsdatei à la `reachability-metadata.json`), nicht „der Solver löst das schon".

---

## 2. Pipeline

```text
JARs (javac-Ausgabe)
   │
   ▼
1. Whole-Program Solver        — Fakten HERLEITEN
   │   Reachability, Callgraph, Points-to, Escape, CHA,
   │   Reflection-/indy-Auflösung, Immutabilität, <clinit>-Vorausrechnung,
   │   PGO-Einbindung; SMT nur als On-Demand-Orakel
   ▼
2. High-Level-Optimierer auf eigener Mittel-IR — Fakten ANWENDEN
   │   Devirt, Inlining, Heap→Stack, Lock-Elision, Bounds-Check-Elim.,
   │   Layout-Optimierung, guarded speculation (Guard + Slow-Path)
   ▼
3. LLVM-IR-Erzeugung (reich annotiert: TBAA, noalias, !prof, WPD-Metadaten, …)
   ▼
4. LLVM-Optimierung + Codegen
   ▼
5. Natives Binary (+ Mini-Runtime, no_std-fähig)
```

Wichtigste Korrektur gegenüber dem ursprünglichen Entwurf: **Solver (Analyse) und High-Level-Optimierer (Transformation) sind getrennte Phasen auf einer eigenen Mittel-IR.** „Solver liefert Metadaten, LLVM macht den Rest" unterschätzt, wie viele Optimierungen semantisches Java-Wissen brauchen, das in LLVM-IR verloren ist. Native Image (Graal IR) und HotSpot (C2 Ideal Graph) arbeiten aus genau diesem Grund so.

---

## 3. Solver-Komponenten nach Evidenzlage

### 3.1 Bewährt, tragend (Stand der Technik, produktiv erprobt)

| Komponente | Beleg / Verfahren |
|---|---|
| Callgraph + Devirtualisierung | RTA/XTA/points-to-basiert; CHA unter Closed World sound. Größter Einzelhebel, weil er Inlining freischaltet |
| Escape-Analyse → Stack-/Skalarallokation | Choi et al. OOPSLA 1999; Kotzmann/Mössenböck 2005. Statisch unter Closed World sogar sounder als im JIT |
| Immutabilität, Purity, tote Klassen/Methoden | Standard; „nie nach `<clinit>` geschrieben" ist stärker als `final` und lohnt sich |
| `<clinit>`-Vorausberechnung zur Build-Zeit | Native-Image-Praxis (Image Heap) |
| Lock-Elision via Escape-Analyse | thread-lokale Objekte brauchen keine Monitore; HotSpot-erprobt |
| PGO | AOT+PGO drückt den Abstand zum JIT auf typ. einstellige Prozent (Native-Image-Datenlage) |

### 3.2 Machbar, aber nur selektiv/geschichtet

- **Kontextsensitivität:** k-CFA ist EXPTIME-vollständig (Van Horn/Mairson 2008). Sweet Spot: **objektsensitive** Points-to (Milanova 2005; Smaragdakis POPL 2011, Doop), 2obj+heap für mittlere Programme, sonst selektiv.
- **Flow-Sensitivität:** global flow-insensitive Points-to + flow-sensitiv nur intraprozedural in SSA. Kein globales flow-sensitives Java-Whole-Program anstreben (für C skaliert sparse FS — Hardekopf/Lin CGO 2011, SVF — für Java-Whole-Program unüblich).
- **„Whole-Program-SSA":** existiert so nicht und ist unnötig — SSA pro Methode + interprozedurale Summaries (Standardarchitektur).
- **Reflection/MethodHandle/invokedynamic:** Best-Effort per Konstantenpropagation (Lambda-Bootstraps fast immer vollständig statisch auflösbar; String-Konkatenation via `-XDstringConcat=inline` teils vermeidbar). Allgemeiner Fall nachweislich unlösbar (Livshits 2005; Smaragdakis 2015). Rest: Nutzerdeklaration, s. 1.3.

### 3.3 Spekulativ / im Entwurf falsch dimensioniert

- **SMT/SAT + Symbolic Execution als Whole-Program-Phase:** Pfadexplosion, skaliert nicht (KLEE/SAGE-Befund). Stattdessen **On-Demand-Orakel** des Optimierers für punktuelle Anfragen (Bounds-Check-Beweis, einzelne Alias-Kanten, Nicht-Null).
- **Ownership-/Lifetime-Inferenz für unrestringiertes Java:** Forschungsstand ohne skalierendes sound-präzises Verfahren; die Mehrheit realer Heap-Objekte hat keinen eindeutigen Besitzer (Region-Inferenz à la Tofte/Talpin 1997 funktionierte für ML, Java-Äquivalent fehlt). Pipeline muss **ohne** diese Komponente funktionieren; sie ist optionales Forschungsmodul am Ende.
- **Sicherheits-/Thread-Analyse als Optimierungsquelle:** jenseits Escape-basierter Lock-Elision Forschungsniveau; nicht als tragende Optimierung einplanen.

---

## 4. Theoretische Grenzen: Solver vs. JIT

Harte Resultate:

1. **Rice 1953:** jede nichttriviale semantische Eigenschaft ist unentscheidbar → jeder Solver ist konservative Approximation.
2. **Präzisions-Kosten-Wand** (s. 3.2).
3. **Eingabeabhängigkeit:** PGO liefert *ein* Profil; ein JIT misst den tatsächlichen Lauf und passt sich Phasenwechseln an.

Der strukturelle Unterschied: **Ein JIT beweist nicht, er spekuliert mit Deoptimierungs-Fallback.** Ein statischer Compiler muss jede Annahme beweisen oder als Guard mit statisch mitkompiliertem Slow-Path absichern.

Substitutionsgrad der vier JIT-Stärken:

| JIT-Quelle | statischer Ersatz | Grad |
|---|---|---|
| Typspekulation (Inline-Caches) | CHA beweist viele Sites monomorph; Rest: PGO-gestützte guarded devirtualization (Guard bleibt stehen → kleine, messbare Kosten) | ~90 % |
| Wertspekulation / Quasi-Konstanten | nur beweisbar Konstantes (final / „nie nach `<clinit>` geschrieben"); für laufzeitkonstante, unbeweisbare Werte kein Äquivalent | teilweise |
| Profilgesteuerte Entscheidungen (Inlining, Layout) | statisches PGO — solange das Trainingsprofil repräsentativ ist | hoch |
| **Adaptivität** (Phasenwechsel, OSR, Re-Kompilierung) | **prinzipiell nicht substituierbar** | 0 % |

Gegenläufige *Stärken* des statischen Ansatzes, die kein JIT hat: unbegrenztes Analysebudget, globale Koordination (Whole-Program-Objektlayout-Umordnung, Dead-Field-Elimination — für JITs unmöglich, da Layouts nach dem Laden fixiert sind), Startzeit, Speicher.

**Gesamturteil** (Einschätzung, gestützt auf Native-Image-Datenlage): Closed-World-Solver + PGO ≈ 85–100 % der JIT-Peak-Performance auf regulären Server-/Embedded-Workloads (stabile Phasen — passt zum seL4-Ziel); 20–40 % Lücke bei hochdynamischen Workloads (Interpreter, Regelengines). „Solver ersetzt JIT vollständig" ist durch die Adaptivitätslücke widerlegbar; „praktisch überflüssig für statisch geartete Workloads" ist durch Native Image belegt.

---

## 5. LLVM-Anbindung

Grundregel: **Metadaten, die kein LLVM-Pass konsumiert, sind wertlos.** Für jede Information prüfen, welcher Pass sie liest — sonst selbst auf der Mittel-IR transformieren.

| Solver-Ergebnis | LLVM-Mechanismus |
|---|---|
| Devirt (bewiesen) | direkter Call — keine Metadaten nötig |
| Devirt (Kandidatenmenge) | `!callees`; oder WPD-Infrastruktur: `llvm.type.test` / `llvm.type.checked.load` + Type-Metadata an Vtables (gebaut für Clang `-fwhole-program-vtables`, vom Java-Frontend wiederverwendbar) |
| Profilverteilung polymorpher Sites | Value-Profile (`!prof` VP) → Indirect-Call-Promotion erzeugt guarded devirt |
| Aliasfreiheit | `noalias`-Parameter, `!alias.scope`/`!noalias`; **eigener TBAA-Baum für Javas Typhierarchie** (Felder verschiedener Klassen aliassen nie, `int[]`/`float[]` aliassen nie) — vermutlich größter Einzelhebel im Backend |
| Immutabilität / Vtable-Loads | `!invariant.load`, `!invariant.group` (Clang-C++-Vtable-Muster), `readonly`/`readnone` |
| Nicht-Null, Ranges, Fakten | `!nonnull`, `!range`, `!dereferenceable(N)`; `llvm.assume` sparsam (verlangsamt LLVM-Passes) |
| Heap→Stack | im Optimierer entscheiden, direkt `alloca` + `llvm.lifetime.*` emittieren (nicht dem Attributor überlassen) |
| Sync/Thread | `nosync`; elidierte Monitore gar nicht emittieren; `volatile` → LLVM-Atomics (Mapping JMM→LLVM wohldefiniert) |
| Inlining | heiße Pfade schon auf Mittel-IR inlinen; LLVM via `!prof`-Weights + Hints nachputzen lassen |
| GC-Wurzeln | `gc.statepoint`/Stackmaps — einziger Bereich mit echter LLVM-Spezialinfrastruktur |

Ownership über Funktionsgrenzen auf Heap-Objekten hat in LLVM kein Vokabular → nicht als Metadaten ausdrücken, sondern selbst absenken (Freigabe/Arena-Zuordnung direkt emittieren).

**Guarded speculation als expliziter Mechanismus der Mittel-IR** („speculative edge mit Fallback"): jede nur profilgestützte Annahme braucht Guard + statisch mitkompilierten Slow-Path. Deopt-Ersatz; ohne expliziten Mechanismus wuchert das.

---

## 6. Java-Semantik ohne Runtime

„Literally zero Runtime" gibt es nur bei Spracheinschränkung (keine Allokation nach Init, Arena-only — Java-Card-/SCJ-Weg; für seL4 ggf. der ehrlichste). Realistisch: einige hundert Zeilen `no_std`-Rust (Allokator, Wurzeln, Startup, `<clinit>`-Reihenfolge).

| Feature | Auflösung |
|---|---|
| GC | s. u. |
| Exceptions | ✅ **umgesetzt** (pending-Modell): `jrt_throw` setzt eine schwebende Exception, der Code prüft nach jedem werfenden Aufruf `jrt_pending_set` → Handler oder Propagation (Cleanup + Dummy-Return). Kein Unwinder/Personality. Frontend liest die Exception-Table, splittet Blöcke an werfenden Aufrufen, Handler betreten mit der Exception aus `jrt_take_pending`; RC-korrekt. **Typspezifische `catch`-Diskriminierung** über Dispatch-Ketten mit `jrt_pending_instanceof`; mehrere `catch`-Blöcke und Subklassen-Matching; `finally` funktioniert. **ArithmeticException** (Division durch 0) ist **abfangbar**: `idiv/irem/ldiv/lrem` sind werfende Runtime-Calls, die ein immortales Sentinel-Objekt in `pending` setzen (mit Meldungstext für Uncaught). **Array-NPE/Bounds abfangbar** (Zugriffe über gekapselte Runtime-Helfer, s. §6a). Offen: abfangbare Feld-NPE (getfield/putfield) und Receiver-NPE bei virtuellem Aufruf (bleiben `exit` — der inline-Check vor gep/dispatch bräuchte einen Mid-Block-Branch), Klassenname statt Sentinel-Meldung |
| Vererbung/Interfaces | ✅ Vtables mit globalen Interface-Slots (dieselbe Interface-Methode überall am selben Slot); RTA devirtualisiert monomorphe Interface-Calls. Laufzeit-Typinfo: Type-Descriptor pro Klasse in Vtable-Slot 2 (`{ ptr super }`-Kette), `jrt_instanceof` für Casts/catch |
| Reflection/`forName`/dyn. Laden | Closed World + Deklaration (s. 1.3) |
| `null` | explizite Checks (Segfault-Handler-Trick = Runtime) |
| Integer (int/long) | `wrapping_*`; div/0 → `ArithmeticException`; `MIN/-1` definiert; Shift maskiert (&31/&63); `lcmp` über Runtime |
| Floats (double) | striktes IEEE — nie Fast-Math/FMA-Contraction; `dcmpl/dcmpg` mit NaN-Semantik; `d2i/d2l` saturierend (JLS 5.1.3); `toString` als `%g`-Näherung statt Kürzest-Format |
| `synchronized`/`volatile` | JMM → LLVM-Atomics-Ordering |
| `<clinit>` | Startup in definierter Reihenfolge; wo möglich zur Build-Zeit vorausgerechnet |

**GC-Optionen** (Reihenfolge = Implementierungsplan):
1. **Referenzzählung + Zyklen-Collector** ✅ **umgesetzt** — deterministisch, keine Stackmaps; sammelt auch Zyklen ein. Modell (Backend + `runtime.c`): Objekt-Header `{ i64 refcount, i64 rcflags, ptr vtable, felder… }`; refcount<0 = *immortal* (Stack-Objekte aus der Escape-Analyse, String-/Class-Literale) → retain/release/Collector fassen sie nie an. Owning-Slot-Disziplin: jedes Ref-Local/-Feld hält +1; Store retained neu / released alt; Ref-Parameter werden bei Eintritt retained; Rückgabe transferiert +1; Funktionsende released alle Ref-Locals; Vtable-Slot 0 = Drop-Funktion (released Ref-Felder), Slot 1 = Trace-Funktion (besucht Ref-Felder mit Callback). Aufrufargumente sind geborgt (kein RC). **Zyklen:** synchroner Collector nach Bacon & Rajan 2001 (§3) — beim Dekrementieren auf rc>0 wird das Objekt purple *candidate root*; `jrt_collect_cycles` (bei Prozessende und ab Buffer-Schwelle) macht MarkRoots→ScanRoots→CollectRoots über die generierten Trace-Funktionen. `rcflags` trägt Farbe (2 bit) + buffered-Bit. Leak-Detektor über `FASTLLVM_HEAPSTATS`. Verifiziert: azyklische Graphen, Selbst-/Zweier-/Dreier-Zyklen und 500 kurzlebige Zyklen gehen alle auf 0 live. **Erster GC.**
2. Escape-Analyse + Regionen/Arenen — eliminiert je nach Programm 20–60 % der Allokationen, ersetzt den Kollektor aber nicht.
3. Präzises Mark-Sweep via Statepoints — realistisch 2–5k LOC.
4. Arena-only per Spracheinschränkung (SCJ-Modell).

### 6a. Speichersicherheit („Rust-artig")

Ziel: die Sicherheitsgarantien von Rust — kein Use-after-free, kein Out-of-bounds, keine wilden Pointer — hergestellt durch **statischen Beweis wo möglich, Laufzeit-Check wo nötig**. Nicht Ziel: Rusts Typsystem nachbauen; Java-Programme tragen keine Lifetime-Annotationen, also muss der Solver die Beweise liefern (DESIGN.md §3.3: Ownership-Inferenz ist Forschungsmodul, die Teilmenge unten ist der tragfähige Teil).

Stand der Garantien (umgesetzt):

| Gefahr | Absicherung |
|---|---|
| Use-after-free | Kein manuelles `free`. Heap-Objekte werden per **Referenzzählung** (§6-GC-Option 1) freigegeben, sobald die letzte Referenz endet; Stack-Objekte nur nach **bewiesenem** Nicht-Entkommen (Escape-Analyse, s. u.). Doppel-Free ausgeschlossen (immortal-Markierung + Owning-Slot-Disziplin, per Leak-Detektor verifiziert) |
| Wilde/uninitalisierte Pointer | `jrt_alloc` nullt; keine Pointerarithmetik in der Sprache; Casts (`checkcast`) werden **statisch bewiesen** oder sind Build-Fehler |
| Array-Zugriff außerhalb der Grenzen | Zugriffe über Runtime-Helfer (`jrt_iaload`/`jrt_aastore`/…) mit gekapseltem Check → **abfangbare** `ArrayIndexOutOfBoundsException` und `NullPointerException` (pending-Modell, Sentinel-Objekt); negative Länge → `NegativeArraySizeException` (noch `exit`) |
| Null-Dereferenz | expliziter Check vor Feldzugriff/Dispatch → definierte `NullPointerException` statt UB |
| Division/Überlauf | `jrt_idiv`/`jrt_irem` (Exception bei /0, `MIN/-1` definiert); Arithmetik wrappt definiert; Shift-Beträge maskiert |
| Typkonfusion | Closed World + Casts: statisch bewiesen wo möglich, sonst Laufzeit-`checkcast` gegen den Type-Descriptor (modellierte Zielklasse → `ClassCastException` bei Mismatch; nicht modellierte wie `String`/`java.lang.*` → passthrough); Vtable-Slots nur für RTA-erreichbare Methoden |

**Escape-Analyse → Stack-Allokation (`crates/solver/src/escape.rs`):** Objekte, die ihre Funktion beweisbar nie verlassen (kein Return, kein Call-Argument, nie als Wert gespeichert; Alias-Fixpunkt über Copy-Ketten), werden `alloca` statt Heap — exakt Rusts Ownership-Modell für den beweisbaren Teil: ein Besitzer (der Stack-Frame), statisch bekannte Lebenszeit. Konservativ: Allokationen in Schleifen bleiben Heap (Alloca-Wiederverwendung bei lebenden Aliasen wäre unsound). Läuft nach Devirt+Inlining, weil geinlinte Konstruktoren/Getter aus „entkommt als Argument" ein sichtbares, harmloses `putfield` machen.

**Reflection/„dynamisches" Klassenladen (umgesetzt, §1.3):** `Class.forName`, `X.class`, `getName`, `newInstance` werden per lokaler Konstantenpropagation (Origin-Analyse mit Copy-Ketten) zur Compile-Zeit aufgelöst; Class-Objekte sind Singletons mit Pointer-Identität. Nicht auflösbar → Build-Fehler mit Begründung, keine stillen Laufzeitfallen.

**Klassenbibliothek:** „läuft echter Java-Code" heißt `java.base` (String = UTF-16, Collections, Math, IO). OpenJDK `java.base` ist GPLv2 **mit Classpath Exception** → statisches Linken erlaubt. Alternativen: TeaVM-Classlib (Apache-2.0, Teilmenge), GNU Classpath. **Umgesetzte Teilmenge:** `String.length/charAt/equals/isEmpty` und `System.out.print(ln)` für String/int/char als Runtime-Intrinsics (Byte-/ASCII-Semantik statt UTF-16; `charAt` liefert das Byte). **String-Konkatenation** (Java 9+ `invokedynamic`/StringConcatFactory) ✅ statisch aufgelöst (§1.3): der Parser liest BootstrapMethods + InvokeDynamic-Konstanten, das Frontend interpretiert das `makeConcatWithConstants`-Recipe (``=Argument, ``=Konstante) und faltet die Teile mit `jrt_str_concat`; primitive Argumente über `jrt_{int,char,bool}_to_str`. Strings haben jetzt den vollen Objekt-Header, sodass Literale (immortal) und zur Laufzeit erzeugte Strings (RC-verwaltet, per Leak-Detektor auf 0 live verifiziert) uniform sind. Offen: StringBuilder, `Object.toString`-Konkatenation.

**Generische Collections** ✅ demonstriert über eine mitkompilierte Java-Bibliothek (`examples/MiniList.java`): `MiniList<E>` mit internem `Object[]` + Wachstum; javac wendet Type-Erasure an, der Compiler sieht `Object`-Signaturen, der Aufrufer bekommt automatisch `checkcast` eingefügt (statisch/Laufzeit, s. §6a). Voll RC-verwaltet inkl. der beim Wachstum weggeworfenen Arrays. Der Weg für echtes `java.util` (statt Mini-Bibliothek) wäre `--patch-module`/bootclasspath-Kompilierung der Klassen. **equals-basierte Maps** ✅ (`examples/MiniMap.java`): Strings sind jetzt reguläre Objekte mit virtuellem `equals`/`hashCode`/`toString`-Dispatch. Object-Wurzelmethoden bekommen globale Vtable-Slots (wie Interface-Methoden), jede Klasse füllt sie mit ihrer Überschreibung oder dem Runtime-Default (`jrt_obj_equals` = Identität); String füllt sie mit `jrt_str_*`. Strings bekommen eine generierte `@vt.java_lang_String` (Literale referenzieren sie direkt, dynamische über einen von `main` gesetzten Zeiger). `instanceof` und `checkcast` nutzen dieselben Type-Descriptoren. Verifiziert: Map-Lookup über `equals` mit frisch konkateniertem Schlüssel (≠ Identität).

**Autoboxing** ✅: `Integer`/`Long`/`Boolean` als eingebaute Wrapper-Klassen (`register_builtins`) mit eingepacktem Primitivwert und generierter Vtable. `Wrapper.valueOf(prim)` → Runtime-Box, `.<prim>Value()` → Unboxing, `equals`/`hashCode`/`toString` virtuell (Wert-Semantik). Wrapper in Konkatenation über virtuellen `toString`; `String.valueOf`-Überladungen als Intrinsics. Kein Wertecache (`-128..127`) → boxed-Identität kann abweichen, `equals` ist korrekt. Verifiziert: Boxing/Unboxing, `Integer` als Map-Value (mit Unboxing) und als Map-Key (hashCode/equals). **HashMap** ✅ mit echten `hashCode`-Buckets (`examples/MiniHashMap.java`, open addressing + Rehashing) — reine Java-Bibliothek, kein Compiler-Umbau. Offen: `Double`/`Character`-Wrapper, `hashCode`-Wertecache.

---

## 7. Priorisierung (Kosten/Nutzen)

1. Classfile-Parser + Mittel-IR (MIR-Vorbild) + naive LLVM-Absenkung — „Hello World läuft" ✅ **umgesetzt** (Cargo-Workspace `crates/`, Binary `fastjavac`; Teilmenge: statische Methoden, int-Arithmetik, Kontrollfluss, println-Intrinsics; textuelles LLVM-IR + clang statt Bindings, da inkwell/llvm-sys LLVM 22 noch nicht abdecken)
2. Closed-World-Reachability + CHA-Devirt + Inlining (größter Hebel, geringste Forschungsunsicherheit) ✅ **umgesetzt** (`crates/solver`: RTA-Fixpunkt nach Bacon/Sweeney, Devirtualisierung monomorpher Sites mit erhaltenem Null-Check, Pruning unerreichbarer Funktionen, Mid-IR-Inliner; dazu Objektmodell: Prefix-Layout `{vtable-ptr, super-Felder, eigene Felder}`, Vtables mit geerbten Slots, `jrt_alloc` nullt Felder — noch ohne GC, Objekte leben bis Prozessende; Interfaces/`invokeinterface`, Arrays, statische Felder und `<clinit>` weiterhin außerhalb der Teilmenge)
3. TBAA-Baum + Escape-Analyse (Heap→Stack, Lock-Elision) — ⚙️ **teilweise**: Escape-Analyse mit Stack-Allokation umgesetzt (§6a); TBAA und Lock-Elision offen. Dazu vorgezogen aus §1.3: statische Reflection-Auflösung (forName/getName/newInstance/X.class, checkcast-Beweis)
4. RC-GC + Mini-Runtime (`no_std`, seL4-Target) — ✅ **umgesetzt** (Referenzzählung, §6-GC-Option 1; Runtime aktuell hosted/libc, `no_std`-Port offen)
5. PGO + guarded devirtualization
6. Objektsensitive Points-to zur Präzisionsverschärfung
7. Forschungsmodule (optional): Ownership/Regionen, SMT-Orakel-Ausbau

Prototyp für eine Java-Teilmenge (Schritte 1–4): grob 3–6 Monate Ein-Personen-Arbeit.

---

## 8. Präzedenzfälle

GCJ, Excelsior JET, RoboVM, **GraalVM Native Image** (Architektur-Vorbild: Closed World, Points-to vor Codegen, Image Heap, Reachability-Metadaten), TeaVM, ParparVM. Kernliteratur: Dean/Grove/Chambers 1995 (CHA); Choi 1999 (EA); Milanova 2005 & Smaragdakis 2011 (Objektsensitivität, Doop); Van Horn/Mairson 2008 (k-CFA-Komplexität); Livshits 2005 / Smaragdakis 2015 (Reflection-Grenzen); Tofte/Talpin 1997 (Region-Inferenz).
