#!/bin/sh
# @derive(...) reflection: methods synthesized from a type's structure. Phase 3b
# of the compile-time programming layer. Eq (structural ==) and Show (a T(f, …)
# string) for non-generic product types; an explicit method overrides the derive.
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
errck() { # name pattern file
    if "$vire" run "$3" 2>&1 | grep -q "$2"; then echo "ok   $1"; pass=$((pass+1));
    else echo "FAIL $1 (no '$2')"; fail=$((fail+1)); fi
}

# Eq + Show on a struct with mixed field types.
cat > "$work/mix.vr" <<'EOF'
@derive(Eq, Show)
type User {
    name: Str
    age: Int
    score: Float
}
fn main() {
    mut a = User("Ann", 30, 9.5)
    mut b = User("Ann", 30, 9.5)
    mut c = User("Bob", 30, 9.5)
    print(a.eq(b))    // 1
    print(a.eq(c))    // 0
    print(a.show())   // User(Ann, 30, 9.5)
}
EOF
check "Eq+Show mixed fields" "1
0
User(Ann, 30, 9.5)" "$work/mix.vr"

# Show on an empty struct.
cat > "$work/empty.vr" <<'EOF'
@derive(Show)
type Unit {
}
fn main() { mut u = Unit(); print(u.show()) }   // Unit()
EOF
check "Show empty struct" "Unit()" "$work/empty.vr"

# An explicit method overrides the derived one (no coherence conflict).
cat > "$work/override.vr" <<'EOF'
@derive(Show)
type P {
    x: Int
    fn show(self) -> Str { "custom" }
}
fn main() { mut p = P(1); print(p.show()) }      // custom
EOF
check "explicit method overrides derive" "custom" "$work/override.vr"

# The derived Eq is usable in ordinary control flow.
cat > "$work/use.vr" <<'EOF'
@derive(Eq)
type Pair {
    a: Int
    b: Int
}
fn main() {
    mut p = Pair(1, 2)
    mut q = Pair(1, 2)
    if p.eq(q) { print(42) } else { print(0) }   // 42
}
EOF
check "derived eq in control flow" "42" "$work/use.vr"

# Ord + Hash on a product type (numeric, Str, Float fields).
cat > "$work/ord.vr" <<'EOF'
@derive(Ord, Hash, Eq)
type Rec {
    name: Str
    score: Int
}
fn main() {
    mut a = Rec("Ann", 5)
    mut b = Rec("Ann", 9)
    mut c = Rec("Bob", 1)
    mut d = Rec("Ann", 5)
    print(a.cmp(b))               // -1  (score 5 < 9)
    print(a.cmp(c))               // -1  ("Ann" < "Bob")
    print(a.cmp(d))               // 0
    print(b.cmp(a))               // 1
    print(a.hash() == d.hash())   // 1   (equal -> equal hash)
    print(a.hash() == b.hash())   // 0
}
EOF
check "Ord + Hash (product)" "-1
-1
0
1
1
0" "$work/ord.vr"

# Eq + Show on a sum type (match on the tag; dataless + multi-field variants).
cat > "$work/sum.vr" <<'EOF'
@derive(Eq, Show)
type Shape {
    Circle(Float)
    Rect(w: Float, h: Float)
    Nothing
}
fn main() {
    mut a = Circle(2.0)
    mut b = Circle(2.0)
    mut c = Rect(3.0, 4.0)
    mut n = Nothing
    print(a.show())    // Circle(2)
    print(c.show())    // Rect(3, 4)
    print(n.show())    // Nothing
    print(a.eq(b))     // 1
    print(a.eq(c))     // 0
    print(a.eq(n))     // 0
    print(n.eq(n))     // 1
}
EOF
check "Eq + Show (sum type)" "Circle(2)
Rect(3, 4)
Nothing
1
0
0
1" "$work/sum.vr"

# Errors: genuinely unknown derive, Ord on a sum type, generic type, nested field.
printf '@derive(Json)\ntype T { x: Int }\nfn main(){print(1)}\n' > "$work/unk.vr"
errck "unknown derive rejected" "unknown derive .Json." "$work/unk.vr"

printf '@derive(Ord)\ntype S { A(Int)\n B(Int) }\nfn main(){print(1)}\n' > "$work/sumord.vr"
errck "Ord on sum type rejected" "sum type" "$work/sumord.vr"

printf '@derive(Eq)\ntype Box[T] { value: T }\nfn main(){print(1)}\n' > "$work/gen.vr"
errck "generic derive rejected" "generic type" "$work/gen.vr"

printf '@derive(Ord)\ntype Inner { v: Int }\n@derive(Ord)\ntype Outer { a: Int\n inner: Inner }\nfn main(){print(1)}\n' > "$work/nest.vr"
errck "nested-field Ord rejected" "not a scalar" "$work/nest.vr"

# The type graph reflects the declared derives.
if "$vire" types "$work/mix.vr" 2>/dev/null | grep -q 'derive Eq'; then
    echo "ok   type graph shows derives"; pass=$((pass+1))
else
    echo "FAIL type graph shows derives"; fail=$((fail+1))
fi

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
