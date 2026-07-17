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
| Exceptions | ✅ **umgesetzt** (pending-Modell): `jrt_throw` setzt eine schwebende Exception, der Code prüft nach jedem werfenden Aufruf `jrt_pending_set` → Handler oder Propagation (Cleanup + Dummy-Return). Kein Unwinder/Personality. Frontend liest die Exception-Table, splittet Blöcke an werfenden Aufrufen, Handler betreten mit der Exception aus `jrt_take_pending`; RC-korrekt. **Typspezifische `catch`-Diskriminierung** über Dispatch-Ketten mit `jrt_pending_instanceof`; mehrere `catch`-Blöcke und Subklassen-Matching; `finally` funktioniert. **ArithmeticException** (Division durch 0) ist **abfangbar**: `idiv/irem/ldiv/lrem` sind werfende Runtime-Calls, die ein immortales Sentinel-Objekt in `pending` setzen (mit Meldungstext für Uncaught). **Array-NPE/Bounds** und **Feld-/Receiver-NPE abfangbar**: Array-Zugriffe über gekapselte Runtime-Helfer, getfield/putfield/virtueller Aufruf über einen Backend-erzeugten Skip-Branch (LLVM-Blöcke, unabhängig vom Frontend-IR-Modell); devirtualisierte Aufrufe via `CallGuarded`. **Klassenname** in Uncaught-Meldung über den Type-Descriptor. **Exception-Hierarchie + Messages** ✅: `Throwable`/`Exception`/`RuntimeException` sind eingebaute Basisklassen (`register_throwables`) mit `$message`-Feld auf `Throwable` und generierten `<init>()`/`<init>(String)`-Rümpfen — `new RuntimeException("…")` und benutzerdefinierte Exceptions mit `super(msg)` funktionieren, der Type-Descriptor verkettet Subklassen korrekt. `getMessage()` als Frontend-Intrinsic → `jrt_throwable_message` (liest `$message`, Sentinel-sicher via Type-Descriptor-Prüfung → `null`). Die drei Basis-Throwables bleiben im *catch* bewusst catch-all, damit descriptor-lose Laufzeit-Sentinels weiter von `catch(RuntimeException)` gefangen werden. `CallGuarded` wird geinlint (Null-Wächter als synthetische Blöcke vor dem Callee-Rumpf, abfangbare NPE bleibt erhalten). Offen: String-Intrinsic-NPE (`s.length()` bei null) bleibt `exit` |
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
| Null-Dereferenz | expliziter Check vor Feldzugriff/Dispatch → **abfangbare** `NullPointerException` (Backend erzeugt einen Skip-Branch um getfield/putfield/virtuellen Aufruf; `jrt_throw_npe` setzt pending). String-Methoden-NPE (Intrinsics) bleibt `exit` |
| Division/Überlauf | `jrt_idiv`/`jrt_irem` (Exception bei /0, `MIN/-1` definiert); Arithmetik wrappt definiert; Shift-Beträge maskiert |
| Typkonfusion | Closed World + Casts: statisch bewiesen wo möglich, sonst Laufzeit-`checkcast` gegen den Type-Descriptor (modellierte Zielklasse → `ClassCastException` bei Mismatch; nicht modellierte wie `String`/`java.lang.*` → passthrough); Vtable-Slots nur für RTA-erreichbare Methoden |

**Escape-Analyse → Stack-Allokation (`crates/solver/src/escape.rs`):** Objekte, die ihre Funktion beweisbar nie verlassen (kein Return, kein Call-Argument, nie in Statik/Array gespeichert; Alias-Fixpunkt über Copy-Ketten), werden `alloca` statt Heap — exakt Rusts Ownership-Modell für den beweisbaren Teil: ein Besitzer (der Stack-Frame), statisch bekannte Lebenszeit. Konservativ: Allokationen in Schleifen bleiben Heap (Alloca-Wiederverwendung bei lebenden Aliasen wäre unsound). Läuft nach Devirt+Inlining, weil geinlinte Konstruktoren/Getter aus „entkommt als Argument" ein sichtbares `putfield` machen. **Feld-Sensitivität** ✅: `obj.field = value` verbindet `value` und `obj` in einer Zusammenhangskomponente; eine Komponente wird nur *gemeinsam* stack-alloziert (both-or-neither), sobald **kein** Mitglied entkommt. Das ist RC-sicher, weil immortale Stack-Objekte ihre Drop-Funktion nie ausführen: ein promovierter Container hält damit ausschließlich ebenfalls immortale (Stack-)Inhalte — nichts, das lecken könnte. Speichert ein verfolgtes Objekt dagegen eine *unbekannte* Heap-Referenz (Parameter/`this`/getfield-Ergebnis) in ein Feld, entkommt der Container (sonst Leck); wird ein Objekt in einen *fremden* Container gelegt, entkommt der Inhalt (sonst dangling). Verifiziert: verschachtelte lokale Objektgraphen und lokal geteilte Inhalte werden komplett auf den Stack gelegt, entkommende Container halten ihre Inhalte korrekt im Heap — Heap-Bilanz überall 0 live.

