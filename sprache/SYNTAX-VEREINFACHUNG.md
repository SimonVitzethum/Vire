# Syntax leichter machen — Exploration (ohne Performance/Mächtigkeit zu verlieren)

Alle Vereinfachungen hier sind **reiner Frontend-Zucker**: sie ändern nur Lexer/
Parser, erzeugen dieselbe IR → **null Laufzeitkosten**, und sind **additiv** (keine
bestehende Fähigkeit fällt weg). Kriterium für „mächtig genug": jede muss sich auf
den vorhandenen Kern zurückführen lassen (desugaring), nicht ihn einschränken.

## Umgesetzt
1. **Skript-Stil / implizites `main`.** Top-Level-Anweisungen werden zu `fn main()`
   zusammengefasst — Python-artig, kein Boilerplate:
   ```vire
   mut s = 0
   for i in 0..10 { s = s + i }
   print(s)          // kein fn main() nötig
   ```
   `fn main` UND Top-Level-Anweisungen zugleich = Fehler (Eindeutigkeit).
2. **Mehrargumentiges `print`.** `print(a, b, "c")` gibt jedes Argument in eigener
   Zeile aus. Kein Format-String nötig für den Normalfall.
3. **Abschließende Kommas** in Aufrufen/Listen (`f(a, b,)`) — diff-freundlich.
4. **Ausdrucksfunktionen** `fn quad(x) = x * x` (war schon da, jetzt bestätigt).
5. **Zeilenkommentare `//`** und schachtelbare `/* */` (war schon da).
6. **Newline-als-Terminator** (Go-Stil, kein `;`), volle Fortsetzung nach Operatoren.
7. **Volle Typinferenz für Locals/Parameter** — Annotationen optional (skalar).

## Analysiert, bewusst (noch) NICHT umgesetzt — mit Begründung
- **String-Interpolation `"sum = {x}"`** — der größte verbleibende Ergonomiegewinn.
  Braucht Lexer-Aufspaltung in Teile + `str_concat` + Zahl→String zur Laufzeit
  (existiert im Runtime-Stringpfad). **Wert hoch, Aufwand mittel** → als nächster
  Zucker-Schritt vorgemerkt (das Design reserviert `{{` als Escape).
- **Verkettete Vergleiche `0 < x < 10`** — Python-artig; desugart zu `0<x and x<10`.
  Klein, aber Kollisionsrisiko mit generischen `[]`/Vergleichs-Lesarten → erst nach
  Interpolation.
- **Optionale Klammern bei Ein-Argument-Aufrufen** (`print x`) — BEWUSST NICHT:
  schafft Grammatik-Mehrdeutigkeit (`f -x` = Aufruf oder Subtraktion?), kostet
  Eindeutigkeit ohne echten Gewinn. Mächtigkeit ≠ weniger Klammern.
- **Signifikante Einrückung** (Python-Blöcke) — BEWUSST NICHT: der Nutzer hat sich
  früh für `{}`-Blöcke entschieden; Einrückung bringt bekannte Tooling-/Refactoring-
  Kosten ohne Ausdruckskraft-Gewinn.

## Leitlinie
Zucker ja, solange (a) er sich auf den Kern zurückführen lässt, (b) er die Grammatik
nicht mehrdeutig macht, (c) er null Laufzeitkosten hat. „Leichter" heißt weniger
Boilerplate und mehr Inferenz — nicht weniger Präzision.
