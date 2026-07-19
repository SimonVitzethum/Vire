#!/bin/sh
# Vire heap-balance + correctness suite. Each case must (a) print the expected value
# and (b) end with 0 live objects (the soundness oracle: RC/arena/collector balanced).
# This is the Vire-side analogue of the Java heap oracle in run.sh. Covers the
# auto-arena (escape→arena) promotion paths in particular, since a wrong promotion
# would surface as a leak (>0 live) or a use-after-free (crash/wrong output).
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

# case <name> <expected-output> <<vr...   — build, run with heapstats, check value + 0-live.
case_() {
    name="$1"; want="$2"; f="$work/$name.vr"; cat > "$f"
    if ! "$vire" build "$f" -o "$work/$name.bin" >/dev/null 2>"$work/e"; then
        echo "FAIL $name (build): $(head -1 "$work/e")"; fail=$((fail+1)); return
    fi
    out="$(FASTLLVM_HEAPSTATS=1 "$work/$name.bin" 2>&1)"
    val="$(printf '%s\n' "$out" | grep -v '^\[heap\]' | head -1)"
    heap="$(printf '%s\n' "$out" | grep '^\[heap\]')"
    if [ "$val" != "$want" ]; then
        echo "FAIL $name (got '$val', want '$want')"; fail=$((fail+1)); return
    fi
    if [ -n "$heap" ] && ! printf '%s' "$heap" | grep -q '0 still live'; then
        echo "FAIL $name (heap leak: $heap)"; fail=$((fail+1)); return
    fi
    echo "ok   $name"; pass=$((pass+1))
}

# --- for-loop auto-arena: non-escaping array, scalar stores → promoted, must stay balanced ---
case_ for_array_arena 1499998500000 <<'EOF'
fn work(n: Int) -> Int {
    mut acc = 0
    for i in 0..n {
        mut a = array(8)
        a[0] = i
        a[3] = i * 2
        acc = acc + a[0] + a[3]
    }
    acc
}
fn main() { print(work(1000000)) }
EOF

# --- for-loop auto-arena: non-escaping objects ---
case_ for_object_arena 900000000000000 <<'EOF'
type P { x: Int y: Int }
fn work(n: Int) -> Int {
    mut acc = 0
    for i in 0..n { mut p = P(i, i + 1)  acc = acc + p.x + p.y }
    acc
}
fn main() { print(work(30000000)) }
EOF

# --- ESCAPE guard: the array escapes (kept across iterations) → must NOT be arena'd,
#     and must stay balanced (RC frees it). Wrong promotion would UAF or leak. ---
case_ escape_no_arena 19 <<'EOF'
fn work(n: Int) -> Int {
    mut keep = array(1)
    keep[0] = 0
    for i in 0..n { mut a = array(2)  a[0] = i  a[1] = i  keep = a }
    keep[0] + keep[1] + 1
}
fn main() { print(work(10)) }
EOF

# --- stack-promoted fixed-size array (nested loops): non-escaping const array →
#     alloca, no heap allocation; must still compute correctly + stay balanced. ---
case_ stack_array_nested 3200002240000000 <<'EOF'
fn work(n: Int) -> Int {
    mut acc = 0
    for i in 0..n {
        mut a = array(16)
        for j in 0..16 { a[j] = i + j }
        for j in 0..16 { acc = acc + a[j] }
    }
    acc
}
fn main() { print(work(20000000)) }
EOF

# --- ESCAPE guard for stack arrays: an array returned from its function MUST stay
#     on the heap (stack promotion there would be a use-after-return). ---
case_ stack_array_escape_return 42 <<'EOF'
fn make(k: Int) -> array { mut a = array(4)  a[0] = k  a }
fn main() { mut x = make(42)  print(42) }
EOF

# --- while-loop arena still balanced (regression for the pre-existing path) ---
case_ while_arena 500000500000 <<'EOF'
fn work(n: Int) -> Int {
    mut acc = 0
    mut i = 0
    while i < n {
        mut a = array(4)
        a[0] = i + 1
        acc = acc + a[0]
        i = i + 1
    }
    acc
}
fn main() { print(work(1000000)) }
EOF

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
