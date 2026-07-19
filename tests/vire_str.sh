#!/bin/sh
# Str methods and the Int hash Set. A string receiver is a bare Ref with no
# sentinel class; known method names route to the jrt_str_* runtime. A set() is
# the map runtime with an add/contains/remove/len surface.
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

# --- String methods -------------------------------------------------------
cat > "$work/str.vr" <<'EOF'
fn main() {
    mut s = "  Hello, World  "
    mut t = s.trim()
    print(t.length())            // 12
    print(t.lower())             // hello, world
    print(t.upper())             // HELLO, WORLD
    print(t.substring(0, 5))     // Hello
    print(t.substring(7))        // World
    print(t.indexOf("World"))    // 7
    print(t.startsWith("Hell"))  // 1
    print(t.endsWith("World"))   // 1
    print(t.charAt(1))           // 101
    print(t.isEmpty())           // 0
    print(t.equals("Hello, World")) // 1
}
EOF
check "str methods" "12
hello, world
HELLO, WORLD
Hello
World
7
1
1
101
0
1" "$work/str.vr"

# Chaining: a string-returning method yields another string.
cat > "$work/chain.vr" <<'EOF'
fn main() {
    mut s = "  ABCdef  "
    print(s.trim().lower().substring(0, 3))  // abc
}
EOF
check "str chaining" "abc" "$work/chain.vr"

# A Str-typed parameter (arrives as a bare Ref) dispatches too.
cat > "$work/param.vr" <<'EOF'
fn shout(x: Str) -> Int { x.upper().length() }
fn main() { print(shout("hi there")) }  // 8
EOF
check "str param" "8" "$work/param.vr"

# --- Set ------------------------------------------------------------------
cat > "$work/set.vr" <<'EOF'
fn main() {
    mut s = set()
    s.add(10)
    s.add(20)
    s.add(10)              // duplicate ignored
    print(s.len())         // 2
    print(s.contains(20))  // 1
    print(s.contains(99))  // 0
    s.remove(20)
    print(s.contains(20))  // 0
    print(s.len())         // 1
    // dedup a stream of ints
    mut xs = list()
    xs.push(3); xs.push(3); xs.push(7); xs.push(3); xs.push(7)
    mut uniq = set()
    mut i = 0
    while i < xs.len() { uniq.add(xs.get(i)); i = i + 1 }
    print(uniq.len())      // 2
}
EOF
check "set ops" "2
1
0
0
1
2" "$work/set.vr"

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
