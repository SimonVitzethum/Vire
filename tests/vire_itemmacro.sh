#!/bin/sh
# Hygienic item macros: `macro name(P: type, n: ident, e: expr) { <items> }`
# invoked as `name!(args)`, expanding to declarations. Phase 3c of the compile-
# time programming layer — safe by construction: AST-level (no text), kind-checked
# parameters, hygienic bodies, type-checked after expansion, and duplicate
# generated names are a clear front-end error (never a silent merge).
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

# type + ident + type params: generate a newtype and an accessor per invocation.
cat > "$work/newtype.vr" <<'EOF'
macro newtype(Name: ident, Get: ident, Inner: type) {
    type Name {
        value: Inner
    }
    fn Get(x: Name) -> Inner { x.value }
}
newtype!(UserId, userId, Int)
newtype!(Score, score, Float)
fn main() {
    mut u = UserId(42)
    mut s = Score(9.5)
    print(userId(u))    // 42
    print(score(s))     // 9.5
}
EOF
check "newtype (type/ident/type params)" "42
9.5" "$work/newtype.vr"

# expr params + hygiene: the macro-local `tmp` must not capture the call site's.
cat > "$work/hygiene.vr" <<'EOF'
macro define(Fn: ident, T: type, Val: expr) {
    fn Fn() -> T {
        mut tmp = Val
        tmp
    }
}
define!(answer, Int, 6 * 7)
fn main() {
    mut tmp = 999
    print(answer())    // 42
    print(tmp)         // 999  (not captured by the macro's tmp)
}
EOF
check "expr param + hygiene" "42
999" "$work/hygiene.vr"

# A generated method (item macro producing a type WITH a method).
cat > "$work/method.vr" <<'EOF'
macro boxed(Name: ident, T: type) {
    type Name {
        value: T
        fn get(self) -> T { self.value }
    }
}
boxed!(IntBox, Int)
fn main() {
    mut b = IntBox(7)
    print(b.get())     // 7
}
EOF
check "item macro with a method" "7" "$work/method.vr"

# SAFETY: an expression where a `type` is required is rejected (no blind splice).
printf 'macro nt(N: ident, T: type) { type N { v: T } }\nnt!(Foo, 3 + 4)\nfn main(){print(1)}\n' > "$work/kind1.vr"
errck "kind check: expr for type param" "must be a type" "$work/kind1.vr"

# SAFETY: an expression where an `ident` is required is rejected.
printf 'macro nt(N: ident) { type N { v: Int } }\nnt!(1 + 2)\nfn main(){print(1)}\n' > "$work/kind2.vr"
errck "kind check: expr for ident param" "must be an identifier" "$work/kind2.vr"

# SAFETY: arity mismatch is a clear error.
printf 'macro nt(N: ident, T: type) { type N { v: T } }\nnt!(Foo)\nfn main(){print(1)}\n' > "$work/arity.vr"
errck "arity mismatch rejected" "expected 2 argument" "$work/arity.vr"

# SAFETY: duplicate generated names are a clear front-end error, not a silent merge.
printf 'macro mk(N: ident) { type N { v: Int } }\nmk!(Dup)\nmk!(Dup)\nfn main(){print(1)}\n' > "$work/dup.vr"
errck "duplicate generated type rejected" "duplicate type .Dup." "$work/dup.vr"

# Nested invocation: a macro body invokes another item macro (fixpoint expansion).
cat > "$work/nested.vr" <<'EOF'
macro field(Name: ident, T: type) {
    type Name { inner: T }
}
macro pair(A: ident, B: ident, T: type) {
    field!(A, T)
    field!(B, T)
}
pair!(Left, Right, Int)
fn main() {
    mut l = Left(1)
    mut r = Right(2)
    print(l.inner + r.inner)    // 3
}
EOF
check "nested item-macro invocation" "3" "$work/nested.vr"

# Generic type argument `T[Arg]` in a `type` parameter lands as a real type app.
cat > "$work/generic.vr" <<'EOF'
type Box[T] { value: T }
macro holder(Name: ident, T: type) {
    type Name { boxed: T }
}
holder!(IntHolder, Box[Int])
fn main() { print(1) }
EOF
if "$vire" types "$work/generic.vr" 2>/dev/null | grep -q 'field boxed: Box\[Int\]'; then
    echo "ok   generic type arg in type param"; pass=$((pass+1))
else
    echo "FAIL generic type arg in type param"; fail=$((fail+1))
fi

# A diverging (self-invoking) macro is caught by the round limit, not a hang.
cat > "$work/diverge.vr" <<'EOF'
macro loop(N: ident) {
    loop!(N)
}
loop!(X)
fn main() { print(1) }
EOF
if timeout 30 "$vire" run "$work/diverge.vr" 2>&1 | grep -q 'recursion limit'; then
    echo "ok   diverging macro caught by round limit"; pass=$((pass+1))
else
    echo "FAIL diverging macro round limit"; fail=$((fail+1))
fi

# Unknown macro invocation is rejected.
printf 'nope!(x)\nfn main(){print(1)}\n' > "$work/unknown.vr"
errck "unknown item macro rejected" "unknown item macro" "$work/unknown.vr"

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
