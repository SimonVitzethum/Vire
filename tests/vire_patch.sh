#!/bin/sh
# Runtime code patcher B1 (DYNAMIC-VIRE-PATCH.md) — hot-update a `@patchable fn` by
# rewriting its reserved 14-byte entry sled with an absolute `jmp` to a loaded module's
# replacement, and restore it byte-identically. Writes ONLY into the compiler-reserved sled
# listed in @jrt_patchtab (never a real instruction), W^X, i-cache flush. Anti-leak: a live
# patch reference-counts the module it points into (retain on patch, release on unpatch /
# replace), so a module a patch still uses is never dlclose'd (no UAF) and a released one is
# reclaimed (no leak).
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }
command -v clang >/dev/null 2>&1 || { echo "clang missing — skipping"; exit 0; }
ok() { echo "ok   $1"; pass=$((pass+1)); }
no() { echo "FAIL $1"; fail=$((fail+1)); }

# module: replacement `fixed(x) = x*1000` (exported as vire_override_fixed)
cat > "$work/m.vr" <<'EOF'
fn fixed(x: Int) -> Int { x * 1000 }
EOF
"$vire" build --emit-module "$work/m.vr" -o "$work/m.so" >/dev/null 2>"$work/e" || { echo "FAIL emit: $(head -1 "$work/e")"; exit 1; }

run() { # <name> <src> <expected>
    name="$1"; src="$2"; want="$3"
    printf '%s\n' "$src" > "$work/$name.vr"
    if ! "$vire" build "$work/$name.vr" -o "$work/$name.bin" >/dev/null 2>"$work/e"; then
        no "$name (build): $(head -1 "$work/e")"; return
    fi
    out="$("$work/$name.bin" 2>&1 | grep -v '^\[heap\]' | tr '\n' ',' | sed 's/,$//')"
    [ "$out" = "$want" ] && ok "$name ($out)" || no "$name (got '$out', want '$want')"
}

# patch → redirect → restore byte-identically (original behaviour returns).
run patch_restore "@patchable
fn compute(x: Int) -> Int { x + 1 }
fn main() {
    print(compute(5))                               // 6
    mut h = load_module(\"$work/m.so\")
    print(patch(\"compute\", h, \"fixed\"))          // 1
    print(compute(5))                                // 5000
    print(unpatch(\"compute\"))                      // 1
    print(compute(5))                                // 6 (restored)
}" "6,1,5000,1,6"

# anti-leak / no-UAF: a patch keeps the module alive after its load ref is released, and a
# 500× patch/unpatch loop reclaims each cycle (a leak would exhaust the 256 module table).
run patch_lifetime "@patchable
fn f(x: Int) -> Int { x + 1 }
fn main() {
    mut i = 0  mut sum = 0
    while i < 500 {
        mut h = load_module(\"$work/m.so\")
        mut _p = patch(\"f\", h, \"fixed\")
        mut _u = unload_module(h)      // load ref gone, but the patch still references it
        sum = sum + f(3)               // 3000 — module code still mapped (no UAF)
        mut _r = unpatch(\"f\")        // releases the module → dlclose (no leak)
        i = i + 1
    }
    print(sum)                          // 500*3000 = 1500000; any leak/exhaustion → less
}" "1500000"

# patch on a non-@patchable fn is a compile error (the entry has no sled).
if printf '%s\n' "fn g(x: Int) -> Int { x }
fn main() { mut h = load_module(\"$work/m.so\")  print(patch(\"g\", h, \"fixed\")) }" > "$work/bad.vr" && \
   "$vire" build "$work/bad.vr" -o "$work/bad.bin" >/dev/null 2>"$work/e"; then
    no "patch on a non-@patchable fn should be rejected"
else
    grep -q "not a \`@patchable fn\`" "$work/e" && ok "patch on a non-@patchable fn is rejected" || no "wrong error: $(head -1 "$work/e")"
fi

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
