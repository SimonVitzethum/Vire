#!/usr/bin/env bash
# Benchmark-Runner: FastLLVM vs Rust vs C++ (g++ -O3 -march=native), best of N.
# Kompiliert jede Sprache, prüft Ausgabe-Gleichheit, misst, druckt Tabelle.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
fj="$root/target/debug/fastjavac"
N=${N:-5}
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT

best() { local m=999 d; for _ in $(seq 1 "$N"); do
    local s e; s=$(date +%s.%N); "$@" >/dev/null 2>&1; e=$(date +%s.%N)
    d=$(echo "$e - $s" | bc); (( $(echo "$d < $m" | bc) )) && m=$d
  done; echo "$m"; }
ratio() { echo "scale=2; $1 / $2" | bc; }

# name Main   (Main = Java-Klasse; Kleinbuchstabe = rs/cpp basename)
run() {
  local name="$1" Main="$2" low; low=$(echo "$name" | tr 'A-Z' 'a-z')
  # Java → FastLLVM
  javac -d "$work" "$Main.java" 2>/dev/null || { echo "$name: javac FAIL"; return; }
  local classes; classes=$(ls "$work/$Main.class" "$work/$Main"\$*.class 2>/dev/null)
  $fj -o "$work/${low}_fl" $classes 2>/dev/null || { echo "$name: fastjavac FAIL"; return; }
  # Rust, C++
  rustc -O "$low.rs" -o "$work/${low}_rs" 2>/dev/null || echo "$name: rustc FAIL"
  g++ -O3 -march=native "$low.cpp" -o "$work/${low}_cpp" 2>/dev/null || echo "$name: g++ FAIL"
  # Korrektheit
  local of or oc; of=$("$work/${low}_fl"); or=$("$work/${low}_rs" 2>/dev/null); oc=$("$work/${low}_cpp" 2>/dev/null)
  local mark=""; [ "$of" = "$or" ] && [ "$of" = "$oc" ] || mark=" ⚠MISMATCH($of|$or|$oc)"
  # Zeiten
  local f r c; f=$(best "$work/${low}_fl"); r=$(best "$work/${low}_rs"); c=$(best "$work/${low}_cpp")
  printf "%-10s %9ss %9ss %9ss   %6sx %6sx%s\n" "$name" "$f" "$r" "$c" "$(ratio "$f" "$r")" "$(ratio "$f" "$c")" "$mark"
}

echo "Benchmark (best of $N)  FastLLVM      Rust       C++      vsRust vsC++"
run Matmul Matmul
run Mandel Mandel
run Quick  Quick
run NBody  NBody
run Trees  Trees
