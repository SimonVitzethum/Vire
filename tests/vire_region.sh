#!/bin/sh
# Explicit bounded pools: `region(max) { … }` (M3, OWNERSHIP-STATIC.md §2.4).
# Body allocations land in a declared pool (immortal, no RC/collector, freed en
# bloc); past the ceiling the program aborts (refuse-on-full, never silent);
# the body value must be scalar; boundary-crossing return/break/continue and
# pool-unsafe stores are E300 (fail loudly, never miscompiled).
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

check() { # name expected file — must build, run, print expected, 0-live
    got="$("$vire" run "$3" 2>/dev/null)"
    heap="$(FASTLLVM_HEAPSTATS=1 "$vire" run "$3" 2>/dev/null | grep '^\[heap\]' || true)"
    if [ "$got" = "$2" ]; then echo "ok   $1"; pass=$((pass+1));
    else echo "FAIL $1 (want [$2] got [$got])"; fail=$((fail+1)); fi
    case "$heap" in *0\ still\ live*|"") : ;; *) echo "FAIL $1 (leak: $heap)"; fail=$((fail+1));; esac
}
reject() { # name pattern file — must be rejected with pattern
    if "$vire" build "$3" -o "$work/$1.bin" >/dev/null 2>"$work/e"; then
        echo "FAIL $1 (compiled — must be rejected)"; fail=$((fail+1)); return
    fi
    if grep -q "$2" "$work/e"; then echo "ok   $1"; pass=$((pass+1));
    else echo "FAIL $1 (wrong error: $(head -1 "$work/e"))"; fail=$((fail+1)); fi
}
refuse() { # name pattern file — must build, then abort with pattern
    if ! "$vire" build "$3" -o "$work/$1.bin" >/dev/null 2>"$work/e"; then
        echo "FAIL $1 (build): $(head -1 "$work/e")"; fail=$((fail+1)); return
    fi
    out="$("$work/$1.bin" 2>&1)"; code=$?
    if [ "$code" -ne 0 ] && printf '%s' "$out" | grep -q "$2"; then echo "ok   $1"; pass=$((pass+1));
    else echo "FAIL $1 (rc=$code out=[$out], want abort [$2])"; fail=$((fail+1)); fi
}

# --- promote: list build/sum in a pool, scalar out ---------------------------
cat > "$work/sum.vr" <<'EOF'
type Node { val: Int next: Node }
fn main() {
    mut s = region(100000) {
        mut head = Node(0, null)
        mut cur = head
        mut i = 1
        while i < 100 {
            mut x = Node(i, null)
            cur.next = x
            cur = x
            i = i + 1
        }
        mut t = 0
        mut c = head
        while c != null {
            t = t + c.val
            c = c.next
        }
        t
    }
    print(s)
}
EOF
check "region list sum (4950, 0-live)" "4950" "$work/sum.vr"

# --- refuse: pool too small aborts with a named message -----------------------
cat > "$work/small.vr" <<'EOF'
type Node { val: Int next: Node }
fn main() {
    mut s = region(64) {
        mut head = Node(0, null)
        mut cur = head
        mut i = 1
        while i < 10 {
            mut x = Node(i, null)
            cur.next = x
            cur = x
            i = i + 1
        }
        head.val
    }
    print(s)
}
EOF
refuse "region overflow aborts named" "vire.region.Overflow" "$work/small.vr"

# --- E300: references must not leave the pool ---------------------------------
cat > "$work/retref.vr" <<'EOF'
type Node { val: Int next: Node }
fn main() {
    mut h = region(100000) {
        Node(1, null)
    }
    print(0)
}
EOF
reject "region ref result rejected" "E300" "$work/retref.vr"

cat > "$work/outerstore.vr" <<'EOF'
type Node { val: Int next: Node }
fn main() {
    mut g = Node(0, null)
    mut s = region(100000) {
        mut x = Node(1, null)
        g.next = x
        1
    }
    print(s)
}
EOF
reject "region outer store rejected" "E300" "$work/outerstore.vr"

cat > "$work/retinbody.vr" <<'EOF'
fn f(n: Int) -> Int {
    mut s = region(100000) {
        if n > 0 { return 7 }
        1
    }
    s
}
fn main() { print(f(1)) }
EOF
reject "region return-in-body rejected" "E300" "$work/retinbody.vr"

cat > "$work/badmax.vr" <<'EOF'
fn main() {
    mut s = region("big") {
        1
    }
    print(s)
}
EOF
reject "region non-Int ceiling rejected" "E300" "$work/badmax.vr"

echo "---"
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
