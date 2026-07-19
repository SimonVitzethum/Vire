#!/bin/sh
# Typed AST (`vire infer`): the inferred type of every expression, keyed by source
# span. Phase 1 of the compile-time programming layer — per-expression types now
# survive inference as a persisted side-table. Output: `line:col<TAB>Type<TAB>snippet`.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

cat > "$work/prog.vr" <<'EOF'
fn area(w, h) -> Float {
    w * h
}
fn main() {
    x = 3.0
    y = area(x, 2.0)
    n = 40 + 2
    ok = x > y
    print(y)
}
EOF

out="$("$vire" infer "$work/prog.vr" 2>/dev/null)"

# Column 2 is the inferred type; assert an expression of each kind got it right.
has_type() { # description type snippet-substr
    if printf '%s\n' "$out" | awk -F'\t' -v t="$2" -v s="$3" '$2==t && index($3,s)>0 {found=1} END{exit !found}'; then
        echo "ok   $1"; pass=$((pass+1))
    else
        echo "FAIL $1 (no $2 expr matching '$3')"; fail=$((fail+1))
    fi
}

has_type "float arithmetic backprop"  "Float" "*"     # w * h : Float (from -> Float)
has_type "float parameter use"        "Float" "h"     # h : Float
has_type "integer literal"            "Int"   "40"    # 40 : Int
has_type "integer arithmetic"         "Int"   "+"     # 40 + 2 : Int
has_type "comparison is Bool"         "Bool"  ">"     # x > y : Bool
has_type "float literal"              "Float" "3.0"   # 3.0 : Float

# `print(...)` is a void call → Unit.
if printf '%s\n' "$out" | awk -F'\t' '$2=="Unit"{f=1} END{exit !f}'; then
    echo "ok   void call is Unit"; pass=$((pass+1))
else
    echo "FAIL void call is Unit"; fail=$((fail+1))
fi

# A wholly-annotated float program must leave NO expression Unknown.
if printf '%s\n' "$out" | awk -F'\t' '$2=="?"{f=1} END{exit f}'; then
    echo "ok   no Unknown in annotated program"; pass=$((pass+1))
else
    echo "FAIL Unknown types leaked: $(printf '%s\n' "$out" | awk -F'\t' '$2=="?"' | tr '\n' ' ')"; fail=$((fail+1))
fi

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
