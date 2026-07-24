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

# --- Phase 3a: the comptime interpreter ---------------------------------

# comptime function call, including recursion.
cat > "$work/call.vr" <<'EOF'
fn fact(n: Int) -> Int { if n <= 1 { 1 } else { n * fact(n - 1) } }
fn sq(x: Int) -> Int { x * x }
fn main() {
    print(comptime fact(5))   // 120
    print(comptime sq(9))     // 81
}
EOF
check "comptime call + recursion" "120
81" "$work/call.vr"

# comptime block with let + for accumulation, and while.
cat > "$work/block.vr" <<'EOF'
fn main() {
    print(comptime {
        mut sum = 0
        for i in 0..=10 { sum = sum + i }
        sum
    })                          // 55
    print(comptime {
        mut n = 1
        mut c = 0
        while n < 100 { n = n * 2; c = c + 1 }
        c
    })                          // 7
}
EOF
check "comptime block let/for/while" "55
7" "$work/block.vr"

# A comptime function call as a const initializer AND as an array size.
cat > "$work/csize.vr" <<'EOF'
fn fact(n: Int) -> Int { if n <= 1 { 1 } else { n * fact(n - 1) } }
const F4 = fact(4)
fn main() {
    print(F4)                       // 24
    mut a = array(comptime fact(4))
    a[F4 - 1] = 9
    print(a[23])                    // 9
}
EOF
check "comptime fn in const + array size" "24
9" "$work/csize.vr"

# An accidental infinite comptime loop is caught by the step budget, not a hang.
cat > "$work/loop.vr" <<'EOF'
fn main() {
    print(comptime {
        mut n = 0
        while n >= 0 { n = n + 1 }
        n
    })
}
EOF
if timeout 60 "$vire" run "$work/loop.vr" 2>&1 | grep -q 'step budget'; then
    echo "ok   infinite comptime loop caught by budget"; pass=$((pass+1))
else
    echo "FAIL infinite comptime loop budget"; fail=$((fail+1))
fi

# comptime assert(cond[, msg]): a compile-time check. A passing assert is a no-op.
cat > "$work/assertok.vr" <<'EOF'
const N = 8
fn sq(x: Int) -> Int = x * x
fn main() {
    comptime assert(N > 0)
    comptime assert(sq(4) == 16, "square broken")
    print(N)
}
EOF
check "comptime assert (passing = no-op)" "8" "$work/assertok.vr"

# A failing comptime assert is a compile error carrying the message.
cat > "$work/assertbad.vr" <<'EOF'
const N = 8
fn main() { comptime assert(N > 100, "N too small")  print(N) }
EOF
if "$vire" run "$work/assertbad.vr" 2>&1 | grep -q 'comptime assert failed: N too small'; then
    echo "ok   comptime assert failure is a compile error"; pass=$((pass+1))
else
    echo "FAIL comptime assert failure is a compile error"; fail=$((fail+1))
fi

# A non-constant condition is rejected (not silently passed).
cat > "$work/assertnc.vr" <<'EOF'
fn main() { mut x = 5  comptime assert(x > 0)  print(x) }
EOF
if "$vire" run "$work/assertnc.vr" 2>&1 | grep -q 'not a compile-time constant'; then
    echo "ok   comptime assert rejects non-constant"; pass=$((pass+1))
else
    echo "FAIL comptime assert rejects non-constant"; fail=$((fail+1))
fi

# @when(os): platform conditional compilation. On this host (linux/macos/unix) the
# matching same-named fn survives; the others are dropped (no duplicate-def clash).
host_os="$(uname -s)"
case "$host_os" in
    Linux)  want_plat=1 ;;
    Darwin) want_plat=3 ;;
    *)      want_plat=1 ;;
esac
cat > "$work/when.vr" <<'EOF'
@when(linux)
fn plat() -> Int = 1
@when(windows)
fn plat() -> Int = 2
@when(macos)
fn plat() -> Int = 3
fn main() { print(plat()) }
EOF
check "@when picks the host variant" "$want_plat" "$work/when.vr"

# @when(unix) keeps the item on the unix family (linux + macos).
cat > "$work/whenunix.vr" <<'EOF'
@when(unix)
fn only_unix() -> Int = 42
fn main() { print(only_unix()) }
EOF
check "@when(unix) on the unix family" "42" "$work/whenunix.vr"

# An unknown platform name is a compile error, not a silent drop.
cat > "$work/whenbad.vr" <<'EOF'
@when(atari)
fn f() -> Int = 1
fn main() { print(0) }
EOF
if "$vire" run "$work/whenbad.vr" 2>&1 | grep -q 'unknown platform'; then
    echo "ok   @when unknown platform rejected"; pass=$((pass+1))
else
    echo "FAIL @when unknown platform rejected"; fail=$((fail+1))
fi

# `comptime for` — UNROLL the body once per compile-time index (loop var → literal),
# emitting straight-line runtime statements. Range may come from a const.
cat > "$work/cfor.vr" <<'EOF'
const N = 4
fn main() {
    mut sum = 0
    comptime for i in 0..N { sum = sum + i * i }   // 0+1+4+9 = 14
    comptime for k in 1..=3 { print(k * 10) }      // 10, 20, 30
    print(sum)
}
EOF
check "comptime for unroll (const bound + inclusive range)" "10
20
30
14" "$work/cfor.vr"

# A `comptime for` over a non-constant range is a clear error (not a crash).
cat > "$work/cfor_rt.vr" <<'EOF'
fn main() { mut n = 3  comptime for i in 0..n { print(i) } }
EOF
if "$vire" run "$work/cfor_rt.vr" 2>&1 | grep -q 'not constant-foldable'; then
    echo "ok   comptime for over runtime range rejected"; pass=$((pass+1))
else
    echo "FAIL comptime for runtime range"; fail=$((fail+1))
fi

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
