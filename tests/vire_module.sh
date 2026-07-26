#!/bin/sh
# Dynamic module loading — M1, scalar-only (DYNAMIC-VIRE-PLAN.md P1, step 1).
# `vire --emit-module` builds a prebuilt native module (.so) exporting a scalar C-ABI
# entry + an ABI-version constant; the host loads it with `load_module(path)` (verified:
# dlopen + ABI-version check) and calls it with `module_call(handle, arg)`. Scalar in/out
# only — no object crosses the boundary — so there is no cross-module RC/arena question.
# Pins: the round-trip value, 0-live heap, and the load gate (missing / non-module /
# wrong-ABI .so → handle 0, never a crash or a wrong call).
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }
command -v clang >/dev/null 2>&1 || { echo "clang missing — skipping"; exit 0; }

ok() { echo "ok   $1"; pass=$((pass+1)); }
no() { echo "FAIL $1"; fail=$((fail+1)); }

# The module: a pure scalar computation.
cat > "$work/plugin.vr" <<'EOF'
fn module_main(x: Int) -> Int { x * x + 1 }
EOF
if ! "$vire" build --emit-module "$work/plugin.vr" -o "$work/plugin.so" >/dev/null 2>"$work/e"; then
    echo "FAIL emit-module: $(head -1 "$work/e")"; exit 1
fi
# The exported symbols must be present.
if nm -D "$work/plugin.so" 2>/dev/null | grep -q vire_module_main && \
   nm -D "$work/plugin.so" 2>/dev/null | grep -q vire_module_abi; then
    ok "module exports vire_module_main + vire_module_abi"
else
    no "module missing exported symbols"
fi

# host <name> <module-path> <expected-stdout>
host() {
    name="$1"; mod="$2"; want="$3"
    cat > "$work/$name.vr" <<EOF
fn main() {
    mut h = load_module("$mod")
    if h != 0 { print(module_call(h, 7))  print(module_call(h, 12)) } else { print(0 - 1) }
}
EOF
    if ! "$vire" build "$work/$name.vr" -o "$work/$name.bin" >/dev/null 2>"$work/e"; then
        no "$name (build): $(head -1 "$work/e")"; return
    fi
    out="$(FASTLLVM_HEAPSTATS=1 "$work/$name.bin" 2>&1)"
    val="$(printf '%s\n' "$out" | grep -v '^\[heap\]' | tr '\n' ',' | sed 's/,$//')"
    heap="$(printf '%s\n' "$out" | grep '^\[heap\]')"
    if [ "$val" != "$want" ]; then no "$name (got '$val', want '$want')"; return; fi
    if [ -n "$heap" ] && ! printf '%s' "$heap" | grep -q '0 still live'; then no "$name (heap leak: $heap)"; return; fi
    ok "$name ($val)"
}

# valid module → real values (7*7+1=50, 12*12+1=145)
host loads_and_calls "$work/plugin.so" "50,145"

# GATE 1: a missing .so → load fails → handle 0 → host prints -1 (no crash)
host missing_module "$work/does-not-exist.so" "-1"

# GATE 2: a non-module .so (no vire_module_abi) → rejected → handle 0
cat > "$work/notmod.c" <<'EOF'
int something(void) { return 1; }
EOF
clang -shared -fPIC "$work/notmod.c" -o "$work/notmod.so" 2>/dev/null
host non_module "$work/notmod.so" "-1"

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
