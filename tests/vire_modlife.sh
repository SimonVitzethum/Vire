#!/bin/sh
# Dynamic-module lifecycle: reference-counted unload — the anti-leak requirement
# (DYNAMIC-VIRE-PLAN.md P1). A loaded module is dlclose'd the instant nothing references it
# (a stale module — override replaced, or explicitly unloaded — never lingers → no
# retained-stale-function leak), and NEVER while a dyn-slot still points into it (no UAF).
# The runtime's module table has 256 slots, so a leak (never dlclose'd) would EXHAUST it
# after 256 loads and later loads would return 0 — the tests detect that as a wrong total.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }
command -v clang >/dev/null 2>&1 || { echo "clang missing — skipping"; exit 0; }
ok() { echo "ok   $1"; pass=$((pass+1)); }
no() { echo "FAIL $1"; fail=$((fail+1)); }

cat > "$work/plug.vr" <<'EOF'
fn module_main(x: Int) -> Int { x + 1 }
EOF
cat > "$work/a.vr" <<'EOF'
fn greet(x: Int) -> Int { x + 10 }
EOF
cat > "$work/b.vr" <<'EOF'
fn greet(x: Int) -> Int { x + 20 }
EOF
for m in plug a b; do
    "$vire" build --emit-module "$work/$m.vr" -o "$work/$m.so" >/dev/null 2>"$work/e" || { echo "FAIL emit $m: $(head -1 "$work/e")"; exit 1; }
done

run() { # <name> <src> <expected-out>
    name="$1"; src="$2"; want="$3"
    printf '%s\n' "$src" > "$work/$name.vr"
    if ! "$vire" build "$work/$name.vr" -o "$work/$name.bin" >/dev/null 2>"$work/e"; then
        no "$name (build): $(head -1 "$work/e")"; return
    fi
    out="$("$work/$name.bin" 2>&1 | head -1)"
    [ "$out" = "$want" ] && ok "$name ($out)" || no "$name (got '$out', want '$want')"
}

# 1000 load+unload cycles: RC unload frees the slot each time, so all 1000 loads succeed.
# A leak would exhaust the 256-entry table after 256 and drop the total below 2000.
run load_unload_1000 "fn main() {
    mut i = 0  mut ok = 0
    while i < 1000 { mut h = load_module(\"$work/plug.so\")  if h != 0 { ok = ok + module_call(h, 1)  mut u = unload_module(h) }  i = i + 1 }
    print(ok)
}" 2000

# install → replace → unload, 500 cycles. install retains the module into the slot; a second
# install releases the displaced module; unload drops the load ref. All loads must succeed
# (no leak) and the replaced override must take effect (no UAF): 500*(10+20)=15000.
run install_replace_500 "dynamic fn greet(x: Int) -> Int { x }
fn main() {
    mut i = 0  mut sum = 0
    while i < 500 {
        mut a = load_module(\"$work/a.so\")
        mut b = load_module(\"$work/b.so\")
        mut _1 = install_override(a, \"greet\")   sum = sum + greet(0)
        mut _2 = install_override(b, \"greet\")   sum = sum + greet(0)
        mut _3 = unload_module(a)   mut _4 = unload_module(b)
        i = i + 1
    }
    print(sum)
}" 15000

# A module kept alive by a slot is still callable after its LOAD ref is released (no
# premature dlclose = no UAF): install, unload the load ref, then still call the override.
run slot_keeps_alive "dynamic fn greet(x: Int) -> Int { x }
fn main() {
    mut a = load_module(\"$work/a.so\")
    mut _1 = install_override(a, \"greet\")
    mut _2 = unload_module(a)        // load ref gone, but the slot still owns it
    print(greet(7))                  // 17 — module code still mapped
}" 17

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
