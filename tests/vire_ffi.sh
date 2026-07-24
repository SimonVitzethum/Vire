#!/bin/sh
# C FFI: extern "C" declarations, embedded `native "c"` blocks, `extern "C" link`,
# and the `cstr(s)` builtin (Vire Str → NUL-terminated char* for C, via vire_cstr).
# cstr requires a Str argument — a non-Str (Int, array, classed object) is a compile error,
# since it would hand vire_cstr(JStr*) a wrong-type pointer.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }
command -v clang >/dev/null 2>&1 || command -v cc >/dev/null 2>&1 || { echo "skip vire_ffi (no C compiler)"; exit 0; }

check() { # name expected file
    got="$("$vire" run "$3" 2>/dev/null)"
    if [ "$got" = "$2" ]; then echo "ok   $1"; pass=$((pass+1));
    else echo "FAIL $1 (want [$2] got [$got])"; fail=$((fail+1)); fi
}
reject() { # name file  — the build MUST fail (soundness / type check)
    if "$vire" build "$2" -o "$work/rej" >/dev/null 2>"$work/e"; then
        echo "FAIL $1 (built, expected a compile error)"; fail=$((fail+1))
    else echo "ok   $1 (rejected: $(head -1 "$work/e"))"; pass=$((pass+1)); fi
}

# --- cstr: pass a Vire Str to C as char* --------------------------------
cat > "$work/cstr.vr" <<'EOF'
native "c" """
#include <string.h>
extern long c_strlen(const char *s) { return (long)strlen(s); }
extern long c_first(const char *s) { return (long)(unsigned char)s[0]; }
"""
extern "C" {
    fn c_strlen(p: Ptr) -> Int
    fn c_first(p: Ptr) -> Int
}
fn main() {
    mut s = "Hello, Vire!"
    mut p = cstr(s)
    print(c_strlen(p))          // 12
    print(c_first(p))           // 72 = 'H'
    mut t = "foo" + "barbar"    // runtime-built string
    print(c_strlen(cstr(t)))    // 9
}
EOF
check "cstr → char* (literal + runtime string)" "12
72
9" "$work/cstr.vr"

# --- extern "C" link "m": libm from source, no -l flag ------------------
cat > "$work/link.vr" <<'EOF'
extern "C" link "m" {
    fn cbrt(x: F64) -> F64
}
fn main() { print(cbrt(27.0)) }   // 3
EOF
check "extern link \"m\" (cbrt)" "3" "$work/link.vr"

# --- soundness: cstr on a non-Str must not compile ----------------------
cat > "$work/bad_int.vr" <<'EOF'
fn main() { mut p = cstr(123)  print(p) }
EOF
reject "cstr(Int) rejected" "$work/bad_int.vr"

cat > "$work/bad_arr.vr" <<'EOF'
fn main() { mut a = farray(4)  mut p = cstr(a)  print(p) }
EOF
reject "cstr(array) rejected" "$work/bad_arr.vr"

# --- Ptr is an opaque i64: comparing it with an integer literal (a null check)
# must type-check (was: "type conflict: object/ref vs Int" — Ptr inferred as Ref). --
cat > "$work/ptrcmp.vr" <<'EOF'
fn use_it(p: Ptr) -> Int {
    if p == 0 { return 0 - 1 }
    if p != 0 { return 1 }
    0
}
fn main() {
    print(use_it(cstr("x")))   // 1   (non-null)
    print(use_it(0))           // -1  (null)
}
EOF
check "Ptr compared with Int literal (null check)" "1
-1" "$work/ptrcmp.vr"

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