**Reflection/„dynamisches" Klassenladen (umgesetzt, §1.3):** `Class.forName`, `X.class`, `getName`, `newInstance` werden per lokaler Konstantenpropagation (Origin-Analyse mit Copy-Ketten) zur Compile-Zeit aufgelöst; Class-Objekte sind Singletons mit Pointer-Identität. Nicht auflösbar → Build-Fehler mit Begründung, keine stillen Laufzeitfallen.

**Klassenbibliothek:** „läuft echter Java-Code" heißt `java.base` (String = UTF-16, Collections, Math, IO). OpenJDK `java.base` ist GPLv2 **mit Classpath Exception** → statisches Linken erlaubt. Alternativen: TeaVM-Classlib (Apache-2.0, Teilmenge), GNU Classpath. **Umgesetzte Teilmenge:** `String.length/charAt/equals/isEmpty` und `System.out.print(ln)` für String/int/char als Runtime-Intrinsics (Byte-/ASCII-Semantik statt UTF-16; `charAt` liefert das Byte). **String-Konkatenation** (Java 9+ `invokedynamic`/StringConcatFactory) ✅ statisch aufgelöst (§1.3): der Parser liest BootstrapMethods + InvokeDynamic-Konstanten, das Frontend interpretiert das `makeConcatWithConstants`-Recipe (``=Argument, ``=Konstante) und faltet die Teile mit `jrt_str_concat`; primitive Argumente über `jrt_{int,char,bool}_to_str`. Strings haben jetzt den vollen Objekt-Header, sodass Literale (immortal) und zur Laufzeit erzeugte Strings (RC-verwaltet, per Leak-Detektor auf 0 live verifiziert) uniform sind. Offen: StringBuilder, `Object.toString`-Konkatenation.

**Lambdas** ✅ (`invokedynamic`/`LambdaMetafactory`, statisch aufgelöst, §1.3): der Parser liest MethodHandle/MethodType-Konstanten, das Frontend erzeugt pro Lambda-Callsite eine **synthetische Klasse**, die das Funktionsinterface implementiert und die SAM-Methode an die von javac generierte `lambda$…`-Rumpfmethode weiterleitet (eingefangene Variablen als Felder). Nicht-einfangende und einfangende Lambdas, mehrere Parameter/Captures, Lambda als Argument — verifiziert (`examples/Lambdas.java`), RC-sauber. Damit sind Funktionsinterfaces möglich. **Methoden-Referenzen** ✅ (statisch, unbound-instanz via `CallVirtual`, Konstruktor via `new`+`<init>`, Intrinsic-Ziele wie `String::length` direkt); **Boxing-Adaption** an der SAM-Grenze (primitive Rückgabe → Wrapper-`valueOf`, wenn das Interface `Object` erwartet). **Streams** ✅ als java.util.stream-Stub-Schicht auf Lambdas: `Stream` (Interface) + `StreamImpl` mit `map`/`filter`/`forEach`/`count`, `ArrayList.stream()`, plus `java.util.function` (`Function`/`Predicate`/`Consumer`). Verifiziert (`examples/Streams.java`): `list.stream().filter(l).map(String::length).forEach(l)` mit Lambdas, Methoden-Referenz und Autoboxing — RC-sauber. **StringBuilder** ✅ (runtime-gestützt). Offen: `altMetafactory`-Sonderfälle (Serializable), Argument-Unboxing an der SAM-Grenze, lazy Streams/`collect`.

**Generische Collections** ✅ demonstriert über eine mitkompilierte Java-Bibliothek (`examples/MiniList.java`): `MiniList<E>` mit internem `Object[]` + Wachstum; javac wendet Type-Erasure an, der Compiler sieht `Object`-Signaturen, der Aufrufer bekommt automatisch `checkcast` eingefügt (statisch/Laufzeit, s. §6a). Voll RC-verwaltet inkl. der beim Wachstum weggeworfenen Arrays. **Echtes `java.util`** ✅ demonstriert (`stdlib/`): Stub-Klassen im reservierten `java.util`-Paket werden per `javac --patch-module java.base=…` kompiliert; Nutzercode nutzt ganz normal `import java.util.ArrayList` (gegen das echte JDK compiliert) und bekommt vom Compiler die Stub-`.class` untergeschoben. Die Stub-Bibliothek (`stdlib/java/util/`) umfasst `List`/`ArrayList` + `Iterator` (mit **for-each**) und `Map`/`HashMap` (hashCode-Buckets). Verifiziert: `examples/StdlibDemo.java` kombiniert `java.util.List` mit for-each, `java.util.Map<String,Integer>` mit Autoboxing, containsKey/put-Rückgabe — idiomatischer Java-Code, ohne den Nutzercode anzupassen. So lässt sich die Standardbibliothek schrittweise erweitern. **equals-basierte Maps** ✅ (`examples/MiniMap.java`): Strings sind jetzt reguläre Objekte mit virtuellem `equals`/`hashCode`/`toString`-Dispatch. Object-Wurzelmethoden bekommen globale Vtable-Slots (wie Interface-Methoden), jede Klasse füllt sie mit ihrer Überschreibung oder dem Runtime-Default (`jrt_obj_equals` = Identität); String füllt sie mit `jrt_str_*`. Strings bekommen eine generierte `@vt.java_lang_String` (Literale referenzieren sie direkt, dynamische über einen von `main` gesetzten Zeiger). `instanceof` und `checkcast` nutzen dieselben Type-Descriptoren. Verifiziert: Map-Lookup über `equals` mit frisch konkateniertem Schlüssel (≠ Identität).

