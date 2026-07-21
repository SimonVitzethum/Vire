#!/bin/sh
# GPU-track sweep: run the `heavy` kernel on GPU vs CPU across arithmetic
# intensities, printing wall-clock time and confirming the checksums match
# (integer math → bit-exact GPU-vs-CPU). See README.md.
set -u
root="$(cd "$(dirname "$0")/../.." && pwd)"
vire="$root/target/release/vire"
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }
if [ ! -e /dev/nvidia0 ] || ! command -v llc >/dev/null 2>&1 || ! llc --version 2>/dev/null | grep -q nvptx64; then
    echo "SKIP: no NVIDIA GPU (/dev/nvidia0) or no NVPTX-capable llc"; exit 0
fi
work="$(mktemp -d)"
run() { s=$(date +%s.%N); "$1" >/dev/null; e=$(date +%s.%N); echo "$e - $s" | bc; }

build() { # <src> <iters> <out>
    sed "s/k < 2000/k < $2/" "$1" > "$work/t.vr"
    "$vire" build "$work/t.vr" -o "$3" 2>/dev/null
}

# Warm the GPU once (first launch pays context + JIT).
build "$root/benchmarks/gpu/heavy_gpu.vr" 2000 "$work/warm" && "$work/warm" >/dev/null 2>&1

printf '%-10s %-6s %-10s %-10s %s\n' "K" "match" "GPU(s)" "CPU(s)" "speedup"
for K in 2000 20000 100000 400000; do
    build "$root/benchmarks/gpu/heavy_gpu.vr" "$K" "$work/g"
    build "$root/benchmarks/gpu/heavy_cpu.vr" "$K" "$work/c"
    gsum=$("$work/g"); csum=$("$work/c")
    m=$([ "$gsum" = "$csum" ] && echo yes || echo NO)
    gt=$(run "$work/g"); ct=$(run "$work/c")
    sp=$(echo "scale=2; $ct / $gt" | bc)
    printf '%-10s %-6s %-10s %-10s %sx\n' "$K" "$m" "$gt" "$ct" "$sp"
done
