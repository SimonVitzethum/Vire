#!/bin/sh
# `vire fmt` — canonical AST→source printer, used as parser-fuzz insurance.
# For each program we check BOTH invariants:
#   (1) idempotency:      fmt(fmt(src)) == fmt(src)
#   (2) round-trip run:   running fmt(src) prints exactly what running src prints
# A parser/printer mismatch breaks one of the two.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

rt() { # name file  — check idempotency + round-trip run equality
    name="$1"; f="$2"
    f1="$work/$name.1.vr"; f2="$work/$name.2.vr"
    if ! "$vire" fmt "$f" > "$f1" 2>"$work/e"; then
        echo "FAIL $name (fmt: $(head -1 "$work/e"))"; fail=$((fail+1)); return
    fi
    "$vire" fmt "$f1" > "$f2" 2>/dev/null
    if ! cmp -s "$f1" "$f2"; then
        echo "FAIL $name (not idempotent)"; fail=$((fail+1)); return
    fi
    want="$("$vire" run "$f" 2>/dev/null)"
    got="$("$vire" run "$f1" 2>/dev/null)"
    if [ "$want" != "$got" ]; then
        echo "FAIL $name (round-trip run: want [$want] got [$got])"; fail=$((fail+1)); return
    fi
    echo "ok   $name"; pass=$((pass+1))
}

# --- diverse constructs: sum types, match+guard, if/elif/else, loops, strings,
#     interpolation, generics, methods, lambdas, comprehensions, casts ---------
cat > "$work/a.vr" <<'EOF'
type Shape {
    Circle(radius: Float)
    Rect(w: Float, h: Float)
}
fn area(s: Shape) -> Float {
    match s {
        Circle(r) -> r * r * 3
        Rect(w, h) -> w * h
    }
}
fn classify(x: Int) -> Str {
    if x > 10 { "big" } elif x > 0 { "small" } else { "neg" }
}
fn main() {
    mut shapes = [area(Circle(2.0)), area(Rect(3.0, 4.0))]
    mut total = 0.0
    for a in shapes { total = total + a }
    print("total {total} over {shapes.len()}")
    print(classify(5))
}
EOF
rt sumtype_match_interp "$work/a.vr"

cat > "$work/b.vr" <<'EOF'
type Box[T] { value: T
    fn get(self) -> T { self.value }
}
fn twice(f: Int) -> Int { f + f }
fn main() {
    mut b = Box(21)
    mut xs = [n * n for n in 0..5 if n % 2 == 0]
    mut sum = 0
    for x in xs { sum = sum + x }
    print(b.get() + twice(0) + sum)
    mut g = x -> x + 1
    print(g(41))
}
EOF
rt generics_comprehension_lambda "$work/b.vr"

cat > "$work/c.vr" <<'EOF'
fn main() {
    mut i = 0
    mut acc = 0
    while i < 10 {
        if i == 5 { i = i + 1  continue }
        if i == 8 { break }
        acc = acc + i
        i = i + 1
    }
    print(acc)
    mut f = 3.5 as Int
    print(f + (1 - 2) * 3)
    print("quote: \" tab:\t brace: {{x}}")
}
EOF
rt loops_break_cast_escapes "$work/c.vr"

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