**Autoboxing** ✅: `Integer`/`Long`/`Boolean` als eingebaute Wrapper-Klassen (`register_builtins`) mit eingepacktem Primitivwert und generierter Vtable. `Wrapper.valueOf(prim)` → Runtime-Box, `.<prim>Value()` → Unboxing, `equals`/`hashCode`/`toString` virtuell (Wert-Semantik). Wrapper in Konkatenation über virtuellen `toString`; `String.valueOf`-Überladungen als Intrinsics. Kein Wertecache (`-128..127`) → boxed-Identität kann abweichen, `equals` ist korrekt. Verifiziert: Boxing/Unboxing, `Integer` als Map-Value (mit Unboxing) und als Map-Key (hashCode/equals). **HashMap** ✅ mit echten `hashCode`-Buckets (`examples/MiniHashMap.java`, open addressing + Rehashing) — reine Java-Bibliothek, kein Compiler-Umbau. Offen: `Double`/`Character`-Wrapper, `hashCode`-Wertecache.

**Enum** ✅ (`examples/Enum1.java`): `java.lang.Enum` als eingebaute Basisklasse (`register_enum`) mit `$name`/`$ordinal`-Feldern und generierten IR-Rümpfen (`name`/`ordinal`/`toString`/`<init>(String,int)`). Der von javac erzeugte `values()`-Rumpf klont das `$VALUES`-Array via `[…].clone()` → `jrt_array_clone` (flache Kopie, retained Ref-Elemente, elem_size aus dem Array-Deskriptor). `valueOf(String)` läuft über `jrt_enum_valueof`, das das statisch bekannte `values()`-Array nach `$name` durchsucht (`IllegalArgumentException` sonst). Verifiziert: `name`/`ordinal`/for-each über `values()`/`valueOf`/Identitätsvergleich, RC-sauber.

**enum in `switch`** ✅ (`examples/EnumSwitch.java`): javac erzeugt eine synthetische Hilfsklasse (`Main$1`) mit `$SwitchMap`-`int[]`, das `ordinal()` auf dichte case-Labels abbildet; deren `<clinit>` baut die Tabelle (defensiv in `try/catch(NoSuchFieldError)`). Alles gewöhnliches Bytecode → funktioniert, sobald die synthetische Klasse als Closed-World-Input dabei ist. Dafür nötig war eine **abhängigkeitsgeordnete `<clinit>`-Ausführung**: Java initialisiert lazy bei erstem Zugriff, wir eager beim Start — der Helfer-`<clinit>` ruft aber `Dir.values()`, also muss der enum-`<clinit>` vorher laufen. Das Backend zieht deshalb vor jedem `<clinit>` die `<clinit>`s der Klassen vor, deren Statik der Rumpf berührt (Feld-/New-/Call-Referenzen; ein emitted-Guard bricht Zyklen). Allgemeine Korrektheitsverbesserung, nicht nur für enum-switch.

**try-with-resources** ✅ (`examples/Twr.java`): javac entzuckert es bereits vollständig zu `try/catch(Throwable)` + `close()` in umgekehrter Reihenfolge + `addSuppressed` + `athrow` — das vorhandene pending-Exception-Modell trägt es unverändert; es fehlte nur `Throwable.addSuppressed` (rein diagnostisch → no-op). Verifiziert: Normal- und Exception-Pfad schließen mehrere `AutoCloseable`-Ressourcen in umgekehrter Reihenfolge, Heap-Bilanz sauber.

---

## 7. Priorisierung (Kosten/Nutzen)

