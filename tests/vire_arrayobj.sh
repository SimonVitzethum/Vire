#!/bin/sh
# Array-of-objects (ref-element arrays) + the loop-arena INDEX-store freshness
# relaxation — soundness in both directions, on the Vire path.
#
# The feature: `mut x: Array[Node] = array(n)` builds a ref array whose elements
# are `Node` objects. `x[i] = Node(...)` is class-checked; `x[i].field` resolves;
# and when the whole pool is iteration-fresh (built, cross-linked by index/field
# ref stores, consumed and dropped inside one loop iteration) the loop-arena frees
# it en bloc — no per-node RC, no cycle collector.
#
#   PROMOTE — a fresh pool built + consumed + dropped per iteration MUST arena
#             (`jrt_arena_push` emitted), compute the right value, 0 live.
#   DECLINE — a pool element that escapes the iteration (stored to an outer var)
#             MUST NOT arena; the value is read AFTER the loop, so a wrong promotion
#             would read freed memory.
#   REJECT  — a class-mismatched element store, or a `.field` on an array whose
#             element class is unknown, MUST be a loud compile error (never a
#             wrong-offset load/store).
#
# Every runnable case also asserts heap balance (no `[heap] … still live`).
# Run: sh tests/vire_arrayobj.sh   (needs target/release/vire).
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

# run <name> <promote|decline> <expected-output> <<vr…
run() {
    name="$1"; want_dir="$2"; want="$3"; f="$work/$name.vr"; cat > "$f"
    if ! "$vire" build "$f" -o "$work/$name.bin" >/dev/null 2>"$work/e"; then
        echo "FAIL $name (build): $(head -1 "$work/e")"; fail=$((fail+1)); return
    fi
    pushes="$("$vire" build --emit=llvm "$f" 2>/dev/null | grep -c 'call void @jrt_arena_push')"
    if [ "$want_dir" = promote ] && [ "$pushes" -lt 1 ]; then
        echo "FAIL $name (expected arena promotion, none emitted)"; fail=$((fail+1)); return
    fi
    if [ "$want_dir" = decline ] && [ "$pushes" -ne 0 ]; then
        echo "FAIL $name (arena WRONGLY emitted: $pushes — would read freed memory)"; fail=$((fail+1)); return
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
    echo "ok   $name ($want_dir, pushes=$pushes)"; pass=$((pass+1))
}

# reject <name> <substring-of-expected-error> <<vr…
reject() {
    name="$1"; want="$2"; f="$work/$name.vr"; cat > "$f"
    if "$vire" build "$f" -o "$work/$name.bin" >/dev/null 2>"$work/e"; then
        echo "FAIL $name (compiled — must be rejected)"; fail=$((fail+1)); return
    fi
    if grep -q "$want" "$work/e"; then
        echo "ok   $name (rejected: $(head -1 "$work/e" | cut -c1-60))"; pass=$((pass+1))
    else
        echo "FAIL $name (wrong error: $(head -1 "$work/e"))"; fail=$((fail+1))
    fi
}

# ── PROMOTE: a node pool held in a fresh Array[Node], cross-linked by index+field
#    ref stores, consumed and dropped per iteration → freed en bloc.
#    sum of ids 0..9 = 45, ×100000 = 4500000.
run pool_build_consume promote 4500000 <<'EOF'
type Node { id: Int  next: Node }
fn main() {
    mut sum = 0
    mut t = 0
    while t < 100000 {
        mut pool: Array[Node] = array(10)
        mut i = 0
        while i < 10 { pool[i] = Node(i, null)  i = i + 1 }
        mut j = 0
        while j < 9 { pool[j].next = pool[j + 1]  j = j + 1 }
        mut c = pool[0]
        while c != null { sum = sum + c.id  c = c.next }
        t = t + 1
    }
    print(sum)
}
EOF

# ── DECLINE: a pool element escapes to an OUTER variable → the arena must be
#    suppressed (the ref outlives the iteration). Read after the loop: last iter
#    t=99, pool[2] = 99+2 = 101.
run escape_element_outer decline 101 <<'EOF'
type Node { id: Int }
fn main() {
    mut esc: Node = Node(0)
    mut t = 0
    while t < 100 {
        mut pool: Array[Node] = array(4)
        mut i = 0
        while i < 4 { pool[i] = Node(t + i)  i = i + 1 }
        esc = pool[2]
        t = t + 1
    }
    print(esc.id)
}
EOF

# ── PROMOTE across a call: a ref array passed to callees that index it (read the
#    element's field, mutate its ref field). The pool is fresh + consumed per
#    iteration through the callees → still arena. ids 0,10,20,30 sum = 60, ×1000.
run refarray_param_calls promote 60000 <<'EOF'
type Node { id: Int  next: Node }
fn link(a: Array[Node], n: Int) {
    mut j = 0
    while j < n - 1 { a[j].next = a[j + 1]  j = j + 1 }
}
fn total(a: Array[Node]) -> Int {
    mut s = 0
    mut c = a[0]
    while c != null { s = s + c.id  c = c.next }
    s
}
fn main() {
    mut sum = 0
    mut t = 0
    while t < 1000 {
        mut pool: Array[Node] = array(4)
        mut i = 0
        while i < 4 { pool[i] = Node(i * 10, null)  i = i + 1 }
        link(pool, 4)
        sum = sum + total(pool)
        t = t + 1
    }
    print(sum)
}
EOF

# ── REJECT: a class-mismatched element store inside a callee (the param carries
#    the declared element class across the call).
reject reject_param_mismatch "array element is \`A\`, stored value is \`B\`" <<'EOF'
type A { x: Int }
type B { y: Int }
fn put(a: Array[A]) { a[0] = B(1) }
fn main() { mut a: Array[A] = array(2)  put(a)  print(0) }
EOF

# ── REJECT: a class-mismatched element store.
reject reject_class_mismatch "array element is \`A\`, stored value is \`B\`" <<'EOF'
type A { x: Int }
type B { y: Int }
fn main() {
    mut a: Array[A] = array(2)
    a[0] = B(5)
    print(0)
}
EOF

# ── REJECT: `.field` on an array whose element class is unknown (no annotation) —
#    inference must not silently guess a layout.
reject reject_unknown_elem "type of the object unknown" <<'EOF'
type Node { id: Int }
fn main() {
    mut a = array(2)
    a[0] = Node(7)
    print(a[0].id)
}
EOF

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
