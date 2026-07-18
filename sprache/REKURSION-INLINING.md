# Warum g++ auf fib ~1,8× schneller ist — und wie LLVM es übertreffen kann

*Nutzerfrage: „schaue warum g++ derart schneller ist und ob man das mit LLVM
übernehmen kann."*

## Der Befund (disassembliert + gemessen)
naive fib(38): **Vire 0,080 · clang++ 0,077 · g++ 0,042**. Vire == clang (beide
LLVM); g++ ist der Ausreißer. Disassemblat: **g++ inlinet die Rekursion flach in
sich selbst** (großer Stack-Frame, verschachtelte Entfaltung mehrerer Ebenen) →
jeder echte `call` berechnet mehrere fib-Werte inline → **~halb so viele Calls =
~1,8× konstanter Faktor**. clang/LLVM inlinen Rekursion per Default NICHT, und
**kein clang-Flag** aktiviert es (getestet: `-finline-functions`,
`-inline-threshold=2000/5000`, `-funroll-loops`, `-O3`, `__attribute__((const))` —
alle bleiben bei ~0,077).

## Der Hebel ist größer als g++ (via LLVM-CSE)
Manuell EINE Rekursionsebene selbst-inlined (`fib(n-1)`/`fib(n-2)` je einen Schritt
aufgefaltet), semantisch identisch, in Vire gemessen: **0,0047 s — 17× schneller
als g++.** Grund: das Inlining legt einen **doppelten Subaufruf** frei —
`fib(n-1)` und `fib(n-2)` rufen BEIDE `fib(n-3)`. LLVMs GVN/CSE mergt die beiden
identischen Calls (fib ist seiteneffektfrei → LLVM inferiert `readnone`) → **der
Branching-Faktor sinkt** (φ=1,618 → ~1,47), und das kompoundiert rekursiv über die
Frames. **g++ holt diese CSE-Branching-Reduktion NICHT** (bleibt bei 1,8×) — LLVMs
CSE ist hier stärker, sobald das Inlining die Duplikate sichtbar macht.

## Zwei getrennte Effekte (ehrlich einordnen)
1. **Call-Overhead-Halbierung (~1,8×):** gilt für JEDE rekursionslastige Funktion
   (auch ohne überlappende Teilprobleme, z.B. `check` in binary-trees). Reiner
   konstanter Faktor durch weniger `call`s.
2. **Branching-Reduktion (bis ~17× auf fib):** NUR bei **überlappenden**
   Teilproblemen (fib, naive DP), wo Inlining doppelte reine Subaufrufe freilegt,
   die LLVM-CSE mergt. Nicht bei disjunkter Baum-Rekursion.

## Kann Vire es übernehmen? JA — als Solver-Pass „Rekursions-Inlining"
Vire kann eine kleine, reine, selbst-rekursive Funktion **1–2 Ebenen in sich selbst
inlinen** (mit erhaltener Basisfall-Guard → Terminierung), dann macht LLVM den Rest:
- Call-Overhead-Halbierung fällt sofort an (~1,8× auf Rekursion, ≥5%-Schwelle klar
  überschritten).
- Bei überlappender Rekursion mergt LLVM-CSE die freigelegten Duplikate → der große
  Zusatzgewinn — GRATIS, weil LLVM fib bereits als `readnone` führt.

**Bedingungen (sound, konservativ):** nur selbst-rekursive Funktion; klein (Inline-
Budget); reiner Rumpf (keine Seiteneffekte/Allokation — sonst kein CSE + Semantik);
Basisfall bleibt als Rekursions-Boden erhalten (der inline-expandierte Rumpf ruft an
seinem Grund wieder das echte `fib`). Risiko: Code-Bloat (Tiefe begrenzen auf 1–2)
und Compile-Zeit.

## Empfehlung
Der Pass hat eine **gemessene, große Decke** (fib 0,080→0,0047; allgemein ~1,8× auf
Rekursion) und ist ein echtes „AOT tut, was der Programmierer nicht tat". Bauen als
fokussierter Schritt: **shallow self-recursive inlining** in der Vire-Absenkung
(oder als Solver-Pass auf der IR), Tiefe 1–2, nur reine kleine self-rekursive Fns.
Das ist der EINZIGE gefundene Fall, in dem Vire hinter g++ lag — und es ist
einholbar UND übertreffbar. (Priorität nach den explizit gewünschten RAM-/C++-
Punkten.)
