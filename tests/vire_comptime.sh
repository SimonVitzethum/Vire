#!/bin/sh
# Compile-time evaluation pass (comptime.rs), run AFTER inference: module `const`
# declarations become compile-time values, `const` references inline to literals
# (respecting lexical shadowing), and `comptime`/`comptime if` fold on the AST
# before lowering. Phase 2 of the compile-time programming layer.
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

# const as a value + in comptime + as an array size (all previously broken).
cat > "$work/const.vr" <<'EOF'
const WIDTH = 8
fn main() {
    print(WIDTH)                 // 8
    print(comptime WIDTH * 2)    // 16
    mut a = array(WIDTH)
    a[WIDTH - 1] = 5
    print(a[7])                  // 5
}
EOF
check "const value/comptime/array-size" "8
16
5" "$work/const.vr"

# const referencing an earlier const.
cat > "$work/chain.vr" <<'EOF'
const BASE = 10
const DOUBLED = BASE * 2
fn main() { print(DOUBLED) }     // 20
EOF
check "const references const" "20" "$work/chain.vr"

# comptime if on a const bool drops the untaken branch.
cat > "$work/cif.vr" <<'EOF'
const DEBUG = false
fn main() {
    comptime if DEBUG {
        print(999)
    } else {
        print(1)
    }
    comptime if true { print(2) }
}
EOF
check "comptime if on const bool" "1
2" "$work/cif.vr"

# Lexical shadowing: a local of the same name wins over the const.
cat > "$work/shadow.vr" <<'EOF'
const N = 100
fn main() {
    print(N)         // 100  (the const)
    mut N = 7
    print(N)         // 7    (the local shadows it)
}
EOF
check "local shadows const" "100
7" "$work/shadow.vr"

# A non-constant const initializer is a compile error (not silently ignored).
cat > "$work/bad.vr" <<'EOF'
const OOPS = someRuntimeThing()
fn main() { print(1) }
EOF
if "$vire" run "$work/bad.vr" 2>&1 | grep -q 'not a compile-time constant'; then
    echo "ok   non-constant const rejected"; pass=$((pass+1))
else
    echo "FAIL non-constant const rejected"; fail=$((fail+1))
fi

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
