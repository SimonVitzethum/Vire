#!/bin/sh
# Persisted type graph (`vire types`): the source-level, structural view of a
# program's types that survives past inference — the foundation of the
# compile-time programming layer. It must preserve generics, variants, trait
# signatures, and impls (which the IR lowering erases).
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

cat > "$work/prog.vr" <<'EOF'
type Point {
    x: Int
    y: Int
}

type Box[T] {
    value: T
}

type Shape {
    Circle(radius: Float)
    Rect(w: Float, h: Float)
}

trait Show {
    fn to_str(self) -> Str
}

impl Show for Point {
    fn to_str(self) -> Str { "point" }
}

impl Point {
    fn dist(self, o: Point) -> Float { 0.0 }
}

fn add[T](a: T, b: T) -> T { a }
fn main() { print(1) }
EOF

out="$("$vire" types "$work/prog.vr" 2>/dev/null)"

want() { # description grep-pattern
    if printf '%s\n' "$out" | grep -qF "$2"; then echo "ok   $1"; pass=$((pass+1));
    else echo "FAIL $1 (missing: $2)"; fail=$((fail+1)); fi
}

want "product type Point"        "type Point"
want "field with type"           "field x: Int"
want "generic product Box[T]"    "type Box[T]"
want "generic field T"           "field value: T"
want "sum type Shape"            "enum Shape"
want "variant with payload"      "variant Circle(Float)"
want "multi-field variant"       "variant Rect(Float, Float)"
want "trait Show"                "trait Show"
want "trait method signature"    "method to_str(self) -> Str"
want "impl recorded on type"     "impl Show"
want "inherent method"           "method dist"
want "generic fn signature"      "fn add[T](a: T, b: T) -> T"
want "builtin Option preserved"  "enum Option[T] (builtin)"
want "builtin Result two params" "enum Result[T, E] (builtin)"

# A program with NO user types still yields the builtins (and no crash).
cat > "$work/empty.vr" <<'EOF'
fn main() { print(1) }
EOF
if "$vire" types "$work/empty.vr" 2>/dev/null | grep -qF "enum Option[T] (builtin)"; then
    echo "ok   empty program (builtins only)"; pass=$((pass+1))
else
    echo "FAIL empty program"; fail=$((fail+1))
fi

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
