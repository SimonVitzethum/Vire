#!/bin/sh
# Iterator adapters (fold/sum/count/each/map/filter) over ranges and lists. The
# lambda body is inlined per element into a generated counting loop — no closure
# object. Elements are Int (i64). map/filter yield a new $List (chainable).
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

check() { # name expected file
    got="$("$vire" run "$3" 2>/dev/null)"
    if [ "$got" = "$2" ]; then echo "ok   $1"; pass=$((pass+1));
    else echo "FAIL $1 (want [$2] got [$got])"; fail=$((fail+1)); fi
}

# --- Ranges -------------------------------------------------------------
cat > "$work/range.vr" <<'EOF'
fn main() {
    print((1..=5).sum())                        // 15
    print((0..5).fold(0, (acc, x) -> acc + x))  // 10
    print((1..=4).fold(1, (acc, x) -> acc * x)) // 24  (factorial)
    print((0..10).count())                      // 10
    print((0..0).sum())                         // 0   (empty)
    (1..=3).each(x -> print(x))                 // 1,2,3
}
EOF
check "range adapters" "15
10
24
10
0
1
2
3" "$work/range.vr"

# --- Lists --------------------------------------------------------------
cat > "$work/list.vr" <<'EOF'
fn main() {
    mut xs = list()
    xs.push(1); xs.push(2); xs.push(3); xs.push(4); xs.push(5)
    print(xs.sum())                          // 15
    print(xs.count())                        // 5
    print(xs.map(x -> x * x).sum())          // 55
    print(xs.filter(x -> x % 2 == 0).len())  // 2
    print(xs.filter(x -> x % 2 == 0).sum())  // 6
    print(xs.fold(0, (acc, x) -> acc + x))   // 15
}
EOF
check "list adapters" "15
5
55
2
6
15" "$work/list.vr"

# --- Chaining across both sources --------------------------------------
cat > "$work/chain.vr" <<'EOF'
fn main() {
    // range -> map -> filter -> sum
    print((1..=10).map(x -> x).filter(x -> x % 3 == 0).sum())  // 3+6+9 = 18
    // list -> map -> filter -> count
    mut xs = list()
    mut i = 1
    while i <= 6 { xs.push(i); i = i + 1 }
    print(xs.map(x -> x + 10).filter(x -> x > 13).count())     // 14,15,16 -> 3
}
EOF
check "adapter chaining" "18
3" "$work/chain.vr"

# --- Statement-bodied lambdas (braceless assignment) -------------------
cat > "$work/stmtlam.vr" <<'EOF'
fn main() {
    mut total = 0
    (1..=5).each(x -> total = total + x)   // braceless assignment body
    print(total)                            // 15
    mut prod = 1
    (1..=4).each(x -> prod *= x)            // compound assignment body
    print(prod)                             // 24
}
EOF
check "statement-bodied lambda (each)" "15
24" "$work/stmtlam.vr"

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
