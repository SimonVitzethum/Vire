#!/bin/sh
# Differential compiler fuzzer runner. Generates N matched Vire+C programs
# (tests/fuzz_gen.py), compiles both, and diffs stdout. C (clang -O2) is the
# oracle. Also checks Vire heap balance (0 live). Any mismatch/crash/leak is
# printed with the seed so it can be reproduced and minimized.
#   sh tests/fuzz.sh [N] [start-seed]
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
N="${1:-500}"; S="${2:-1}"
[ -x "$vire" ] || { echo "vire missing — cargo build --release -p vire"; exit 1; }
w="$(mktemp -d)"; fail=0; ok=0
i="$S"; end=$((S + N))
while [ "$i" -lt "$end" ]; do
    python3 "$root/tests/fuzz_gen.py" "$i" "$w/p.vr" "$w/p.c" || { echo "gen fail seed=$i"; i=$((i+1)); continue; }
    if ! "$vire" build "$w/p.vr" -o "$w/pv" >/dev/null 2>"$w/ve"; then
        echo "SEED $i: VIRE BUILD FAIL: $(head -1 "$w/ve")"; cp "$w/p.vr" "$root/fuzz_fail_$i.vr"; fail=$((fail+1)); i=$((i+1)); continue
    fi
    clang -O2 "$w/p.c" -o "$w/pc" 2>/dev/null || { i=$((i+1)); continue; }   # skip if C invalid (gen bug, not Vire)
    out_v="$(FASTLLVM_HEAPSTATS=1 "$w/pv" 2>"$w/heap")"; vst=$?
    out_c="$("$w/pc")"; cst=$?
    heap="$(grep -o '[0-9]* still live' "$w/heap" | head -1)"
    if [ "$vst" -ne 0 ]; then
        echo "SEED $i: VIRE CRASH (exit $vst)"; cp "$w/p.vr" "$root/fuzz_fail_$i.vr"; fail=$((fail+1)); i=$((i+1)); continue
    fi
    if [ "$out_v" != "$out_c" ]; then
        echo "SEED $i: MISMATCH  vire='$out_v'  c='$out_c'"; cp "$w/p.vr" "$root/fuzz_fail_$i.vr"; cp "$w/p.c" "$root/fuzz_fail_$i.c"; fail=$((fail+1)); i=$((i+1)); continue
    fi
    if [ -n "$heap" ] && [ "$heap" != "0 still live" ]; then
        echo "SEED $i: HEAP LEAK ($heap)"; cp "$w/p.vr" "$root/fuzz_fail_$i.vr"; fail=$((fail+1)); i=$((i+1)); continue
    fi
    ok=$((ok+1)); i=$((i+1))
done
rm -rf "$w"
echo "--- fuzz: $ok ok, $fail fail (seeds $S..$((end-1))) ---"
[ "$fail" -eq 0 ]
