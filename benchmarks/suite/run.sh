#!/bin/sh
# Builds+measures each benchmark in Vire/Rust/C++(clang), best-of-5 time + peak RSS
# (RAM, via ../peakrss), checks output equality.
cd "$(dirname "$0")"
VIRE=../../target/release/vire
[ -x "$VIRE" ] || { cargo build --release -q --manifest-path ../../Cargo.toml -p vire; }
PEAKRSS=/tmp/peakrss
[ -x "$PEAKRSS" ] || clang -O2 ../peakrss.c -o "$PEAKRSS"
best() { m=999; for r in 1 2 3 4 5; do s=$(date +%s.%N); "$@" >/tmp/bo 2>/dev/null; e=$(date +%s.%N); d=$(echo "$e - $s"|bc); c=$(echo "$d < $m"|bc); [ "$c" = 1 ] && m=$d; done; echo $m; }
rss() { "$PEAKRSS" "$1" 2>/tmp/rss_kb >/dev/null; awk "BEGIN{printf \"%.1f\", $(cat /tmp/rss_kb)/1024}"; }
printf "%-14s %10s %10s %10s %11s | %6s %6s %6s\n" "Benchmark" "Vire" "Rust" "clang++" "Vire/clang" "RAM-V" "RAM-R" "RAM-C"
for b in bitmanip matmul nbody montecarlo sort binsearch vcall; do
  [ -f $b.vr ] || continue
  "$VIRE" build $b.vr -o /tmp/v_$b 2>/dev/null
  rustc -O -C target-cpu=native $b.rs -o /tmp/r_$b 2>/dev/null
  clang++ -O2 -march=native $b.cpp -o /tmp/c_$b 2>/dev/null
  ov=$(/tmp/v_$b 2>/dev/null); orr=$(/tmp/r_$b 2>/dev/null); oc=$(/tmp/c_$b 2>/dev/null)
  vt=$(best /tmp/v_$b); rt=$(best /tmp/r_$b); ct=$(best /tmp/c_$b)
  ratio=$(echo "scale=2; $vt / $ct" | bc)
  match="OK"; [ "$ov" = "$orr" ] && [ "$orr" = "$oc" ] || match="DIFF($ov/$orr/$oc)"
  printf "%-14s %10s %10s %10s %11s | %5sM %5sM %5sM  %s\n" "$b" "$vt" "$rt" "$ct" "${ratio}x" "$(rss /tmp/v_$b)" "$(rss /tmp/r_$b)" "$(rss /tmp/c_$b)" "$match"
done