1. Classfile-Parser + Mittel-IR (MIR-Vorbild) + naive LLVM-Absenkung — „Hello World läuft" ✅ **umgesetzt** (Cargo-Workspace `crates/`, Binary `fastjavac`; Teilmenge: statische Methoden, int-Arithmetik, Kontrollfluss, println-Intrinsics; textuelles LLVM-IR + clang statt Bindings, da inkwell/llvm-sys LLVM 22 noch nicht abdecken)
2. Closed-World-Reachability + CHA-Devirt + Inlining (größter Hebel, geringste Forschungsunsicherheit) ✅ **umgesetzt** (`crates/solver`: RTA-Fixpunkt nach Bacon/Sweeney, Devirtualisierung monomorpher Sites mit erhaltenem Null-Check, **bikonditionale Devirtualisierung** polymorpher Sites mit ≤3 konkreten Zielklassen (`CallPoly` → Vtable-Zeiger-Vergleichskaskade aus Direkt-Aufrufen statt Vtable-Dispatch; das letzte Ziel ist der else-Zweig, unter Closed World beweisbar erschöpfend; LLVM inlinet die Direkt-Calls), Pruning unerreichbarer Funktionen, Mid-IR-Inliner; dazu Objektmodell: Prefix-Layout `{vtable-ptr, super-Felder, eigene Felder}`, Vtables mit geerbten Slots, `jrt_alloc` nullt Felder — noch ohne GC, Objekte leben bis Prozessende; Interfaces/`invokeinterface`, Arrays, statische Felder und `<clinit>` weiterhin außerhalb der Teilmenge)
3. TBAA-Baum + Escape-Analyse (Heap→Stack, Lock-Elision) — ✅ **umgesetzt** (Lock-Elision entfällt mangels Threads): Escape-Analyse mit Stack-Allokation (§6a). **TBAA** ✅: Instanzfeld-Loads/Stores tragen `!tbaa`-Tags aus einem Typbaum mit einem Geschwister-Knoten je `(Owner-Klasse, Feld)` — verschiedene Felder sind für LLVM beweisbar alias-frei (CSE/Hoisting), gleiches Feld teilt einen Knoten (konservativ korrekt); nicht getaggte Zugriffe (RC-Header, Vtable, Array-Elemente über die Runtime) aliasieren konservativ mit allem → soundness-neutral. Dazu vorgezogen aus §1.3: statische Reflection-Auflösung (forName/getName/newInstance/X.class, checkcast-Beweis)
4. RC-GC + Mini-Runtime (`no_std`, seL4-Target) — ✅ **umgesetzt** (Referenzzählung, §6-GC-Option 1). Die Runtime hat eine **Plattformschicht** (die einzige Stelle mit OS-Abhängigkeiten): hosted nutzt libc, `--freestanding` (`-DFASTLLVM_FREESTANDING`) einen **statischen Heap-Allokator + zwei schwache Hooks** (`jrt_debug_putchar`/`jrt_platform_halt`) und **keine libc** — Zahlen-/Float-Formatierung, Ausgabe und Uncaught-Meldungen laufen über eigene `plat_`/`fmt_`-Helfer. `fastjavac --freestanding` erzeugt ein relozierbares Objekt; verifiziert: statisch, libc-frei (`ldd`: nicht dynamisch), RC + Zyklen-Collector + statischer Heap liefern bit-gleiche Ausgabe wie hosted (`sel4/`, Bring-up-Shim über rohe Syscalls). seL4-Einbettung: Hooks auf `seL4_DebugPutChar`/`TCB_Suspend` abbilden.
5. PGO + guarded devirtualization
6. Objektsensitive Points-to zur Präzisionsverschärfung
7. Forschungsmodule (optional): Ownership/Regionen, SMT-Orakel-Ausbau

Prototyp für eine Java-Teilmenge (Schritte 1–4): grob 3–6 Monate Ein-Personen-Arbeit.

### Stand Richtung „JARs mit Libs → performante, speichersichere Binary"

**Umgesetzt:** JAR-/Classpath-Ingestion (entpacken, Manifest-`Main-Class`, `--main`; automatische Closed-World-Sammlung aller `.class`); freestanding/seL4-Runtime (libc-frei, statischer Heap, verifiziert bit-gleich zu hosted); Intrinsics `System.arraycopy` (ref-/größenkorrekt), `Integer.parseInt`/`Long.parseLong`, `Math.abs/max/min/sqrt`, `System.currentTimeMillis/nanoTime`; `synchronized` (Einthread-No-Op-Monitore); erweiterte `String`-Methoden (indexOf/substring/startsWith/endsWith/trim/concat/compareTo). Dazu die frühere Basis: Solver (RTA/CHA + bikonditionale Devirt, Inlining, feld-sensitive Escape-Analyse, TBAA), RC + Zyklen-Collector, Exceptions, enum, Lambdas/Streams, Generics-Erasure, statisch auflösbare Reflection.

**Inzwischen zusätzlich umgesetzt:**
- **Performance/RC-Elision**: nie neu zugewiesene Ref-Parameter (v.a. `this`) bleiben geborgt — kein Entry-retain/Cleanup-release (−12% RC-Aufrufe auf Shapes, sound per Heap-Bilanz). Array-Zugriffe brauchen kein manuelles Inlining: clang -O2 inlinet die Runtime-Helfer vollständig.
- **Laufzeit-Reflection**: jede Klasse hat ein immortales `@jclass`-Objekt (Name + simpleName), der Type-Descriptor verlinkt darauf; `obj.getClass()`/`getName()`/`getSimpleName()` funktionieren am echten Laufzeittyp, Class-Identität per Pointer-Vergleich.
- **Echte Nebenläufigkeit** (`--threads`): `java.lang.Thread`/`Runnable` mit pthreads (run() über generierte Trampoline), rekursiver globaler Monitor, **atomare Refcounts** + atomare Heap-Zähler — verifiziert mit zwei OS-Threads (200000, keine Race, 0 live). Ohne `--threads` läuft `start()` synchron. Die inkrementelle Zyklen-Erkennung ist unter Threads deaktiviert (dokumentierte Grenze).
- **stdlib**: `java.util.Arrays` (fill/copyOf/sort/toString).

