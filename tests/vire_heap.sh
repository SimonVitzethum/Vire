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

# --- function-scoped region: a non-escaping DYNAMIC array, not in a loop, is
#     bump-allocated in the per-function region and freed at return. The function
#     is called in a hot loop; must compute correctly and stay 0-live. ---
case_ region_scratch 8000112000000 <<'EOF'
fn score(seed: Int, m: Int) -> Int {
    mut buf = array(m)
    mut i = 0
    while i < m { buf[i] = seed + i  i = i + 1 }
    mut s = 0
    mut j = 0
    while j < m { s = s + buf[j]  j = j + 1 }
    s
}
fn main() {
    mut total = 0
    mut k = 0
    while k < 1000000 { total = total + score(k, 16)  k = k + 1 }
    print(total)
}
EOF

# --- ESCAPE guard for region arrays: a returned dynamic array must stay on the
#     heap (region promotion there would be a use-after-return). ---
case_ region_escape_return 55 <<'EOF'
fn make(m: Int) -> array { mut a = array(m)  a[0] = 55  a }
fn main() { mut x = make(4)  print(55) }
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

# --- Array as a function parameter: recursive quicksort over `Array[Int]`, sorting
#     in place through a ref param (was impossible: a ref param carried no ArrKind).
#     Prints 1 iff fully sorted, and must end 0-live (the array is freed). ---
case_ array_param_qsort 1 <<'EOF'
fn qsort(a: Array[Int], lo: Int, hi: Int) {
    if lo < hi {
        mut pivot = a[hi]
        mut i = lo - 1
        mut j = lo
        while j < hi {
            if a[j] <= pivot { i = i + 1  mut t = a[i]  a[i] = a[j]  a[j] = t }
            j = j + 1
        }
        mut t2 = a[i + 1]  a[i + 1] = a[hi]  a[hi] = t2
        qsort(a, lo, i)
        qsort(a, i + 2, hi)
    }
}
fn sorted(a: Array[Int], n: Int) -> Int {
    mut i = 1
    while i < n { if a[i] < a[i - 1] { return 0 }  i = i + 1 }
    1
}
fn main() {
    mut a = array(8)
    a[0]=5 a[1]=2 a[2]=8 a[3]=1 a[4]=9 a[5]=3 a[6]=7 a[7]=4
    qsort(a, 0, 7)
    print(sorted(a, 8))
}
EOF

# --- capsule deep-copy-OUT: a primitive-array result is copied out of the arena to
#     the RC heap and survives the pop; everything stays 0-live ---
case_ capsule_array_out 30 <<'EOF'
fn main() {
    mut r = capsule() {
        mut a = array(5)
        mut i = 0
        while i < 5 { a[i] = i * i  i = i + 1 }
        a
    }
    mut s = 0
    mut j = 0
    while j < 5 { s = s + r[j]  j = j + 1 }
    print(s)
}
EOF

# --- capsule deep-copy-IN: the array input is deep-copied into the arena, so a body
#     mutation must NOT touch the caller's original (containment) — 100*(1+2+3+4)=1000,
#     and the caller's xs[0] stays 1 ---
case_ capsule_array_in 1010 <<'EOF'
fn main() {
    mut xs = array(4)
    xs[0]=1 xs[1]=2 xs[2]=3 xs[3]=4
    mut total = capsule(xs) {
        mut i = 0
        while i < 4 { xs[i] = xs[i] * 100  i = i + 1 }
        mut s = 0
        mut j = 0
        while j < 4 { s = s + xs[j]  j = j + 1 }
        s
    }
    mut check = xs[0] + xs[1] + xs[2] + xs[3]
    print(total + check)
}
EOF

# --- capsule in+out: read the isolated input copy, build + return a new array ---
case_ capsule_array_io 30 <<'EOF'
fn main() {
    mut src = array(4)
    src[0]=1 src[1]=2 src[2]=3 src[3]=4
    mut out = capsule(src) {
        mut r = array(4)
        mut i = 0
        while i < 4 { r[i] = src[i] * src[i]  i = i + 1 }
        r
    }
    print(out[0] + out[1] + out[2] + out[3])
}
EOF

# --- capsule STRUCT deep-copy in+out: the input graph is copied into the arena
#     (mutating it can't reach the caller's), and a struct result is copied out to
#     the RC heap. out.x=110, out.y=40, caller p.x stays 10 → 160; 0-live ---
case_ capsule_struct_io 160 <<'EOF'
type P { x: Int  y: Int }
fn main() {
    mut p = P(10, 20)
    mut out = capsule(p) {
        p.x = p.x + 100
        mut q = P(p.x, p.y * 2)
        q
    }
    print(out.x + out.y + p.x)
}
EOF

# --- capsule struct CYCLE: a self-referential result is deep-copied out to the
#     heap; the copied cycle must be collected (0-live) and preserve the self-link ---
case_ capsule_struct_cycle 16 <<'EOF'
type Node { v: Int  next: Node }
fn main() {
    mut a = Node(7, null)
    a.next = a
    mut out = capsule(a) {
        mut b = Node(a.v + 1, null)
        b.next = b
        b
    }
    print(out.v + out.next.v)
}
EOF

# --- capsule struct SHARING: two fields alias one node → the deep copy must keep
#     them shared (one copy via the map), so a mutation shows through both ---
case_ capsule_struct_share 30 <<'EOF'
type Node { v: Int  next: Node }
type Pair { a: Node  b: Node }
fn main() {
    mut shared = Node(5, null)
    mut p = Pair(shared, shared)
    mut r = capsule(p) {
        p.a.v = p.a.v + 10
        p.a.v + p.b.v
    }
    print(r)
}
EOF

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
