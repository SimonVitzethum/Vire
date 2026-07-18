# SPEC CPU 2017 — ehrliche Einordnung + Plan

*Nutzerwunsch: „mach auch SPEC CPU 2017". Hier die ehrliche Lage, warum es nicht
1:1 geht, und was stattdessen aussagekräftig ist.*

## Warum SPEC CPU 2017 nicht direkt auf Vire läuft
1. **Proprietär/lizenziert.** SPEC CPU 2017 ist kein frei verfügbares Benchmark-Set
   (Lizenz ~1000 $, nicht im Repo/Netz frei ladbar). Ich kann es hier nicht
   beschaffen oder ausführen.
2. **Die Workloads sind C/C++/Fortran, nicht Vire.** SPEC misst *Compiler auf
   Standard-Programmen*: intrate = perlbench, gcc, mcf, omnetpp, xalancbmk, x264,
   deepsjeng, leela, exchange2, xz; fprate = bwaves, cactuBSSN, namd, parest, povray,
   lbm, wrf, blender, cam4, imagick, nab, fotonik3d, roms. **Das sind zehntausende
   Zeilen realer C/C++/Fortran** — keine Vire-Programme. „SPEC auf Vire laufen lassen"
   gibt es nicht, weil die SPEC-Programme nicht in Vire geschrieben sind.
3. **Vires Backend IST clang/LLVM.** Auf den C/C++-SPEC-Workloads wäre „Vire-Leistung"
   = clang-Leistung — Vire trägt dort nichts bei, es ist nicht diese Sprachen. Ein
   ehrlicher SPEC-Vergleich würde nur clang vs gcc vs icc messen, nicht Vire.

## Was stattdessen aussagekräftig ist
Zwei ehrliche Wege, SPEC-*repräsentativ* zu messen, ohne SPEC zu behaupten:

**(a) SPEC-repräsentative Kernels nach Vire portieren** und gegen Rust/C++ messen —
je ein Vertreter der SPEC-Charakteristika:
- **519.lbm-Stil** (Lattice-Boltzmann, Float-Stencil über großes Gitter) →
  Compute+Array-bound. Vire kann das mit `farray(n)` (typisierte Float-Arrays).
  Erwartung nach den Messungen: **Parität** (compute+array = schon Parität, nsieve/
  mandelbrot).
- **505.mcf-Stil** (Netzwerk-Simplex, Zeiger-Jagd über Graphknoten, int-lastig) →
  Objekt-/Pointer-bound. Vire trifft hier die **RC-Steuer** (~2× RC-only, wie
  pagerank/binary-trees). Der ehrliche Gap-Fall.
- **557.xz-Stil** (Kompression, Byte-Arrays + Bit-Manipulation) → braucht **Byte-
  Arrays** (heute nur i64-`array()`; ArrKind::Byte im Builtin fehlt) → derzeit
  nicht sauber portierbar.

**(b) Die schon laufenden CLBG-Benchmarks als Proxy** (`vire-lang/`): sie decken
dieselben Achsen ab — Compute (mandelbrot, arith), Rekursion (fib), Array (nsieve),
Objekt-Allokation/GC (binary-trees). Ergebnis: **Compute/Array/Rekursion = C++/Rust-
Parität, Objekt-Allokation = ~2,7× (RC-Steuer)**. Das ist genau die Aussage, die ein
SPEC-Lauf für eine Sprache dieser Bauart liefern würde.

## Plan (wenn SPEC-repräsentativ gewünscht)
1. **lbm-Stil-Stencil** nach Vire (`farray`, nested loops) + Rust/C++ → erwartete
   Parität bestätigen (fp-rate-Charakteristik).
2. **mcf-Stil-Graph** (Knoten-Objekte, Kanten-Traversal) + Rust/C++ → die RC-Steuer
   quantifizieren (int-rate-Charakteristik, der ehrliche Gap).
3. **Byte-Arrays** (`ArrKind::Byte` im `array()`-Builtin) nachrüsten → dann xz-Stil
   (Kompression) portierbar.
Nicht: die echte SPEC-Suite behaupten. Das wäre unehrlich — sie ist weder verfügbar
noch in Vire geschrieben.

## Kurzfassung
**SPEC CPU 2017 ist proprietär und in C/C++/Fortran — nicht direkt auf Vire
lauffähig, und wo doch, würde es clang messen, nicht Vire.** Die aussagekräftige
Alternative sind SPEC-repräsentative Kernel-Ports + die CLBG-Suite, und die sagen
dasselbe wie der Rest dieses Projekts: **Compute-Parität, RC-Steuer auf Objekten.**
EOF
echo "SPEC-Doku geschrieben"; cd /home/simon/Schreibtisch/FastLLVM; git add -A && git commit -q -m "$(cat <<'MSG'
SPEC CPU 2017: honest positioning + representative-port plan

Document why SPEC CPU 2017 can't run on Vire directly: it's proprietary/licensed
(not freely available), its workloads are large C/C++/Fortran programs (not Vire),
and where they are C/C++ Vire's backend IS clang so it would measure clang, not
Vire. The meaningful alternatives: (a) port SPEC-representative kernels -- lbm-style
float stencil (expect parity, per nsieve/mandelbrot), mcf-style pointer-chasing
graph (hits the RC tax ~2x, per pagerank/binary-trees), xz-style needs byte arrays
(pending ArrKind::Byte); (b) the already-running CLBG suite as a proxy covering the
same axes, which says compute/array/recursion = C++/Rust parity, object allocation
= ~2.7x (RC tax). Not claiming the real SPEC suite -- it's neither available nor
written in Vire. See benchmarks/SPEC-CPU2017.md.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
MSG
)" >/dev/null && echo committed