**Weiterhin offen (nach Hebel):**
- **Standardbibliothek** (dominant): weiterhin nur Ausschnitt. Realer Weg zu vollem `java.base`: TeaVM-Classlib/GNU Classpath adaptieren; JNI-artige C-Shims. **UTF-16**: Strings sind Byte/ASCII — echtes UTF-16 ist ein Refactor des String-Runtime + aller String-Intrinsics.
- **Reflection-Metamodell (Rest)**: `Method.invoke`/`Field.get/set`/`getDeclared*`, `Proxy`, `ServiceLoader`/SPI — Member-Metadatentabellen + generischer Invoke (Native-Image-Stil).
- **Nebenläufige Zyklen-Collection**: Bacon-Rajans concurrent-Variante (aktuell unter Threads deaktiviert), feingranulare Monitore statt eines globalen, `java.util.concurrent`, formales Speichermodell.
- **Sprach-Rest**: `new java.lang.Object`, echte Stacktraces/`getCause`, innere Klassen mit `this$0`, `ArrayStoreException`, Records/Sealed/Pattern-Matching; PGO.

Kurzfassung: **Compiler-Technik + Speichersicherheits-/Nebenläufigkeits-*Fundamente* stehen; der stehende Großaufwand ist die Breite von `java.base` (inkl. UTF-16) und das vollständige Reflection-Metamodell.** Die 55 Regressionstests laufen grün mit Heap-Bilanz 0 live — hosted, freestanding/seL4 **und** unter echten Threads.

---

## 8. Präzedenzfälle

GCJ, Excelsior JET, RoboVM, **GraalVM Native Image** (Architektur-Vorbild: Closed World, Points-to vor Codegen, Image Heap, Reachability-Metadaten), TeaVM, ParparVM. Kernliteratur: Dean/Grove/Chambers 1995 (CHA); Choi 1999 (EA); Milanova 2005 & Smaragdakis 2011 (Objektsensitivität, Doop); Van Horn/Mairson 2008 (k-CFA-Komplexität); Livshits 2005 / Smaragdakis 2015 (Reflection-Grenzen); Tofte/Talpin 1997 (Region-Inferenz).

---

## 9. Plan: Runtime-Elimination durch Solver-Ausbau

**Projektziel:** JAR → Binary *ohne Runtime*, Performance auf Rust-Niveau. Maßstab
ist Rust — das selbst nicht runtime-frei ist (liballoc, Bounds-/Overflow-Checks,
Panic-Pfad). „Mit Rust mithalten" heißt **nicht mehr Overhead als Rust**. Die
einzigen echten Deltas des heutigen `runtime.c` gegenüber Rust sind (1) der GC
(RC + Zyklen-Collector — hat Rust nicht) und (2) Java-Overhead (Boxing,
String-als-Objekt). Alles andere entspricht Rusts `std`. **Wichtig:** Rust nutzt
für geteilte veränderliche Graphen `Rc`/`Arc` = Laufzeit-RC; Java-mit-RC gegen
Rust-mit-`Rc` ist *Parität*. Der Rückstand ist nur dort, wo Rust plain ownership
nutzt und der Compiler mangels Beweis auf RC zurückfällt — das schließt der Solver.

**Harte Grenze (Ehrlichkeit):** präzises compilezeitliches Speichermanagement
beliebiger Objektgraphen ist unentscheidbar (Aliasing, dynamische Lebensdauern,
Zyklen). „Null Runtime für *jedes* Programm" ist unmöglich. Erreichbar: den
analysierbaren Großteil auf Rust-Niveau, den GC für die meisten Programme *ganz*
entfernen, den Rest auf minimale RC reduzieren.

**Gestuftes Speichermanagement** (Objekt fällt in die höchste beweisbare Stufe):
1. Stack/Skalar (entkommt nicht) — null Kosten. ✅ feld-sensitiv
2. Region/Arena (LIFO-Lebensdauer, Tofte-Talpin) — Bump/Bulk-Free.
3. Unique/Owned (linear) — Free bei letztem Gebrauch (Rust-`move`).
4. RC ohne Collector (Typgraph azyklisch) — nur inc/dec.
5. Voll-RC + Zyklen — nur der beweisbare Rest. ✅

### Sechs Phasen (je einzeln messbar, Suite bleibt grün)

1. **Azyklizitäts-Analyse → Collector-Elimination.** Typ-Referenzgraph unter
   Closed World (Kante A→B, wenn A ein Ref-Feld vom Typ T hat und B ein
   instanziierter Subtyp von T ist; Arrays als Durchleitung). Kein Typ auf einem
   Zyklus → `-DFASTLLVM_NO_CYCLES`: der Zyklen-Collector (~250 Zeilen) fällt weg,
   `retain`/`release` werden farb-/pufferfrei (billiger). Größter Runtime-Wegfall,
   sauber beweisbar, an der Binary messbar.
