# Vire-Frontend — Stand der 7 Absenkungs-Punkte

Session-Ziel: die 7 „geparst aber nicht abgesenkt"-Lücken schließen (nach FFI +
Syntax-Zucker). Stand:

| # | Punkt | Stand | Beleg |
|---|---|---|---|
| 1 | Methoden / `impl`-Blöcke | ✅ **fertig** | `Class.method`, self=Ref, `p.sum()`→70 |
| 2 | Summentypen + Pattern-`match` | ✅ **fertig** | getaggte Klasse, `match`→Tag-Kaskade; area()=300/20/0 |
| 6 | Collections (`List`) | ✅ **fertig** | List-Literal/Index/`.len()`/`for x in xs` über Arrays |
| 4 | Comprehensions | ✅ **fertig** | `[x*x for x in xs if c]` (Map+Filter), Zwei-Pass |
| 3 | Generics / Traits / Monomorph. | 🟡 **offen** | Mono-Engine nötig (siehe unten) |
| 5 | Closures / Lambdas | 🟡 **offen** | Closure-Conversion + indirekte Calls nötig |
| 7 | Option/Result + `?`, comptime/Makros | 🟡 **teils** | Option/Result nach Generics trivial; comptime groß |

Zusätzlich diese Session: **FFI (C/C++/Python)**, **Syntax-Zucker** (Skript-`main`,
mehrarg-`print`, Trailing Commas), **Map/Set-Literale** (Teil von #4) noch offen.

## Die 3 offenen — je der ehrliche Implementierungspfad
### 3. Generics / Monomorphisierung
Braucht eine **Instanz-Worklist**: generische `fn f[T]` NICHT direkt absenken;
an jeder Aufrufstelle die Typargumente aus den Argumenttypen binden, `T` im
FnDef substituieren, eine Instanz `f$Int`/`f$Point` on-demand generieren (Cache),
bis Fixpunkt. Architektur-Änderung: die Absenkung muss während des Lowerings neue
Funktionen erzeugen können (aktuell wird jede Funktion unabhängig abgesenkt).
Substitution: `Type{name:"T"}` → konkreter Typ in Param/Return/Body. Traits
(beschränkte Generics + Dispatch) sind eine weitere Schicht darüber.
**Danach trivial:** `Option[T]`/`Result[T,E]` als eingebaute Summentypen (die
Match-Maschinerie steht schon), `?` desugart zu `match … { Err(e) -> return Err(e); Ok(v) -> v }`.
Generische `List[T]` fällt auch heraus.

### 5. Closures / Lambdas
Nicht-fangende Lambdas: zu Top-Level-Funktionen liften, Funktionszeiger übergeben —
braucht **indirekte Calls** (CallVirtual-artig auf einen Funktionszeiger) und
Higher-Order-Infrastruktur (`xs.map(f)`). Fangende Closures: **Closure-Conversion**
(gefangene Variablen in ein Heap-Environment boxen, als versteckter Parameter).

### 7b. comptime / Makros
`comptime <expr>` braucht einen **Compilezeit-Evaluator** (const-folding-Interpreter
über dem AST) + Reflection-API. Hygienische Makros: AST→AST-Transformation mit
Hygiene-Kontext. Beides eigenständige Teilstücke.

## Warum hier gestoppt (statt überstürzt)
Die 4 erledigten Punkte sind sound, getestet (41 Vire-Tests), Java 65/65. Die 3
offenen brauchen je eine Architektur-Erweiterung, die — überstürzt — Unsoundness
riskiert (die Disziplin dieses ganzen Threads: lieber ehrlich messen/markieren als
Momentum). Jeder Pfad oben ist konkret; Generics ist der Hebel, der #7 (Option/
Result) mit freischaltet und daher der nächste sinnvolle Schritt.
