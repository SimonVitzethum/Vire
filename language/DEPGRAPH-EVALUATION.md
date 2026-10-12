# Vire — Evaluation: Dependency-Graph-Ausführungsmodell

Bewertung der Idee, Vire-Programme primär (oder optional) als Dependency-Graph statt
als sequenzielle Instruktionsfolge auszuführen und daraus automatisch CPU-Parallelität
zu gewinnen. Maßstab ist das erklärte Ziel: *maximale CPU-Performance bei möglichst
wenig expliziter Parallelitätsverwaltung*.

---

## Entscheidung (vorweg)

| Variante | Verdikt |
|---|---|
| **Grundlegendes Ausführungsmodell** (Graph als Default-Semantik, Statement-Granularität, Runtime-Scheduler) | 🔴 **NICHT SINNVOLL** |
| **Optionale, compilerseitige Parallelisierung mit `par`-Scope + Auto-Parallel-Loops** (Graph nur als Analyse-Layer, statisches Fork-Join-Lowering) | 🟡 **OPTIONAL** — lohnend, aber erst nach dem Performance-Push |
| **Dependency-Graph über GPU-/Device-Operationen** (Kernel-Launches, H2D/D2H, Streams/Events) | 🟢 **SEHR SINNVOLL** — kleiner Aufwand, steht schon halb in der TODO |

Kurzfassung: Die Idee ist konzeptionell anschlussfähig, aber **an der falschen
Granularität**. Was sie verspricht, liefert auf Statement-Ebene bereits die Hardware
(Out-of-Order-Execution ist eine Dataflow-Maschine mit ~0 Overhead), und was sie an
grober Granularität liefern könnte, kostet Vire heute *mehr* als es bringt — wegen
drei sehr konkreter Eigenschaften des existierenden Runtimes.

---

## 1. Ausgangslage — was Vire heute tatsächlich ist

Belege aus dem Baum, nicht aus dem Design-Dokument:

