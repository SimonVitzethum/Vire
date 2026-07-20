#!/bin/sh
# Complex benchmarks: multi-algorithm workloads + fair fork/join multithreading.
# Builds each in Vire / Rust / C++(clang), checks bit-identical output, best-of-5.
# The parallel ones (pmontecarlo, pmandel) use 4 threads in ALL three languages.
cd "$(dirname "$0")"
VIRE=../../target/release/vire
[ -x "$VIRE" ] || cargo build --release -q --manifest-path ../../Cargo.toml -p vire
export LC_ALL=C
best() { m=999; for r in 1 2 3 4 5; do s=$(date +%s.%N); "$@" >/dev/null 2>&1; e=$(date +%s.%N); d=$(awk "BEGIN{print $e-$s}"); awk "BEGIN{exit !($d<$m)}" && m=$d; done; echo $m; }
printf "%-13s %10s %10s %10s | %8s %8s  %s\n" Benchmark Vire Rust "C++" V/Rust V/C++ output
for b in pipeline kmeans hashmap graph matrix fft raytracer compression compiler json regex pquicksort pmontecarlo pmandel; do
  [ -f $b.vr ] || continue
  "$VIRE" build $b.vr -o /tmp/cx_$b 2>/dev/null
  rustc -O -C target-cpu=native -C llvm-args=-fp-contract=fast $b.rs -o /tmp/cx_${b}_r 2>/dev/null
  clang++ -O2 -march=native -pthread $b.cpp -o /tmp/cx_${b}_c 2>/dev/null
  ov=$(/tmp/cx_$b); orr=$(/tmp/cx_${b}_r); oc=$(/tmp/cx_${b}_c)
  match="OK"; { [ "$ov" = "$orr" ] && [ "$orr" = "$oc" ]; } || match="DIFF"
  vt=$(best /tmp/cx_$b); rt=$(best /tmp/cx_${b}_r); ct=$(best /tmp/cx_${b}_c)
  awk "BEGIN{printf \"%-13s %10.4f %10.4f %10.4f | %7.2fx %7.2fx  %s\n\",\"$b\",$vt,$rt,$ct,$vt/$rt,$vt/$ct,\"$match\"}"
done