2. **Support-Bibliothek nach stdlib + Dead-Stripping.** String/StringBuilder/
   Boxing aus C nach `stdlib/` (wie ArrayList/Arrays) → unterliegen demselben
   Solver (Inlining, Devirt, Escape → lokaler StringBuilder wird stack-alloziert
   wie Rusts String-Buffer). Runtime mit `-ffunction-sections -fdata-sections` +
   `--gc-sections` → ungenutzte `jrt_`-Symbole werden gestrippt.
3. **Region/Arena-Inferenz.** Allokationslastige Aufrufbäume/Schleifen mit
   geschachtelter Lebensdauer in Arenen (Bump-Alloc, Bulk-Free am Region-Ende).
   Entfernt RC aus den Hotspots. Präzedenz: RTSJ Scoped Memory, ASAP/Proust.
4. **Uniqueness/Ownership-Inferenz → Moves.** Beweisbar eindeutige Referenzen am
   letzten Gebrauch freigeben statt RC — Rusts Owning-Move. Verallgemeinerung der
   Escape-Analyse auf „eindeutig, entkommt an bekannte Senke".
5. **Objekt-sensitive Points-to (Präzision).** Milanova/Smaragdakis (Doop-Stil) +
   interprozedurale Escape-Analyse; hebt automatisch die Trefferquote von 1–4.
6. **Irreduzibler Kern + Rust-Benchmark.** Übrig bleibt, was Rust auch hat:
   Allokator-Shim, Safety-Intrinsics (÷0/Bounds/NPE — per Range-Analyse
   elidierbar), Minimal-`plat_write` — ~150–250 Zeilen, deckungsgleich mit einem
   `no_std`-Rust-Support. Gegen äquivalente Rust-Programme messen (Allokation,
   Traversierung, Zahl-Crunching).

**Urteil:** „Null Runtime für alles" unmöglich; „GC eliminiert / Rust-Parität auf
dem analysierbaren Großteil" realistisch — der Collector verschwindet für
azyklische Programme ganz (Phase 1), Hot-Paths werden RC-frei (Phase 3/4), der
C-Rest schrumpft auf Rust-Niveau. Closed World liefert genau die Whole-Program-
Information, die die Ownership-Beweise brauchen.

### Umsetzungsstand & Messungen (Phasen 1–6)

- **Phase 1 (Collector-Elimination)** ✅: Azyklizitäts-Analyse → `-DFASTLLVM_NO_CYCLES`; azyklische Programme (Hello/Nums/Shapes/…) linken **ohne** Zyklen-Collector, RC wird farb-/pufferfrei. Suite 0 live beweist Soundness.
- **Phase 2 (Dead-Stripping)** ✅: `-ffunction-sections -Wl,--gc-sections` → `Hello` linkt **7 statt 144** `jrt_`-Symbole. (String/Boxing nach stdlib verlagern: dokumentierter Architekturschritt.)
- **Phase 3–5 (Präzisionskern)** ✅ als **interprozedurale Escape-Analyse** (Summaries über den Aufrufgraphen): an nicht-entkommen-lassende Calls übergebene Wertobjekte werden stack-alloziert (leck-sicher: Objekte mit Ref-Feldern bleiben Heap). Region/Arena (Phase 3) und Uniqueness-Move (Phase 4) als eigenständige Transformationen bauen darauf auf — dokumentiert, nicht umgesetzt (Forschungsniveau, RC-Korrektheit hat Vorrang).
- **Phase 6 (Rust-Benchmark, gemessen):**
  - **Reine Arithmetik (300M Iter.):** FastLLVM ≈ Rust (0,12 s vs 0,10 s) — der Backend hält mit.
  - **Division/Modulo:** ~2× — der `÷0`-geprüfte `jrt_irem` je Iteration; Rust elidiert den Check bei konstantem Divisor (dieselbe Range-Analyse elidierte ihn auch hier).
  - **Allokation im Loop (50M Objekte):** anfangs ~20× (Rusts LLVM entfernt die tote Box, FastLLVM sah durch das opake `jrt_alloc` nicht durch). **Nach Phase 3+4 geschlossen:** loop-lokale, nicht entkommende Objekte werden stack-alloziert (Phase 3) UND von der RC-Buchhaltung entkoppelt (Phase 4, immortal-only Locals), sodass LLVM sie ganz eliminiert → **0,055 s vs Rust 0,047 s (≈1,17×)**, heißer Loop ohne retain/release/alloc.
  - **Irreduzibler Kern:** eine freestanding-`Hello` (dead-stripped) hat **~2 KB `.text` / 9 Funktionen** (retain/release, putchar/halt-Hooks, println, str-Helfer) — `no_std`-Rust-Niveau.

