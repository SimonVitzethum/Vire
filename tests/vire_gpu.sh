#!/bin/sh
# GPU kernel suite (Vire `@gpu` path).
#
# End-to-end guard for the `@gpu` single-source GPU feature (see
# language/GPU-KERNELS.md): a `@gpu` function is compiled to an nvptx64 LLVM
# module → PTX (`llc`) → embedded in the binary → launched at runtime via the
# CUDA Driver API (libcuda). The host CPU suite stays bit-identical and
# untouched; this is the separate GPU track.
#
# Cases:
#   saxpy    — integer y[i]=a*x[i]+y[i] on the GPU is bit-exact vs the CPU result.
#   fscale   — float(f64) x[i]=k*x[i] on the GPU (device double path).
#   badcall  — a kernel using an unsupported host op (print) is REJECTED at build
#              with a clear diagnostic (kernels are a restricted device subset).
#
# Skips cleanly when no NVIDIA GPU / CUDA toolchain is present. Needs
# target/release/vire. Run: sh tests/vire_gpu.sh
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

# Skip gracefully in environments without a usable GPU / PTX toolchain.
if [ ! -e /dev/nvidia0 ] || ! command -v llc >/dev/null 2>&1; then
    echo "SKIP vire_gpu: no NVIDIA GPU (/dev/nvidia0) or llc — GPU track not exercised here"
    exit 0
fi
if ! llc --version 2>/dev/null | grep -q nvptx64; then
    echo "SKIP vire_gpu: installed llc has no NVPTX target"
    exit 0
fi

# ok <name> <expected-multiline-output> <<vr…
ok() {
    name="$1"; want="$2"; f="$work/$name.vr"; cat > "$f"
    if ! "$vire" build "$f" -o "$work/$name.bin" >/dev/null 2>"$work/e"; then
        echo "FAIL $name (build): $(head -2 "$work/e" | tr '\n' ' ')"; fail=$((fail+1)); return
    fi
    got="$("$work/$name.bin" 2>/dev/null)"
    if [ "$got" = "$want" ]; then
        echo "ok   $name"; pass=$((pass+1))
    else
        echo "FAIL $name: got [$got] want [$want]"; fail=$((fail+1))
    fi
}

# reject <name> <substr-in-error> <<vr…  — build MUST fail and mention <substr>.
reject() {
    name="$1"; sub="$2"; f="$work/$name.vr"; cat > "$f"
    if "$vire" build "$f" -o "$work/$name.bin" >/dev/null 2>"$work/e"; then
        echo "FAIL $name (expected build rejection, but it built)"; fail=$((fail+1)); return
    fi
    if grep -q "$sub" "$work/e"; then
        echo "ok   $name"; pass=$((pass+1))
    else
        echo "FAIL $name (wrong error): $(head -2 "$work/e" | tr '\n' ' ')"; fail=$((fail+1))
    fi
}

ok saxpy "$(printf '1\n35')" <<'EOF'
@gpu
fn saxpy(i: Int, n: Int, a: Int, x: array, y: array) {
    if i < n { y[i] = a * x[i] + y[i] }
}
fn main() {
    mut x = array(1000)
    mut y = array(1000)
    mut i = 0
    while i < 1000 { x[i] = i  y[i] = 2 * i  i = i + 1 }
    saxpy(1000, 3, x, y)
    mut ok = 1
    mut j = 0
    while j < 1000 { if y[j] != 5 * j { ok = 0 } j = j + 1 }
    print(ok)
    print(y[7])
}
EOF

ok fscale "$(printf '6\n14')" <<'EOF'
@gpu
fn scale(i: Int, n: Int, k: Float, x: farray) {
    if i < n { x[i] = k * x[i] }
}
fn main() {
    mut x = farray(8)
    mut i = 0
    while i < 8 { x[i] = i  i = i + 1 }
    scale(8, 2.0, x)
    print(x[3])
    print(x[7])
}
EOF

# gpu_gsize() intrinsic still available for grid-stride kernels.
ok stride "$(printf '499500')" <<'EOF'
@gpu
fn fill(i: Int, n: Int, out: array) {
    mut j = i
    while j < n { out[j] = j  j = j + gpu_gsize() }
}
fn main() {
    mut out = array(1000)
    fill(1000, out)
    mut s = 0
    mut i = 0
    while i < 1000 { s = s + out[i]  i = i + 1 }
    print(s)
}
EOF

reject badcall "not supported on the device" <<'EOF'
@gpu
fn k(i: Int, n: Int, x: array) {
    if i < n { print(i)  x[i] = i }
}
fn main() { mut x = array(4)  k(4, x)  print(x[1]) }
EOF

echo "---"
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
