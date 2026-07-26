#!/bin/sh
# Runtime override of a `dynamic fn` by a loaded module (DYNAMIC-VIRE-PLAN.md P1, the
# pointer-swap mechanism, scalar-only). A host `dynamic fn` dispatches through a mutable
# slot (@jrt_dynslot); `install_override(handle, "name")` writes the module's
# `vire_override_<name>` into that slot, so subsequent calls run the module's native body.
# No object crosses the boundary (scalar in/out), so no cross-module RC/arena question.
# Pins: default before install, the module body after, that a non-dynamic-fn install is a
# compile error, and 0-live heap throughout.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }
command -v clang >/dev/null 2>&1 || { echo "clang missing — skipping"; exit 0; }
ok() { echo "ok   $1"; pass=$((pass+1)); }
no() { echo "FAIL $1"; fail=$((fail+1)); }

# The override module: greet(x) = x*100  (host default is x+1).
cat > "$work/mod.vr" <<'EOF'
fn greet(x: Int) -> Int { x * 100 }
EOF
if ! "$vire" build --emit-module "$work/mod.vr" -o "$work/mod.so" >/dev/null 2>"$work/e"; then
    echo "FAIL emit-module: $(head -1 "$work/e")"; exit 1
fi
nm -D "$work/mod.so" 2>/dev/null | grep -q vire_override_greet && ok "module exports vire_override_greet" || no "module missing vire_override_greet"

# Host: default greet, then install the override and call again.
cat > "$work/host.vr" <<EOF
dynamic fn greet(x: Int) -> Int { x + 1 }
fn main() {
    print(greet(5))                          // 6 (default)
    mut h = load_module("$work/mod.so")
    if h != 0 {
        print(install_override(h, "greet"))  // 1 (installed)
        print(greet(5))                      // 500 (module override)
    } else { print(0 - 1) }
}
EOF
if ! "$vire" build "$work/host.vr" -o "$work/host.bin" >/dev/null 2>"$work/e"; then
    no "host build: $(head -1 "$work/e")"
else
    out="$(FASTLLVM_HEAPSTATS=1 "$work/host.bin" 2>&1)"
    val="$(printf '%s\n' "$out" | grep -v '^\[heap\]' | tr '\n' ',' | sed 's/,$//')"
    heap="$(printf '%s\n' "$out" | grep '^\[heap\]')"
    if [ "$val" = "6,1,500" ]; then
        if [ -z "$heap" ] || printf '%s' "$heap" | grep -q '0 still live'; then
            ok "default 6 → install → override 500 (0-live)"
        else no "override (heap leak: $heap)"; fi
    else no "override (got '$val', want '6,1,500')"; fi
fi

# install_override on a NON-dynamic fn is a compile error (the host must have declared the seam).
cat > "$work/bad.vr" <<EOF
fn greet(x: Int) -> Int { x + 1 }
fn main() { mut h = load_module("$work/mod.so")  print(install_override(h, "greet")) }
EOF
if "$vire" build "$work/bad.vr" -o "$work/bad.bin" >/dev/null 2>"$work/e"; then
    no "install_override on a non-dynamic fn should be rejected"
else
    grep -q "not a \`dynamic fn\`" "$work/e" && ok "install_override on a sealed fn is rejected" || no "wrong error: $(head -1 "$work/e")"
fi

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
