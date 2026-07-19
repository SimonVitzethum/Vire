#!/bin/sh
# Inline verified-C/asm test suite. Each SAFE block must build and print the expected
# value; each UNSAFE block must be REJECTED by the verification gate (no binary). Runs
# the vire compiler with the default-on gate (vendored CSolver, called as a library).
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"
pass=0
fail=0

[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

# safe <name> <expected-output> <<vr...
safe() {
    name="$1"; want="$2"; f="$work/$name.vr"; cat > "$f"
    out="$("$vire" run "$f" 2>/dev/null | grep -vE '^verify:')"
    if [ "$out" = "$want" ]; then
        echo "ok   safe/$name"; pass=$((pass+1))
    else
        echo "FAIL safe/$name (got '$out', want '$want')"; fail=$((fail+1))
    fi
}

# unsafe <name> <<vr...   — the gate must reject it (build fails, no binary).
unsafe() {
    name="$1"; f="$work/$name.vr"; bin="$work/$name.bin"; cat > "$f"
    rm -f "$bin"
    "$vire" build "$f" -o "$bin" >/dev/null 2>&1
    if [ -f "$bin" ]; then
        echo "FAIL unsafe/$name (accepted — should be rejected)"; fail=$((fail+1))
    else
        echo "ok   unsafe/$name (rejected)"; pass=$((pass+1))
    fi
}

# rejects_with <name> <needle> <<vr...  — build fails AND the error mentions <needle>.
rejects_with() {
    name="$1"; needle="$2"; f="$work/$name.vr"; cat > "$f"
    err="$("$vire" build "$f" -o "$work/$name.bin" 2>&1)"
    if echo "$err" | grep -q "$needle"; then
        echo "ok   diag/$name ($needle)"; pass=$((pass+1))
    else
        echo "FAIL diag/$name (missing '$needle')"; fail=$((fail+1))
    fi
}

# --- SAFE: scalars ---
safe scalar_c 53 <<'EOF'
fn f(x: Int, y: Int) -> Int { inline:c(x, y) { return x * 10 + y; } }
fn main() { print(f(5, 3)) }
EOF

safe scalar_asm 42 <<'EOF'
fn dbl(x: Int) -> Int { inline:asm(x) { movq %rdi, %rax
    addq %rdi, %rax
    ret } }
fn main() { print(dbl(21)) }
EOF

safe float_scalar 7 <<'EOF'
fn g(x: Float) -> Int { inline:c(x) { return (long)(x + 0.5); } }
fn main() { print(g(6.5)) }
EOF

# --- SAFE: buffer captures (int + float), length from the array ---
safe int_buffer 60 <<'EOF'
fn total(a: array) -> Int {
    inline:c(a) { long s = 0; for (long i = 0; i < a_len; i++) s += a[i]; return s; }
}
fn main() { mut xs = array(3)  xs[0]=10  xs[1]=20  xs[2]=30  print(total(xs)) }
EOF

safe float_buffer 70 <<'EOF'
fn fsum(a: farray) -> Int {
    inline:c(a) { double s = 0.0; for (long i = 0; i < a_len; i++) s += a[i]; return (long)(s*10.0); }
}
fn main() { mut xs = farray(3)  xs[0]=1.5  xs[1]=2.5  xs[2]=3.0  print(fsum(xs)) }
EOF

# --- SAFE: guarded access (bound proven by the guard) ---
safe guarded_first 11 <<'EOF'
fn first_or_zero(a: array) -> Int {
    inline:c(a) { if (a_len < 1) { return 0; } return a[0]; }
}
fn main() { mut xs = array(2)  xs[0]=11  xs[1]=22  print(first_or_zero(xs)) }
EOF

# --- SAFE: multiple captures (array + scalar together) ---
safe multi_capture 24 <<'EOF'
fn scaled_sum(a: array, k: Int) -> Int {
    inline:c(a, k) { long s = 0; for (long i = 0; i < a_len; i++) s += a[i] * k; return s; }
}
fn main() { mut xs = array(3)  xs[0]=1  xs[1]=2  xs[2]=3  print(scaled_sum(xs, 4)) }
EOF

# --- CACHE: a repeated safe block reports "cached" on the second build ---
cache_test() {
    f="$work/cache.vr"; cat > "$f" <<'EOF'
fn total(a: array) -> Int {
    inline:c(a) { long s = 0; for (long i = 0; i < a_len; i++) s += a[i]; return s; }
}
fn main() { mut xs = array(1)  xs[0]=7  print(total(xs)) }
EOF
    rm -f "$HOME/.cache/vire/verify-cache"
    first="$("$vire" build "$f" -o "$work/cache.bin" 2>&1 | grep '^verify:')"
    second="$("$vire" build "$f" -o "$work/cache.bin" 2>&1 | grep '^verify:')"
    if echo "$first" | grep -qv "cached" && echo "$second" | grep -q "cached"; then
        echo "ok   cache/repeat (2nd build cached)"; pass=$((pass+1))
    else
        echo "FAIL cache/repeat (1st='$first' 2nd='$second')"; fail=$((fail+1))
    fi
}
cache_test

# --- UNSAFE: rejected by the gate ---
unsafe stack_oob <<'EOF'
fn f(i: Int) -> Int { inline:c(i) { int a[4]; a[i] = 7; return a[0]; } }
fn main() { print(f(10)) }
EOF

unsafe buffer_off_by_one <<'EOF'
fn bad(a: array) -> Int { inline:c(a) { return a[a_len]; } }
fn main() { mut xs = array(3)  print(bad(xs)) }
EOF

unsafe unbounded_index <<'EOF'
fn rr(a: array, off: Int) -> Int { inline:c(a, off) { return a[off]; } }
fn main() { mut xs = array(3)  print(rr(xs, 99)) }
EOF

unsafe unchecked_return_deref <<'EOF'
fn f(x: Int) -> Int {
    inline:c(x) { extern long* get(long); long* p = get(x); return *p; }
}
fn main() { print(f(0)) }
EOF

# --- DIAGNOSTICS ---
rejects_with unknown_assume "unknown @assume" <<'EOF'
fn f(a: array) -> Int { inline:c(a) { //@assume: bogus "x"
    return a_len; } }
fn main() { print(0) }
EOF

rejects_with assume_narrows_obligation "no_null_deref" <<'EOF'
fn f(x: Int) -> Int {
    inline:c(x) { extern long* get(long); long* p = get(x); return *p; }
}
fn main() { print(f(0)) }
EOF

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