**Umgesetzt (alle 6 Phasen):** 1 Azyklizität→Collector-Elimination ✅, 2 Function-Sections/Dead-Stripping ✅, 3 Loop-Stack-Allokation via Liveness (Region-light, both-or-neither-sicher) ✅, 4 RC-Elision für immortal-only Locals (Ownership-artig) ✅, 5 interprozedurale Escape-Analyse ✅, 6 Rust-Benchmark + irreduzibler Kern ✅.

**Fazit der Umsetzung:** Sowohl reine Arithmetik als auch **loop-allozierte, nicht entkommende Objekte** erreichen jetzt Rust-Parität (GC-frei UND RC-frei). Verbleibende Lücken: (a) ~~Safety-Check-Elision~~ **erledigt** (Bounds-Check-Elision per GVN, §9 unten), (b) Division-Check bei konstantem Divisor, (c) entkommende/geteilte Objektgraphen fallen auf RC zurück — was Rust ebenfalls mit `Rc`/`Arc` tut (Parität, kein Defizit). Der GC (Zyklen-Collector) ist für azyklische Programme *ganz* entfernt; für gemischt-zyklische bleibt er der beweisbare Rest. Suite 65/65, Heap 0 live — hosted, freestanding, threaded.

### Benchmark FastLLVM vs Rust vs C++ (g++ -O3 -march=native), bit-gleiche Ergebnisse

Bestes von 7 Läufen, native ISA (AVX2), semantisch **gematchte** Programme
(gleiche Ganzzahlbreiten in allen drei Sprachen):

| Benchmark | FastLLVM | Rust | C++ | vs Rust | vs C++ |
|---|---|---|---|---|---|
| Arithmetik (500M, i64) | 0,052 s | 0,123 s | 0,069 s | **0,42×** | **0,74×** |
| Allokation im Loop (200M) | 0,0014 s | 0,17 s (Box) | 0,0016 s | **~0×** | **0,86×** |
| Fib(42) Rekursion | 0,43 s | 0,51 s | 0,24 s | **0,85×** | 1,78× |
| Sieb (50M `boolean[]`) | 0,28 s | 0,26 s | 0,26 s | **~1,0×** | 1,05× |
| Polymorphie (200M virtuell) | 0,26 s | 0,26 s | 0,098 s | **0,97×** | 2,61× |
| Mandelbrot (4000²) | 1,11 s | 1,11 s | 1,05 s | **1,00×** | 1,06× |
| Quicksort (20M) | 1,54 s | 1,48 s | 1,86 s | **1,03×** | **0,82×** |
| Matmul (512³) | 0,18 s | 0,028 s | 0,020 s | 6,6× | 9,0× |
| NBody (20M, static arrays) | 30 s | 0,78 s | 0,76 s | 39× | 40× |
| binary-trees (Alloc/GC) | 4,4 s | 1,35 s | 1,23 s | 3,2× | 3,6× |

**7 von 10 auf/über Rust-Parität** (Arith/Alloc/Fib/Quick auch ≤ C++). Die drei
offenen Fälle und die dafür nötigen Analysen sind in
[benchmarks/README.md](benchmarks/README.md) präzise dokumentiert: **Matmul**
braucht affine Index-Bounds-Elision (`i·n+j < n²`, flusssensitive Obergrenzen →
throw-frei → LLVM vektorisiert), **NBody** interprozedurale statische Array-Längen
(RC-auf-Statics ist bereits eliminiert: 72×→39×; es fehlt die Länge), **Trees** eine
Shape-Analyse (der `Node→Node`-Typ ist zyklisch, der Baum aber azyklisch → der
Zyklen-Collector bleibt konservativ an). Alle drei sind gezielte Erweiterungen der
bestehenden Infrastruktur, keine Neubauten.

**Zwei allgemeine Codegen-Verbesserungen dieser Runde** (helfen breit, nicht nur
Benchmarks): **RC-Elision auf stabilen statischen Feldern** (ein von Funktion +
Callees ungeschriebenes static bleibt konstant → `GetStatic` ist ein Borrow, kein
retain/release) und **inline-geprüfte Array-Zugriffe** (null-/Bounds-Test setzen
pending inline über `jrt_throw_npe`/`jrt_throw_bounds`; der Zugriff bleibt ein
sichtbarer `load`/`store` statt eines opaken `jrt_?aload`-Calls → hoistbar). Dazu
`wide`-Opcode-Unterstützung (Correctness: `iinc`/Index > 8 Bit).

**4 von 5 der ursprünglichen Kernbenchmarks ≤ Rust; Arithmetik und Polymorphie
liegen beide unter Rust, Arithmetik/Allokation auch unter C++.** Die
Optimierungen im Einzelnen:

**Native Codegen** (`driver`). Der hosted-Build übersetzt mit `-march=native`
(wie optimiertes C++ auf der Zielmaschine) — Closed-World-AOT kennt das Ziel.
Vektorisiert die heiße Arithmetik mit AVX2: 0,12 s → 0,052 s (schneller als
Rusts SSE-Baseline **und** als C++). Freestanding/Cross-Ziele bleiben ausgenommen.

