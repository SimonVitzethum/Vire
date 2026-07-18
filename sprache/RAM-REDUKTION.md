# RAM-Verbrauch — Messung + Reduktionsplan

*Nutzerwunsch: „miss den RAM-Verbrauch und schaue nach Möglichkeiten ihn zu
reduzieren." Besonders relevant fürs seL4-Ziel (knapper Speicher).*

## Messung (MaxRSS, getrusage-Wrapper)
| Workload | Vire | Vergleich |
|---|---|---|
| pagerank 262144 (262144 Nodes lebendig) | **24,4 MB** | Rust (flache vecs) 8,0 MB → **3×** |
| esc (100000-Node-Liste lebendig) | 7,1 MB | — |
| binary-trees (calloc/free) | 7,9 MB | auto-arena 7,7 MB (Working-Set klein) |

## Wohin der RAM geht — zwei Beiträge
Node = `{next, prev, rank}` = 24 B Nutzdaten. Vire legt davor den **jrt-Header**:
`{ int64_t refcount; int64_t rcflags; void *vtable }` = **24 B**. Also 48 B/Objekt —
der Header **verdoppelt** die Objektgröße. Rust: 3 flache `i64`-Vektoren, 24 B/Knoten,
KEIN Header.

Dazu der **glibc-malloc-Overhead** (gemessen mit `malloc_usable_size`):
- 48 B angefragt → **56 B** belegt (8 B Rundung/Bookkeeping),
- 40 B angefragt → **40 B** (exakt, keine Rundung),
- 24 B → 24 B.

→ Ein 48-B-Objekt kostet real **56 B**. Ein 40-B-Objekt kostet **40 B**.

## Der Hebel: Header 24 B → 16 B (rcflags in refcount packen)
`rcflags` nutzt **nur 3 Bits** (Farbe Bit 0-1, buffered Bit 2 — Bacon-Rajan-
Collector), belegt aber ein volles 8-B-Wort. Packt man diese 3 Bits in das
`refcount`-Wort, schrumpft der Header auf **16 B** → Node **48→40 B**, und dank der
malloc-Größenklasse **56→40 B real = −29 %/Objekt**.

**pagerank-Hochrechnung:** 262144 × 56 B = 14,7 MB → 262144 × 40 B = 10,5 MB;
RSS ~24,4 → ~20 MB (**−17 % gesamt, −28 % Objektspeicher**). Beim seL4-Ziel direkt
spürbar.

### Encoding (ausgearbeitet, sound)
Ein einziges `int64_t rc`-Wort:
- **Bits 0-1:** Farbe, **Bit 2:** buffered (wie bisher `rcflags & 7`).
- **Bits 3-62:** Referenzzähler (bis 2^60 — praktisch unbegrenzt).
- **Bit 63 / `rc < 0`:** immortal (Stack/Literale) — unverändert der Schnelltest.
- `retain`: `rc += 8`; `release`: `rc -= 8`, dann Null-Test `(rc >> 3) == 0`
  (äquivalent `rc < 8 && rc >= 0`).
- `COLOR(h) = rc & 3`, `BUFFERED(h) = (rc >> 2) & 1` — unverändert billig.
- `jrt_alloc`: refcount=1 → `rc = 8`; immortal → `rc = -1`.

Der immortal-Schnelltest (`rc < 0`), der in retain/release/Collector den Hot-Path
bildet, bleibt identisch. retain/release werden von `++/--` zu `+=8/-=8` — gleiche
Kosten. Der Null-Test wird `>>3`.

### Betroffene Stellen (koordinierter Umbau, ~40 Sites)
- **Backend:** `HEADER_SLOTS 3→2`, `VTABLE_WORD 2→1`; Struct-Emission
  `{i64,i64,ptr,…}` → `{i64,ptr,…}` (Klassen, `%arr.int/ref`, `@jstr.*`,
  `@jclass*`, String-Konstanten); Metadaten-Offsets (typedesc/name 24/32 → 16/24).
- **Runtime:** 11 Header-Struct-Defs `{refcount, rcflags, vtable}` → `{rc, vtable}`;
  RC-Makros (COLOR/SET_COLOR/BUFFERED); `jrt_retain/release` (+=8/-=8, Null-Test);
  Collector (Farb-Ops lesen/schreiben `rc`); `jrt_alloc` (rc=8/-1); Array-/String-/
  Boxing-/SB-Header.

### Risiko + Validierung
Memory-safety-kritisch (GC-Hot-Path + alle Layouts). **Soundness-Oracle: die
Java-Regressionssuite prüft Heap-Bilanz = 0 live** — jeder RC-/Layout-Fehler
schlägt dort durch. Zusätzlich Vire-Suite + Benchmark-Korrektheit + `HEAPSTATS`.
Deshalb: **als fokussierter, bewusst ausgeführter Schritt** umsetzen (nicht im
Multi-Topic-Turn überstürzen) — dieselbe Gate-Disziplin wie bei der Arena.

## Bereits wirksame RAM-Hebel (gebaut)
- **Auto-Arena** (escape→arena, `ESCAPE-ARENA.md`): allok-lastige `while`-Schleifen
  nutzen Bump-Allokation statt malloc-pro-Knoten → kein malloc-Rundungs-Overhead,
  en-bloc-Freigabe. RAM-Working-Set der Iteration statt Gesamtsumme.
- **Immortal-Objekte** (Stack/Literale, refcount=-1): keine RC-Buchhaltung.

## Weitere Optionen (nachrangig, gemessen/geschätzt)
- **vtable-Pointer entfernen** für Typen ohne RTTI-Bedarf (kein getClass/instanceof)
  UND ohne Ref-Felder (kein drop/trace nötig): −8 B. Aber der Collector-`trace`
  braucht bei Ref-Feldern die vtable → nur für reine Skalar-Structs, layout-invasiv,
  kleiner Gewinn. **Nicht vorrangig.**
- **Pool-/Slab-Allokator** (statt calloc) für gleichgroße Objekte: eliminiert
  malloc-Bookkeeping komplett + bessere Lokalität. Größerer Umbau; die Auto-Arena
  deckt den heißen Fall schon ab. **Später.**
- **Feld-Packing** (i32 statt i64 wo Wertebereich passt): braucht Wertebereichs-
  Analyse; die IR ist heute i64-zentriert. **Später.**

## Empfehlung
Der **Header-Pack (24→16 B)** ist der klare, universelle RAM-Hebel (−28 %
Objektspeicher, trifft die malloc-Größenklasse, hilft seL4). Encoding ist
ausgearbeitet und sound; Umsetzung als bewusster fokussierter Schritt mit der
Heap-Bilanz-Suite als Oracle.
