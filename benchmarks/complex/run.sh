#!/bin/sh
# Complex benchmarks: multi-algorithm workloads + fair fork/join multithreading.
# Builds each in Vire / Rust / C++(clang), checks bit-identical output, then reports
# best-of-5 wall time AND peak resident memory (RSS, via ../peakrss). The parallel ones
# (pmontecarlo, pmandel, pquicksort) use 4 threads in ALL three languages.
cd "$(dirname "$0")"
VIRE=../../target/release/vire
[ -x "$VIRE" ] || cargo build --release -q --manifest-path ../../Cargo.toml -p vire
PEAKRSS=/tmp/peakrss
[ -x "$PEAKRSS" ] || clang -O2 ../peakrss.c -o "$PEAKRSS"
export LC_ALL=C
best() { m=999; for r in 1 2 3 4 5; do s=$(date +%s.%N); "$@" >/dev/null 2>&1; e=$(date +%s.%N); d=$(awk "BEGIN{print $e-$s}"); awk "BEGIN{exit !($d<$m)}" && m=$d; done; echo $m; }
rss() { "$PEAKRSS" "$1" 2>/tmp/rss_kb >/dev/null; cat /tmp/rss_kb; }   # peak RSS in KB
printf "%-12s | %8s %8s %8s %6s %6s | %6s %6s %6s\n" Benchmark Vire Rust "C++" V/R V/C RAM-V RAM-R RAM-C
for b in pipeline kmeans hashmap graph matrix fft raytracer compression compiler json regex pquicksort pmontecarlo pmandel; do
  [ -f $b.vr ] || continue
  "$VIRE" build $b.vr -o /tmp/cx_$b 2>/dev/null
  rustc -O -C target-cpu=native -C llvm-args=-fp-contract=fast $b.rs -o /tmp/cx_${b}_r 2>/dev/null
  clang++ -O2 -march=native -pthread $b.cpp -o /tmp/cx_${b}_c 2>/dev/null
  ov=$(/tmp/cx_$b); orr=$(/tmp/cx_${b}_r); oc=$(/tmp/cx_${b}_c)
  match="OK"; { [ "$ov" = "$orr" ] && [ "$orr" = "$oc" ]; } || match="DIFF"
  vt=$(best /tmp/cx_$b); rt=$(best /tmp/cx_${b}_r); ct=$(best /tmp/cx_${b}_c)
  vm=$(rss /tmp/cx_$b); rm=$(rss /tmp/cx_${b}_r); cm=$(rss /tmp/cx_${b}_c)
  awk "BEGIN{printf \"%-12s | %8.4f %8.4f %8.4f %5.2fx %5.2fx | %4dMB %4dMB %4dMB  %s\n\",\"$b\",$vt,$rt,$ct,$vt/$rt,$vt/$ct,$vm/1024,$rm/1024,$cm/1024,\"$match\"}"
done