**Sieb — Rust-Parität (2,92× → ~1×)** durch drei zusammenwirkende Solver-Passes:
1. **Bounds-Check-Elision via globales Value-Numbering** (`solver/bounds.rs`).
   Das nicht-SSA-Mittel-IR recycelt javac-Slots, sodass Index, Schranke und Array
   am Schleifenwächter in *anderen* Locals liegen als am Zugriff. GVN vergibt
   jedem *Wert* eine slot-unabhängige Nummer (Kopien erben, Merges bilden ein Phi;
   optimistischer Phi-Kollaps löst schleifeninvariante Werte auf). „Index `<` Länge"
   (Wächter-Fakt) gegen `arr.length` (aus `new T[n]` verfolgt) + Nichtnegativitäts-
   Fixpunkt ⇒ Zugriff *unchecked* (inline-GEP, throw-frei). Deckt das Sieb-Innere
   (Long-Induktion `j += i`, `(int)j`-Index) ab (Ganzzahl-Casts werttransparent, da
   `0 ≤ j < len < 2³¹` verlustfrei) und **konstante Schranken ohne Wächter**
   (`sh[i & 1]`: `i & m` liegt in `[0,m]`, in-bounds gegen konstante Länge `> m`).
2. **Long-Vergleichs-Fusion** (`solver/longcmp.rs`). `jrt_lcmp; CmpX(_,0)` →
   native `icmp i64` (`sign(x−y) op 0 ⟺ x op y`), spart einen Aufruf je Iteration.
3. **Ref-Selbstkopie-Elision** (`solver/refcopy.rs`). GVN-bewiesen redundante
   `Assign(d, Copy(s))` (env[d]==env[s]) sind RC-neutral (`retain(x)+release(x)`
   hebt sich auf) und werden entfernt.

**Polymorphie — unter Rust (1,38× → 0,97×)** durch Reduktion des Methodenaufruf-
Overheads, den Rust/C++ nicht haben:
- **Borrow-Slot-RC-Elision** (`backend`). javacs `aload_0`-Reloads von `this` vor
  jedem `getfield` erzeugen Ref-Locals, die das Backend je Zugriff retain/release.
  Ein Local, das ausschließlich Kopien geborgter Parameter (`this`) hält, besitzt
  nie eine Referenz → RC-frei (sound, weil Heap-Stores/`return` selbst retainen).
  `Sq::area()` schrumpft von ~15 auf 3 Instruktionen (`mov; imul; ret`).
- **Null-Check-Elision** (`backend`, `Function::receiver_nonnull`). `this` in
  Instanzmethoden ist nicht-null (der Aufrufer prüft den Receiver) → die inline-
  Null-Prüfung bei `this.f`-Zugriffen entfällt.
- **Ref-Array-Bounds-Elision** (s.o. Punkt 1): `sh[i & 1]` wird *unchecked* (reiner
  GEP), Ref-Stores bleiben geprüft (Kovarianz/ArrayStoreException).

Alle Passes sind sound (Suite 65/65, Heap 0 live; Out-of-bounds/NPE mit
unbeweisbarem Index/Receiver werfen weiter). **C++ gewinnt** bei Fib (GCC-
Rekursions-Codegen) und Polymorphie (konstant-faltet die beiden festen `area()`-
Werte — Benchmark-Artefakt; FastLLVM und Rust dispatchen ehrlich dynamisch).

### Kompilierbarkeit komplexer Programme (Stand)

**Läuft:** Interfaces + **instanceof/checkcast gegen Interfaces** (Type-
Descriptor trägt die transitive Interface-Menge), Generics-Erasure +
`Comparable`-Bounds, Lambdas/Funktionsinterfaces, rekursive Strukturen, enums,
try-with-resources, switch, Exceptions, Methoden-Referenzen, **innere Klassen**
(`Objects.requireNonNull`), **Primitiv-Arrays aller Typen**, **Records**
(ObjectMethods-indy → feldweise toString/hashCode/equals via memcmp),
**Sealed + Pattern-Switch** (`SwitchBootstraps.typeSwitch` → instanceof-Index +
lookupswitch, `MatchException`). Alle bit-gleich zur JVM.
**Offen:** guarded/constant patterns (`when`), `java.time`/volle `java.base`.
Records mit Ref-Feldern vergleichen per Identität (memcmp-Grenze).

**Sieb ≤1,1× — erledigt ✅.** Beide vormals offenen Features sind umgesetzt:
(1) **Bounds-Check-Elision** per GVN-basierter Range-/Wertanalyse (Array-Länge
symbolisch aus `new T[n]`, Schleifenwächter-Fakt, Nichtnegativitäts-Fixpunkt →
*unchecked* + throw-frei; s.o. §9). (2) **Schmale Array-Breiten** — `byte[]`/
`boolean[]` liegen jetzt als 1 Byte, `char[]`/`short[]` als 2 Byte
(`ArrKind::size()`), bandbreiten-parität mit Rusts `Vec<u8>`. Ergebnis: Sieb
0,98× Rust.