* **Performance-Stand:** geomean ~1.00× Rust über 12 Benchmarks ([TODO.md](../TODO.md) „Current state"),
  pagerank bei `iters=400` bei **1.03× Rust**, dort gilt `normal == --no-rc` — es gibt
  **kein RC mehr zu eliminieren** ([M0.3-REGION-INFERENCE-V1.md](M0.3-REGION-INFERENCE-V1.md) v3).
* **Der verbleibende Rückstand ist der Allokator, nicht fehlende Parallelität:**
  binary-trees 2.7×, pagerank-Restlücke „pure allocation" (malloc-per-node vs. Bulk/Arena).
* **Nebenläufigkeit heute:** `spawn`/`join` mit einem Send-Check, der an der
  Thread-Grenze **nur Skalare oder `Atomic`/`Mutex`/`Channel`** durchlässt
  ([spawn.rs `boundary_cty`](../crates/vire/src/spawn.rs)) — ein gewöhnlicher Record
  (`Image`, `Node`, `List[T]`) ist an einer `spawn`-Grenze *heute ein Compile-Fehler*.
  Dazu `parallel_map(n, shared, worker)`.
* **IR:** MIR-artig (Locals + Basic Blocks + Terminator), **keine Alias-Metadaten**,
  kein TBAA, keine Effekt-Annotation je Statement ([crates/ir/src/lib.rs](../crates/ir/src/lib.rs)).
* **Was an Effekt-Analyse existiert:** `instance_field_writes` (interprozeduraler
  Call-Graph-Fixpunkt über geschriebene Instanzfelder) und `static_writes` im Backend
  ([backend/src/lib.rs:1794](../crates/backend/src/lib.rs)) — das ist der *Keim* einer
  Mod/Ref-Summary, aber feldklassen-granular und nicht objektinstanz-genau.
* **Was NICHT existiert:** eine Points-to-/Alias-Analyse auf Vire-IR. Die Andersen-Analyse
  (`csolver/absint/pointsto.rs`) und die Eraser-Datarace-Analyse (`csolver/verifier/datarace.rs`)
  arbeiten auf **C**-MSIR — sie verifizieren `native "c"`-Blöcke, nicht Vire-Code.

Das ist die reale Basis. Jede Bewertung, die davon abweicht, bewertet ein anderes Projekt.

---

## 2. Die drei harten Blocker

### 2.1 Blocker A — die Hardware ist bereits eine Dataflow-Maschine

Das Beispiel aus der Fragestellung:

```
a = f(x); b = g(y); c = h(a); d = k(b); e = combine(c, d)
```

Wenn `f`,`g`,`h`,`k` klein sind (Inline-Kandidaten, ≤ einige hundert Zyklen), dann
**überlappt der Out-of-Order-Kern die beiden Ketten bereits vollständig**: ein
Raptor-Cove-Kern (dein 13650HX) hat ~512 ROB-Einträge, 6-fach Decode/8-fach Issue,
Register-Renaming gegen WAR/WAW, spekulative Ausführung über Branches hinweg. Das ist
exakt Tomasulo-Dataflow — mit *Null* Software-Overhead und einer Präzision, die keine
statische Analyse erreicht (es kennt die tatsächlichen Adressen, nicht Alias-Klassen).

Ein Software-Scheduler kann in diesem Regime nur verlieren: Task-Erzeugung + Steal +
Join kostet in einer guten Work-Stealing-Runtime 50–500 ns; ein erfolgreicher Steal über
Kerngrenzen inkl. Cache-Migration eher 1–5 µs. Der Kern erledigt in dieser Zeit
5.000–25.000 Instruktionen.

**Konsequenz:** Software-Task-Parallelität lohnt erst ab ca. **10–50 µs Arbeit pro Task**.
Statement-Granularität liegt 3–4 Größenordnungen darunter. Das Modell muss also ≈99,9 %
seines eigenen Graphen wieder zusammenfusionieren — der gesamte Wert hängt an einer
Kostenschätzung, die der Compiler bei datenabhängiger Laufzeit prinzipiell nicht hat.

### 2.2 Blocker B — Vires Runtime bestraft Threads global (der entscheidende Punkt)

Das ist der Vire-spezifische Killer für die „grundlegend"-Variante. Sobald ein Programm
Threads benutzt, definiert der Treiber `FASTLLVM_THREADS`, und damit passieren drei
Dinge gleichzeitig ([crates/driver/src/runtime.c](../crates/driver/src/runtime.c)):

1. **Alle Refcounts werden atomar** (Zeile 1625 ff.): `__atomic_add_fetch` statt
   `h->refcount += RC_ONE`. Auf x86 ist das `lock xadd` — ~20–50 Zyklen bei Contention
   statt ~1. Das trifft *jede* Retain/Release-Stelle im ganzen Programm, auch im rein
   sequenziellen Teil.
2. **Der Zyklen-Collector wird abgeschaltet** (Zeile 1289: `FASTLLVM_COLLECTOR` ist nur
   ohne `FASTLLVM_THREADS` definiert; Kommentar Zeile 1626: *„Incremental cycle detection
   is not thread-safe under threads → disabled; … cycles remain until program end"*).
   Zyklische Strukturen — der pagerank-Ring, Doubly-Linked-Lists, Graphen — **leaken**.
3. **Der Slab-Allokator serialisiert auf einem globalen Mutex** (Zeile 218 ff.:
   `g_slab_lock`). Jede Allokation aller Worker geht durch *ein* Lock. Und Allokation
   ist ausweislich der Messungen bereits der Hauptrückstand auf Rust/C++.

Ein Dependency-Graph-Modell **als Default** bedeutet: jedes Vire-Programm ist ein
Thread-Programm. Damit zahlt jedes Programm atomares RC, verliert den Zyklen-Collector
und bekommt einen serialisierten Allokator. Das kollidiert frontal mit dem
Soundness-Floor, der laut TODO nie aufgeweicht wird (*„Java heap-balance oracle 67/67 +
`tests/vire_heap.sh` 0-live"*) — Punkt 2 macht 0-live für zyklische Programme
strukturell unerreichbar.

Grobe Schätzung des Preises, bevor überhaupt ein Task läuft: **RC-lastiger Code
−20…−60 %, allokationslastiger Code −30…−200 %** (Lock-Contention skaliert negativ mit
der Kernzahl). Man würde also eine 1.00×-Rust-Position aufgeben, um Parallelität zu
kaufen, die die meisten Programme nicht nutzen.

Das ist reparierbar (Thread-Escape-Analyse → RC nur für nachweislich geteilte Objekte
atomar; per-Thread-Slabs; nebenläufiger Bacon-Rajan-Collector) — aber das *ist* das
Projekt, nicht ein Nebenaspekt. Realistisch mehrere Personenmonate, bevor die erste
Zeile Scheduler existiert.

### 2.3 Blocker C — die Alias-Präzision, die das Modell braucht, existiert nicht

Um `f(x) ‖ g(y)` zu emittieren, braucht man ein **Must-not-conflict**-Ergebnis über die
gesamte dynamische Ausdehnung beider Aufrufe: keine gemeinsame geschriebene
Speicherstelle, transitiv über alle Callees. Vire hat dafür heute:

* Escape-Analyse (funktionslokal, konservativ),
* `instance_field_writes` (Feldklassen-granular: „schreibt `Node::rank` irgendwo im
  transitiven Callee-Set"),
* Closed World + CHA/RTA-Devirtualisierung.

Feldklassen-Granularität heißt: zwei Tasks, die **verschiedene** `Node`-Objekte
mutieren, sehen für die Analyse aus wie ein Konflikt auf `Node::rank`. Genau die Fälle,
die man parallelisieren will (disjunkte Teilgraphen, disjunkte Array-Slices), sind die
Fälle, die man ohne objektinstanz-genaue oder region-basierte Trennung nicht beweisen
kann. Ohne Points-to/Regionen auf Vire-IR degeneriert der Dependency-Graph zur
sequenziellen Kette — man baut die Maschinerie und bekommt keine Kanten heraus.

---

## 3. Die 20 Fragen

### (1) Passt das Modell konzeptionell zu Vire?

**Zur Philosophie: ja. Zur heutigen Implementierung: nein.** Vire beansprucht
ohnehin, was andere Sprachen annotieren zu lassen, per Whole-Program-Analyse zu
beweisen (Ownership, Lifetimes, Send). „Parallelität ableiten statt annotieren" ist die
konsequente Fortsetzung derselben These. Der Bruch ist nicht philosophisch, sondern
liegt in §2.2/§2.3.

### (2) Verbesserung oder unnötige Verkomplizierung?

Als Default-Modell: **Verkomplizierung** mit negativem Erwartungswert. Als
opt-in-Optimierung mit sequenzieller Referenzsemantik: **Verbesserung**, weil die
Semantik unverändert bleibt und die Analyse abschaltbar ist (jede Kante, die man nicht
beweist, kostet nur Speedup, nie Korrektheit).

### (3) Was profitiert besonders?

* **GPU-/Vulkan-/CUDA-Track** — mit Abstand am meisten (siehe §5). Kernel-Launches,
  H2D/D2H-Kopien und Vulkan-Barrieren *sind* bereits ein Dependency-Graph; heute wird
  synchron gelauncht (TODO Tier 4: *„a non-synchronous launch path (v1 syncs every
  launch)"*). Node-Granularität hier: 10 µs – 10 ms. Das ist das richtige Regime.
* **Schleifen über Collections** (`parallel_map`-Fälle) — aber die kann man heute schon
  schreiben; der Gewinn ist Ergonomie, nicht Speed.
* **Rekursive Divide-and-Conquer-Kerne** (quicksort, trees, Mandelbrot-Kacheln) —
  klassisches Cilk-Terrain, echter Gewinn, weil Granularität über Rekursionstiefe
  steuerbar ist.
* **Pipelines mit unabhängigen Phasen**, wenn die Phasen ≥ 100 µs dauern.

### (4) Was wird schwieriger?

RC/Lifetime-Korrektheit (§2.2), der Allokator, Determinismus von Panics und
Fehlerreihenfolgen, Debugbarkeit (`vire`-Backtraces über Task-Grenzen), Reproduzierbarkeit
von Benchmarks, das Heap-Oracle, `--emit-llvm`-Lesbarkeit, und der Inliner: das
Auslagern von Tasks in Outlined Functions kostet `-flto`-Inlining. Dafür gibt es im Repo
bereits einen gemessenen Präzedenzfall — *„RC-inline-as-IR (costs `-flto` inlining of
other hot helpers)"* steht in TODO.md unter „Do NOT re-attempt (measured dead)". Outlining
ist derselbe Mechanismus.

### (5)+(6) Kombination mit der Memory-/Lifetime-Analyse; taugt der Solver als Dependency-Quelle?

**Teilweise — und das ist die ehrlichste Antwort.** Was der bestehende Solver liefert
und was ein Dependency-Graph braucht, überlappt nur zur Hälfte:

| Solver liefert heute | Als Dependency-Info | Lücke |
|---|---|---|
| Escape-Analyse (Objekt verlässt Funktion nicht) | ✅ direkt: nicht-escapendes Objekt kann keinen Cross-Task-Konflikt haben | nur intraprozedural |
| `instance_field_writes` (interproz. Feldschreib-Fixpunkt) | ✅ ist eine echte Mod-Summary | Feld*klassen*-, nicht instanzgenau |
| `static_writes` | ✅ globale Konflikte | — |
| Borrow-Slots / Region-Inferenz (`cur.next` als Borrow) | ✅ „Feld ist in dieser Funktion immutabel" = Read-Only-Nachweis | an harte Guards gebunden |
| CHA/RTA-Devirtualisierung | ✅ macht Callees überhaupt sichtbar | offener virtueller Call = TOP |
| Send-Check von `spawn` | ✅ genau die richtige Frage | nur Skalare/Sync-Typen |
| — | ❌ **Ref-Summary** (was *liest* eine Funktion) fehlt komplett | |
| — | ❌ **Points-to auf Vire-IR** fehlt | |
| — | ❌ **Regionen als erstklassige Alias-Klassen** fehlen | |

Der Ownership-Solver beantwortet *„wann darf ich freigeben"*, nicht *„auf welche
Speicherregion greift dieser Aufruf zu"*. Das sind verwandte, aber nicht identische
Fragen. Der Solver ist also ein guter **Startpunkt** (~40–50 % der nötigen Information),
kein fertiger Lieferant. Ein `EffectSummary`-Fixpunkt (Reads ∪ Writes über abstrakte
Locations) wäre der ehrliche nächste Schritt — und der ist unabhängig vom
Parallelisierungsthema wertvoll (er verschärft auch die `snapshot()`-Regel aus
REFERENCE §9 und die Region-Inferenz).

### (7) Die Problemliste im Einzelnen

| Bereich | Problem | Behandlung |
|---|---|---|
| **Mutable State** | WAR/WAW erzeugen Fake-Kanten | SSA/Renaming für Werte; für Heap-Objekte mit Identität + RC **nicht** möglich (Renaming ändert Freigabezeitpunkte) |
| **Pointers/Refs** | `Ptr[T]` nur in `unsafe` | `unsafe`-Blöcke = TOP-Effekt → Serialisierungskante. Sauber lösbar, weil Vire Pointer aus dem Normalpfad heraushält |
| **Aliasing** | *der* Präzisionsboden (§2.3) | Regionen + TBAA + Closed-World-Klassen; ohne das kein nutzbarer Graph |
| **Atomics** | sind Ordnungspunkte | Als Zugriff auf eine gemeinsame abstrakte Location modellieren, SC-Kanten; Reordering über Atomics hinweg verboten |
| **Locks** | Auto-Parallelität + Locks = automatisch erzeugte Deadlock-Möglichkeit, die der Programmierer nie geschrieben hat | In `par`-Regionen **verbieten**; Lock-Nutzung ⇒ Region wird sequenziell. (CSolvers `lockorder.rs` kann das nur für C) |
| **I/O, Syscalls** | Reihenfolge ist beobachtbar | Effekt `io` = totalgeordnete Kette; alternativ Ausgabe puffern und in Programmordnung committen |
| **FFI/C/ASM** | opak | Default TOP. **Aber:** für `native "c"`-Blöcke kann CSolver Summaries liefern (`csolver_symbolic::Summary::is_pure()`) — echte Synergie, die es sonst nirgends gibt |
| **Vulkan/CUDA** | heute synchron | ✅ **der Gewinnfall** — siehe §5 |
| **Exceptions/Errors** | Vire hat overflow-checked Arithmetik → Panics; welcher von zwei parallelen Panics gewinnt? | Erste in *Programmordnung*, nicht in Zeitordnung: Fehler bis zum Join puffern. Kostet Latenz, sichert Determinismus |
| **Loops** | brauchen Dependence-Testing | Kein Polyeder-Modell nötig: Stride-/Affinitätstest + „kein Cross-Iteration-Write auf gemeinsame Location" deckt 90 % ab |
| **Rekursion** | Granularität unbekannt | Cilk-Rezept: spawnen bis Tiefe ≈ log₂(P)+2, darunter seriell; oder Lazy Task Creation (siehe §6) |
| **Dynamic Dispatch** | unaufgelöster virtueller Call = TOP ⇒ Graph kollabiert | CHA/RTA/PIC hilft; in OO-lastigem Code bleibt es der häufigste Präzisionsverlust |

### (8) Determinismus trotz paralleler Ausführung

Determinismus ist **nicht** das schwere Problem — er folgt gratis, *wenn* man nur
konfliktfreie Tasks parallelisiert. Was zusätzlich fixiert werden muss:

1. **Reduktionen mit fester Baumform**, unabhängig von Kernzahl und Scheduling. Sonst
   ändert Float-Assoziativität das Ergebnis. (Genau hier sind OpenMP, rayon und TBB
   **nicht** deterministisch — ein echtes Alleinstellungsmerkmal für Vire.)
2. **Seiteneffekte in Programmordnung committen** (Ausgabepuffer je Task, Merge beim Join).
3. **Fehler/Panics in Programmordnung** auswählen.
4. **Allokation aus Per-Task-Arenen**, damit Adressen nicht scheduling-abhängig werden —
   und Identitäts-Hashing (`identityHashCode`-artig) muss verboten oder adressunabhängig
   sein, sonst ist Determinismus nicht haltbar.
5. **Freigabereihenfolge (RC-Drop):** über Peak-RSS und Arena-Verhalten beobachtbar. Für
   Bit-Determinismus des *Ergebnisses* irrelevant, für Speicher-Benchmarks nicht.

Gut formuliert: **„gleiche Ausgabe, bitgleich, unabhängig von Kernzahl und Scheduling"**
ist erreichbar und wäre ein starkes, verkaufbares Versprechen.

### (9) Race Conditions durch Sprachdesign verhindern

Vire hat den Mechanismus im Kleinen bereits: der Send-Check in `spawn.rs`. Die
Verallgemeinerung ist eine **inferierte Sharing-Klasse** je Wert:

* `local` — nur ein Task erreicht ihn (Escape-/Thread-Escape-Analyse), nicht-atomares RC;
* `frozen` — geteilt, aber in der parallelen Region nachweislich nur gelesen
  (`instance_field_writes` liefert das schon fast), nicht-atomares RC möglich, wenn die
  RC-Ops selbst elidiert sind;
* `sync` — `Atomic`/`Mutex`/`Channel`.

Regel in einer `par`-Region: jeder Schreibzugriff auf eine Location, die nicht `local`
ist, muss `sync` sein — sonst **Compile-Fehler mit sequenziellem Fallback als
Vorschlag**. Das ist Rusts `Send`/`Sync`, nur inferiert statt annotiert, und passt exakt
zu Vires „safety by construction, not by annotation"-Linie aus
[FEATURES-EVALUATION.md](FEATURES-EVALUATION.md) §1.

### (10) Graph-Größe beherrschen

Drei Regeln, die zusammen ausreichen:

1. **Hierarchisch, nie flach.** Knoten = Region im Regionenbaum (Block, Schleifennest,
   Call), nicht Statement. Die Statement-Ebene wird nie materialisiert.
2. **Interprozedural nur über Summaries.** Ein Aufruf ist ein Knoten mit dem
   `EffectSummary` des Callees — der Callee-Graph wird nicht eingebettet.
3. **Cut-Budget.** Der Compiler schneidet nur die *k* teuersten Stellen auf
   (k ≈ 2–4 × Kernzahl); alles andere fusioniert per Default zu sequenziellem Code.
   Damit ist die Laufzeit-Graphgröße O(Kerne), nicht O(Programm), und der
   Compile-Zeit-Graph O(Statements) mit kleinem Faktor — dieselbe Größenordnung wie
   die bestehenden Solver-Passes.

### (11) Task-Fusion, Inlining, Granularität

* **Default ist Fusion**, nicht Splitting. Eine Kante wird nur zum Task, wenn das
  Kostenmodell ≥ Schwelle (Vorschlag: 20 µs geschätzte Arbeit) sagt.
* **Kostenmodell:** statische Instruktionszahl je Region × geschätzte Trip-Counts
  (Konstanten, Array-Längen, `comptime`-Werte) + Callee-Kosten aus derselben Summary.
  Unbekannt ⇒ „unbekannt", und *unbekannt heißt sequenziell*, außer bei rekursiven
  Divide-and-Conquer-Formen.
* **Ehrlicher Ausweg aus dem Kostenproblem: Lazy Task Creation** (Cilk-Prinzip). Der
  Compiler markiert *potenzielle* Parallelität; die Fortsetzung läuft sofort seriell
  weiter und wird nur dann zu einem echten Task, wenn ein anderer Kern *tatsächlich
  idle* ist und stiehlt. Dann kostet nicht-genutzte Parallelität ~10–20 ns
  (Deque-Push/Pop ohne Atomic im Normalfall) statt eines Thread-Handoffs. Das ist die
  einzige bekannte Technik, die das Granularitätsproblem *ohne* perfektes Kostenmodell
  löst — und der Grund, warum Cilk funktioniert und generische Task-Graph-Systeme nicht.
* **Inlining zuerst, Graph danach.** Der Graph muss nach Inlining/Devirt gebaut werden,
  sonst ist jeder Callee opak — dieselbe Reihenfolgen-Logik, die die Escape-Analyse schon
  nutzt (*„Runs after devirtualization + inlining"*).

### (12) CPU-Topologie, Cache, SMT, NUMA

Realistische Priorisierung für deine Zielhardware (i7-13650HX: 6 P-Kerne mit SMT,
8 E-Kerne, **1 NUMA-Knoten**):

1. **Chunk-Größe aus dem Working Set** (Footprint der Schleife vs. L2) — mit Abstand der
   größte Hebel, simpel umzusetzen.
2. **Heterogene Kerne (P/E)** — auf Alder-Lake-Nachfolgern wichtiger als NUMA: gleich
   große Chunks auf ungleich schnelle Kerne = Load Imbalance. Antwort: Work-Stealing
   (selbstbalancierend) statt statischer Aufteilung.
3. **SMT:** zwei rechenintensive Siblings auf einem Kern bringen ~1.1–1.25×, nicht 2×,
   und zerstören Determinismus-Timing. Default: **nur physische Kerne belegen**,
   SMT-Geschwister nur für nachweislich speicherlatenz-gebundene Tasks.
4. **NUMA:** auf deiner Maschine irrelevant (1 Knoten). Für Server: First-Touch-Allokation
   + per-Knoten-Deques + `numa_alloc_local`. Sollte im Design vorgesehen, aber **nicht
   in v1 gebaut** werden — sonst baut man ungetesteten Code.
5. **False Sharing:** Ergebnis-Slots je Task auf 64 B padden. Trivial, aber ohne das
   frisst es den Gewinn.

### (13) Erwarteter Performancegewinn — differenziert

Die Frage „gegenüber Vires aktuellem Modell" hat zwei Antworten:

* **Gegenüber Code, den man heute schon mit `parallel_map` schreiben kann:** ~0–15 %.
  Der Scheduler ist nicht besser als der Fork-Join, den man dort hat.
* **Gegenüber Code, der heute gar nicht parallelisierbar ist** (weil der Send-Check
  keine Records durchlässt): auf 6 P-Kernen realistisch **2.5–5×** — aber nur für den
  Teil, der beweisbar konfliktfrei ist. Und: **denselben Gewinn bekäme man deutlich
  billiger**, indem man nur den Send-Check zu einer echten Sharing-Klassen-Inferenz
  ausbaut (§9) und `parallel_for` über Objektgraphen erlaubt. Das ist ~10 % des Aufwands
  für ~70 % des Nutzens.

### (14) Wo ist das Modell langsamer?

Vollständige Liste, nach Größe sortiert:

1. Atomares RC + Slab-Lock + kein Zyklen-Collector (§2.2) — **global, immer**.
2. Verlorenes `-flto`-Inlining durch Outlining der Tasks (gemessener Präzedenzfall im Repo).
3. Verlorene Auto-Vektorisierung, wenn eine Schleife statement-weise zerschnitten statt
   chunkweise verteilt wird. (Chunking erhält SIMD, Splitting zerstört es.)
4. Working-Set-Verdopplung: `f(x) ‖ g(y)` belegt zwei Arbeitsmengen gleichzeitig im
   geteilten L3 — bei speichergebundenem Code ist parallel *langsamer* als sequenziell.
5. Scheduler-Overhead + Thread-Wakeup (5–50 µs) bei zu feiner Granularität.
6. All-Core-Turbo < Single-Core-Turbo: nominell 6× ist real ~4.5–5×; bei kurzen Regionen
   verbrennt man 6× Energie für 1.05× Speed (auf Laptops relevant).
7. False Sharing, wenn Ergebnis-Slots nicht gepaddet sind.

### (15) Compileraufwand

* **v1, eng begrenzt** (Effekt-Summaries + Konfliktanalyse + `par`-Scope + Auto-Parallel-Loop
  + statisches Fork-Join-Lowering + Determinismus-Reduktionen): **3–6 Personenmonate**
  inkl. Tests und Heap-Oracle-Absicherung. Davon gut die Hälfte allein für die
  Thread-Escape-Analyse, die atomares RC selektiv macht.
* **Vollmodell** (Graph als Ausführungsmodell, Runtime-Scheduler, Topologie, NUMA,
  volle Alias-Präzision): **1.5–3 Personenjahre**, mit erheblicher Wahrscheinlichkeit,
  dass die Präzision nie reicht, um den Overhead zu amortisieren.

### (16) Was müsste sich wo ändern?

| Komponente | Änderung | Umfang |
|---|---|---|
| **Lexer/Parser** | ein Keyword `par` (Block) + Attribut `@par` | ~50 Zeilen |
| **AST** | `Stmt::Par(Block)` bzw. `Block`-Flag | klein |
| **infer/lower** | nichts Strukturelles | — |
| **IR** | `EffectSummary` je `Function`; `Statement::ParFor { … }` + `Fork`/`Join` **oder** (besser) gar keine IR-Änderung, nur Outlining + Runtime-Calls | klein–mittel |
| **Solver** | **neu:** `effects.rs` (Mod/Ref-Fixpunkt), `conflict.rs`, `cost.rs`, `parplan.rs`; **erweitert:** `escape.rs` um Thread-Escape | **der Hauptteil** |
| **Backend** | Task-Outlining, Runtime-Calls, `!llvm.loop.parallel_accesses`, RC-Atomarität **pro Objektklasse** statt global | mittel–groß |
| **Runtime (`runtime.c`)** | Worker-Pool mit Work-Stealing-Deques, Per-Thread-Slabs, nebenläufiger oder abgesicherter Zyklen-Collector, deterministische Reduktionshelfer | ~2–4k LoC C |
| **Tests** | Determinismus-Oracle (N-Kern-Läufe bitgleich), Heap-Oracle unter Threads | nicht optional |

### (17)–(20) Vollgraph vs. hybrid, Syntax, Pipeline

* **(17) Hybrid.** Ein reines Graphmodell zwingt jeden Programmteil in eine Form, die
  ~95 % des Codes nicht braucht, und verliert die Eigenschaft, die Vire heute trägt:
  vorhersagbaren, sequenziell lesbaren, LLVM-freundlichen Code.
* **(18) Ja — sequenzielle Semantik bleibt die Referenz.** Das ist die wichtigste
  Einzelentscheidung. Parallelisierung wird damit zu einer *semantikerhaltenden
  Optimierung*: was der Compiler nicht beweist, kostet Speedup, niemals Korrektheit.
  Und `--no-par` bleibt immer ein gültiger, identisch rechnender Build — unverzichtbar
  fürs Debugging und für die Heap-Oracle-Absicherung.
* **(19) Keine eigene Graph-Syntax.** Ein Graph-DSL wäre eine zweite Sprache in der
  Sprache (genau das, was `comptime` und `@derive` bewusst vermeiden). Was gebraucht
  wird, ist **ein Scope-Marker**, kein neues Programmiermodell:
  `par { … }` bzw. `@par fn`.
* **(20) Die vorgeschlagene Pipeline stimmt fast — mit einer Korrektur:**

  ```
  normaler Vire-Code
      ↓
  Inlining + Devirtualisierung          (muss VOR die Analyse)
      ↓
  Effect-/Region-Summaries              (Solver-Fixpunkt)
      ↓
  Konfliktgraph  ← Analyse-Layer über der bestehenden MIR,
                    KEINE eigene "Execution Graph IR"
      ↓
  Kostenmodell + Cut (Default: fusionieren)
      ↓
  Lowering zu statischem Fork-Join / ParFor  (Outlining)
      ↓
  LLVM -O2 -flto -march=native
  ```

  **Eine vierte IR wäre der wahrscheinlichste Weg, dieses Projekt zu töten.** Vire hat
  bereits AST → MIR → LLVM-IR; eine „VEG-IR" dazwischen bedeutet: alle bestehenden
  Passes (constprop, bounds, escape, inline, narrow, refcopy) müssten sie kennen oder
  doppelt existieren. Der Graph gehört als *Seitenstruktur* (Map von Region-ID auf
  Effekt-Set) neben die MIR, nicht als Ersatz.

---

## 4. Vergleich mit existierenden Ansätzen

| System | Knoten-Granularität | Deps von wem | Lehre für Vire |
|---|---|---|---|
| **Halide** | Loop-Nest/Stage (ms) | Compiler kennt Deps, **Schedule schreibt der Mensch** | Erfolg kommt daher, dass der Schedule *explizit und separat* ist — das Gegenteil von „automatisch" |
| **MLIR** | frei wählbar | Dialekte (`affine`, `async`, `linalg`) | Liefert das Vokabular, nicht die Antwort. Für Vire zu schwer als Zusatz-IR |
| **XLA/TensorFlow** | Tensor-Op (µs–ms) | statischer Graph | Funktioniert, weil die Domäne alias-frei und die Granularität riesig ist |
| **Dask** | Array-Chunk/Partition (ms–s) | Nutzer-API | Overhead irrelevant bei 10⁶× größeren Knoten. Nicht übertragbar |
| **Julia** | `@threads`, Dagger.jl | explizit | Julia hat *nicht* implizit parallelisiert, trotz sehr guter Voraussetzungen |
| **Mojo** | `parallelize(...)`, Autotuning | explizit | Die aktuellste Sprache mit exakt Vires Zielen entschied sich **gegen** implizite DAG-Ausführung |
| **Erlang/Actors** | Prozess (µs) | Messages | Andere Achse: Fehlertoleranz und Isolation, nicht Speed. Kein Vorbild für CPU-Performance |
| **Dataflow-Sprachen** (Id, Sisal, Monsoon, Manchester) | Instruktion | implizit | **Der direkte Vorgänger dieser Idee — und der beste Beleg gegen die Feinvariante.** Sisal war real schneller als Fortran, aber die Token-Matching-Kosten der Dataflow-Maschinen wurden nie amortisiert; alles Überlebende wurde zu Fork-Join auf von-Neumann-Hardware |
| **Taskflow / TBB Flow Graph** | Nutzer-Task | explizit | Zeigt: die Runtime ist der *einfache* Teil. Die Deps schreibt überall der Mensch |
| **OpenMP `task depend`** | Nutzer-Task | Nutzer-Annotation | Dito — und selbst mit Annotationen ist der Overhead bei kleinen Tasks ein bekanntes Problem |
| **Cilk** | Fortsetzung (10–20 ns!) | `spawn`, aber **lazy** | **Die wichtigste Lehre:** Continuation Stealing macht ungenutzte Parallelität fast gratis. Wer automatisch parallelisieren will, braucht dieses Runtime-Modell — nicht einen Graph-Scheduler |
| **Rayon (Rust)** | `par_iter`-Split | Borrow-Checker beweist Sicherheit, Mensch setzt einen Aufruf | **Der eigentliche Konkurrent.** `.par_iter()` sind 11 Zeichen |
| **Auto-Parallelisierer** (SUIF, Polaris, Intel `-parallel`, Polly) | Loop | Compiler | 35 Jahre Forschung; erfolgreich **nur** bei affinen Array-Schleifen. Auf Pointer-/Objektcode praktisch nie |

**Muster:** Implizite feingranulare Dataflow-Ausführung hat sich auf General-Purpose-CPU-Code
noch nie durchgesetzt. Durchgesetzt haben sich (a) *explizite* Schedules in eingeschränkten
Domänen und (b) *lazy* Fork-Join-Runtimes.

---

## 5. Der Fall, in dem die Idee klar gewinnt: der GPU-Track

Das ist der Teil, den ich aktiv empfehlen würde — und er steht bereits halb in der TODO
(Tier 4: *„Write-only H2D elision + persistent context / async"*).

GPU-Operationen sind das ideale Regime für einen Dependency-Graph:
* Knoten kosten 10 µs – 10 ms ⇒ Scheduling-Overhead irrelevant.
* Die Abhängigkeiten sind **exakt bekannt**: welcher Kernel liest/schreibt welchen Buffer
  weiß der Compiler bereits (`read_only_params` existiert schon).
* Der Gewinn ist strukturell, nicht marginal: Overlap von H2D-Kopie, Kernel und
  D2H-Kopie über Streams/Events ist bei mehreren Kernels routinemäßig **1.3–2×**, und
  das Weglassen redundanter Kopien zwischen abhängigen Kernels oft mehr.
* Vulkan hat mit Barriers/Semaphores ohnehin ein Dependency-Modell — der Compiler müsste
  Barrieren aus Buffer-Reads/-Writes ableiten statt aus dem festen Runtime-Protokoll.
* Risiko für den CPU-Pfad: **null** (separater Track, wie in der TODO festgelegt).

Aufwand: Wochen, nicht Monate. Das ist derselbe Grundgedanke wie die große Idee, nur an
der Granularität, an der er nachweislich funktioniert.

---

## 6. Architektur der empfohlenen Variante (falls gebaut wird)

Falls die opt-in-Variante später gebaut wird, so — als **Erweiterung**, nicht als Umbau:

**Syntax (minimal).** Kein neues Programmiermodell, zwei Marker:
```vire
par { … }        // "in diesem Scope darfst du umordnen"
@par fn f(…)     // ganze Funktion freigegeben
for x in xs { … }   // Auto-Parallel NUR wenn (a) beweisbar konfliktfrei,
                    // (b) Kostenmodell ≥ Schwelle, (c) im `par`-Scope oder
                    //     unter --par=auto gebaut
```

**IR.** Keine neue IR. Zwei Additionen:
* `Function.effects: EffectSummary` — `{ reads: LocSet, writes: LocSet, io: bool,
  ffi: bool, alloc: bool, may_panic: bool }` mit
  `Loc ∈ { Static(sym), Field(class,field), ParamRegion(i), Heap⊤ }`.
* `Statement::ParFor { … }` / `Fork`/`Join` — oder ersatzlos über Outlining + Runtime-Call.

**Solver (der Hauptteil).**
`effects.rs` (Call-Graph-Fixpunkt, aufgesetzt auf `instance_field_writes`) →
`conflict.rs` (RAW/WAR/WAW über `Loc`, plus Kontrollkanten) →
`cost.rs` (statische Kostenschätzung, `unknown` ⇒ sequenziell) →
`parplan.rs` (Cut-Budget, Fusion, Chunking) →
`escape.rs` + Thread-Escape (entscheidet **pro Objektklasse**, ob RC atomar sein muss).

**Runtime.** Persistenter Worker-Pool, **Continuation-Stealing** (Cilk-Modell, nicht
Graph-Scheduler), Per-Thread-Slabs (löst den globalen Slab-Mutex), deterministische
Reduktionsbäume, Fehler-/Ausgabepuffer je Task. Nur gelinkt, wenn das Programm eine
`par`-Region enthält — exakt das Muster, das `FASTLLVM_THREADS` heute schon benutzt.

**Determinismus als Vertrag.** Ein `--par`-Build muss bitgleiche Ausgabe liefern wie
`--no-par`, für jede Kernzahl. Das ist testbar (N-Kern-Oracle) und ist das eine
Versprechen, das rayon/OpenMP/TBB *nicht* geben.

**Was zuerst gebaut werden müsste, bevor irgendetwas davon Sinn ergibt:**
1. Thread-Escape-Analyse → selektiv atomares RC.
2. Per-Thread-Slabs → Allokator skaliert.
3. Thread-sicherer oder region-lokaler Zyklen-Collector → 0-live bleibt haltbar.
4. `EffectSummary`-Fixpunkt (Reads *und* Writes).

Punkte 1–3 sind **Voraussetzungen**, kein Teil des Features. Sie sind auch unabhängig
davon nützlich — was sie zum richtigen ersten Schritt macht, falls die Richtung
überhaupt verfolgt wird.

---

## 7. Codebeispiele — heute vs. Modell

### 7.1 Der Task-Parallelismus-Fall

**Heute — was man schreiben *möchte*:**
```vire
fn process(img: Image) -> Report {
    hist  = histogram(img)
    edges = sobel(img)
    stats = analyze(hist)
    lines = hough(edges)
    Report(stats, lines)
}
```

**Heute — was tatsächlich geht:** *nichts davon parallel.* `spawn worker(img)` ist ein
**Compile-Fehler**, weil `Image` an der Send-Grenze weder Skalar noch `Atomic`/`Mutex`/
`Channel` ist (`spawn.rs::boundary_cty`). Man müsste `img` in ein `Mutex` legen — was
die Parallelität wieder wegnimmt. **Das ist die eigentliche Lücke, nicht das fehlende
Graph-Modell.**

**Mit dem Modell:**
```vire
par {
    hist  = histogram(img)      // img ist `frozen` (nur gelesen) → beide Ketten
    edges = sobel(img)          //   dürfen parallel, RC bleibt nicht-atomar
    stats = analyze(hist)
    lines = hough(edges)
}
Report(stats, lines)
```
Gewinn: ~1.9× auf 2 Kernen, *falls* die Stufen je ≥ 100 µs kosten. Bei einem 64×64-Bild
(~20 µs) ist es langsamer als sequenziell.

**Mit dem billigen Alternativweg** (nur Send-Check erweitert, kein Graph-Modell):
```vire
h1 = spawn analyze_hist(img)    // img frozen → jetzt erlaubt
h2 = spawn hough_edges(img)
Report(join(h1), join(h2))
```
Zwei Zeilen mehr als die Graph-Variante — für ~10 % des Aufwands.

### 7.2 Der Schleifenfall (der eigentlich wichtige)

**Heute:**
```vire
mut sum = 0.0
mut i = 0
while i < n { sum = sum + heavy(xs[i]); i = i + 1 }
```
oder explizit: `results = parallel_map(n, xs, |i| heavy(xs[i]))` + Summe.

**Mit dem Modell:**
```vire
sum = 0.0
for x in xs { sum = sum + heavy(x) }     // Auto-Reduktion, wenn heavy konfliktfrei
```
Zwei Bedingungen, die der Compiler *beweisen* muss und die nicht selbstverständlich sind:
`heavy` darf nichts Geteiltes schreiben (Effect-Summary), und die Reduktion muss einen
**festen Baum** benutzen — sonst ändert sich das Float-Ergebnis mit der Kernzahl, und
Vires Determinismus-Versprechen fällt. Beides machbar; beides ist Arbeit, die heute
nirgends existiert.

### 7.3 Der Fall, den der Compiler ablehnen muss

```vire
mut acc = List[Int]()
for x in xs { acc.push(transform(x)) }
```
`acc` ist eine echte Loop-Carried Dependency. Automatisch parallelisierbar **nur**, wenn
der Compiler `push` als reihenfolgeerhaltende Append-Reduktion erkennt (per-Task-Liste +
deterministischer Concat beim Join). Das ist ein Muster-Match, keine allgemeine Analyse —
und es zeigt: das Modell ist eine Sammlung erkannter Idiome, kein universeller
Mechanismus. Wer das anders verkauft, verkauft es falsch.

### 7.4 Der Fall, in dem heute schon alles richtig läuft

```vire
while k < n {
    cur.rank = (cur.prev.rank + cur.next.rank) / 2 + 1
    cur = cur.next
    k = k + 1
}
```
pagerank: strikt sequenzielle Zeigerkette, `cur.rank` wird geschrieben während
`prev.rank`/`next.rank` gelesen werden — **inhärent nicht parallelisierbar** (die
Iterationen sind semantisch abhängig). Das Graph-Modell fände hier exakt null Kanten,
würde aber unter `FASTLLVM_THREADS` atomares RC und den fehlenden Zyklen-Collector
mitbringen — für einen Benchmark, der heute bei 1.03× Rust liegt und dessen Ring ohne
Collector **leakt**. Das ist der Fall in Miniatur, warum „grundlegend" nicht geht.

---

## 8. Zahlen (Schätzung, ehrlich als Schätzung markiert)

Gegenüber Vires heutigem Modell, opt-in-Variante auf 6 P-Kernen:

| Größe | Schätzung | Bemerkung |
|---|---|---|
| Gewinn bei stark parallelisierbarem Code | **+150 … +400 %** (2.5–5×) | gegenüber *heute nicht parallelisierbarem* Code; gegenüber handgeschriebenem `parallel_map` nur **+0 … +15 %** |
| Gewinn bei normalem Code | **+0 … +8 %** | meist 0; die meisten Regionen fusionieren zurück |
| Verlust bei stark sequenziellem Code | **−1 … −4 %** (opt-in) / **−20 … −60 %** (als Default, wegen atomarem RC + Slab-Lock) | die zweite Zahl ist der Grund für das Verdikt |
| Compiler-Komplexität | **+20 … +30 %** (opt-in) / **+60 … +90 %** (Vollmodell) | Solver-Passes dominieren |
| Runtime-Komplexität | **+40 … +80 %** (opt-in) / **+300 … +500 %** (Vollmodell) | relativ zu einer heute bewusst winzigen Runtime |
| Lohnt sich das Feature insgesamt? | **Vollmodell ~10 %** · **opt-in-Variante ~55 %** · **GPU-Graph ~85 %** | |

---

## 9. Begründung der Entscheidung

**Gegen das grundlegende Modell:**

1. Die Hardware macht Feingranular-Dataflow bereits besser und gratis (§2.1).
2. Vires Runtime bestraft Threads global: atomares RC, kein Zyklen-Collector, ein
   Slab-Mutex — das würde die 1.00×-Rust-Position und den 67/67-Heap-Floor gleichzeitig
   kosten (§2.2). Das ist kein Detail, das ist das Fundament.
3. Die nötige Alias-Präzision existiert auf Vire-IR nicht, und ohne sie liefert der
   Graph keine Kanten (§2.3).
4. Der aktuelle Engpass ist der **Allokator**, nicht fehlende Parallelität. Ein
   Dependency-Graph-Modell macht den Allokator-Engpass unter Threads *schlimmer*.
5. 35 Jahre Auto-Parallelisierung und die gesamte Dataflow-Sprachlinie sind an genau
   diesem Punkt gescheitert; die Sprachen mit Vires Zielen (Mojo, Julia) haben sich
   bewusst dagegen entschieden.
6. Opportunitätskosten: Der 2-Monats-Perf-Push hat klar bezifferte Hebel (Auto-Arena ~2×,
   Vektorisierungs-Enabler, GPU-Track). Dieses Feature würde sie alle verdrängen.

**Für die opt-in-Variante (später):**

1. Sie passt zur Philosophie und ist semantikerhaltend — Nicht-Beweisen kostet nur Speed.
2. Ihre Voraussetzungen (Thread-Escape, Per-Thread-Slabs, Effect-Summaries) sind
   **unabhängig wertvoll** und schärfen auch Region-Inferenz und die `snapshot()`-Regel.
3. **Deterministische Parallelität** — bitgleiches Ergebnis unabhängig von Kernzahl —
   wäre ein echtes Alleinstellungsmerkmal gegenüber Rust/rayon, C++/TBB und OpenMP. Das
   ist das einzige Argument in diesem Dokument, das einen belastbaren technologischen
   Vorsprung begründet. Nicht „automatisch parallel" (das kann rayon mit 11 Zeichen),
   sondern **„parallel und trotzdem reproduzierbar"**.

**Empfohlene Reihenfolge, falls die Richtung verfolgt wird:**
GPU-Dependency-Graph (Wochen, hoher Ertrag) → Send-Check zu Sharing-Klassen ausbauen
(`frozen` erlauben; macht `spawn`/`parallel_map` auf Objektgraphen überhaupt erst
benutzbar) → Thread-Escape + Per-Thread-Slabs → `EffectSummary` → *dann erst*
`par`-Scope und Auto-Parallel-Loops.

*Erstellt 2026-08-19. Belege: TODO.md, M0.3-REGION-INFERENCE-V1.md, crates/driver/src/runtime.c,
crates/vire/src/spawn.rs, crates/backend/src/lib.rs, crates/ir/src/lib.rs.*
