#!/bin/sh
# Static reflection API — P0 of the dynamic-runtime plan (language/DYNAMIC-VIRE-PLAN.md §3).
# `type_name(x)` / `field_count(x)` / `abi_version()` resolve at compile time from the type
# graph (no runtime metadata table yet — that lands with dynamic loading, P1). In a sealed
# build a value's runtime type equals its static class, so these are exact today. This suite
# pins the values + heap balance (they allocate nothing: type_name yields an immortal Str).
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

check() {
    name="$1"; want="$2"; f="$work/$name.vr"; cat > "$f"
    if ! "$vire" build "$f" -o "$work/$name.bin" >/dev/null 2>"$work/e"; then
        echo "FAIL $name (build): $(head -1 "$work/e")"; fail=$((fail+1)); return
    fi
    out="$(FASTLLVM_HEAPSTATS=1 "$work/$name.bin" 2>&1)"
    got="$(printf '%s\n' "$out" | grep -v '^\[heap\]' | tr '\n' ',' | sed 's/,$//')"
    heap="$(printf '%s\n' "$out" | grep '^\[heap\]')"
    if [ "$got" != "$want" ]; then
        echo "FAIL $name (got '$got', want '$want')"; fail=$((fail+1)); return
    fi
    if [ -n "$heap" ] && ! printf '%s' "$heap" | grep -q '0 still live'; then
        echo "FAIL $name (heap leak: $heap)"; fail=$((fail+1)); return
    fi
    echo "ok   $name ($got)"; pass=$((pass+1))
}

# struct: name + field count
check struct "Point,3" <<'EOF'
type Point { x: Int  y: Int  z: Int }
fn main() { mut p = Point(1, 2, 3)  print(type_name(p))  print(field_count(p)) }
EOF

# recursive struct
check node "Node,2" <<'EOF'
type Node { id: Int  next: Node }
fn main() { mut n = Node(7, null)  print(type_name(n))  print(field_count(n)) }
EOF

# ABI version constant (the anchor for a future module manifest)
check abi "1" <<'EOF'
fn main() { print(abi_version()) }
EOF

# scalars have a type name and 0 fields
check scalar "Int,0,Float" <<'EOF'
fn main() { print(type_name(42))  print(field_count(42))  print(type_name(3.5)) }
EOF

# field_name / field_type by constant index (exact from the layout)
check fields "x,Int,y,Float,next,Point" <<'EOF'
type Point { x: Int  y: Float  next: Point }
fn main() {
    mut p = Point(1, 2.0, null)
    print(field_name(p, 0))  print(field_type(p, 0))
    print(field_name(p, 1))  print(field_type(p, 1))
    print(field_name(p, 2))  print(field_type(p, 2))
}
EOF

# the class threads through a reassignment (traversal keeps the type)
check traverse "Node" <<'EOF'
type Node { id: Int  next: Node }
fn main() {
    mut a = Node(1, null)
    mut b = Node(2, a)
    mut c = b.next
    print(type_name(c))
}
EOF

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
