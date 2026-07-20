#!/bin/bash
# Matched benchmarks Vire vs Rust vs C++ (best-of-3, optimized). Correctness
# is also checked via output comparison.
set -e
cd "$(dirname "$0")"
export LC_ALL=C
VIRE="cargo run -q -p vire --manifest-path ../../Cargo.toml --"
T=$(mktemp -d)
PEAKRSS=/tmp/peakrss
[ -x "$PEAKRSS" ] || clang -O2 ../peakrss.c -o "$PEAKRSS"
med() { local b="$1"; shift; local best=999; for r in 1 2 3; do local s=$(date +%s.%N); "$b" "$@" >/dev/null 2>&1; local e=$(date +%s.%N); local d=$(awk "BEGIN{print $e-$s}"); best=$(awk "BEGIN{print ($d<$best)?$d:$best}"); done; echo "$best"; }
rss() { "$PEAKRSS" "$1" 2>"$T/rk" >/dev/null; awk "BEGIN{printf \"%.1f\", $(cat "$T/rk")/1024}"; }   # peak RSS in MB
printf "%-8s %9s %9s %9s  %8s %8s | %6s %6s %6s\n" "bench" "Vire" "Rust" "C++" "V/Rust" "V/C++" "RAM-V" "RAM-R" "RAM-C"
for b in arith fib struct mandelbrot btree; do
  $VIRE build -o $T/${b}_v $b.vr 2>/dev/null
  rustc -O -o $T/${b}_r $b.rs 2>/dev/null
  clang++ -O2 -march=native -o $T/${b}_c $b.cpp 2>/dev/null
  [ "$($T/${b}_v)" = "$($T/${b}_r)" ] || echo "  ! $b: Vire/Rust different output"
  vt=$(med $T/${b}_v); rt=$(med $T/${b}_r); ct=$(med $T/${b}_c)
  printf "%-8s %8.3fs %8.3fs %8.3fs  %7.2fx %7.2fx | %5sM %5sM %5sM\n" "$b" "$vt" "$rt" "$ct" "$(awk "BEGIN{print $vt/$rt}")" "$(awk "BEGIN{print $vt/$ct}")" "$(rss $T/${b}_v)" "$(rss $T/${b}_r)" "$(rss $T/${b}_c)"
done
rm -rf "$T"
