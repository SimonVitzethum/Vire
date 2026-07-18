#!/bin/bash
# Matched benchmarks Vire vs Rust vs C++ (best-of-3, optimized). Correctness
# is also checked via output comparison.
set -e
cd "$(dirname "$0")"
export LC_ALL=C
VIRE="cargo run -q -p vire --manifest-path ../../Cargo.toml --"
T=$(mktemp -d)
med() { local b="$1"; shift; local best=999; for r in 1 2 3; do local s=$(date +%s.%N); "$b" "$@" >/dev/null 2>&1; local e=$(date +%s.%N); local d=$(awk "BEGIN{print $e-$s}"); best=$(awk "BEGIN{print ($d<$best)?$d:$best}"); done; echo "$best"; }
printf "%-8s %9s %9s %9s  %8s %8s\n" "bench" "Vire" "Rust" "C++" "V/Rust" "V/C++"
for b in arith fib struct mandelbrot btree; do
  $VIRE build -o $T/${b}_v $b.vr 2>/dev/null
  rustc -O -o $T/${b}_r $b.rs 2>/dev/null
  clang++ -O2 -march=native -o $T/${b}_c $b.cpp 2>/dev/null
  [ "$($T/${b}_v)" = "$($T/${b}_r)" ] || echo "  ! $b: Vire/Rust different output"
  vt=$(med $T/${b}_v); rt=$(med $T/${b}_r); ct=$(med $T/${b}_c)
  printf "%-8s %8.3fs %8.3fs %8.3fs  %7.2fx %7.2fx\n" "$b" "$vt" "$rt" "$ct" "$(awk "BEGIN{print $vt/$rt}")" "$(awk "BEGIN{print $vt/$ct}")"
done
rm -rf "$T"
