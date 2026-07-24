#!/bin/sh
# Byte arrays for byte-scanning (grep/binary I/O in pure Vire): barray(n) (1 byte/element,
# UNSIGNED 0..255), Array[Byte] params, str_from(a,start,len) (bytes -> Str), find_byte(a,
# from,byte) (SIMD memchr), peek_u8(ptr,i) (raw unsafe Ptr load). All array access is
# bounds-checked; the runtime range primitives clamp (sound). Byte load ZERO-extends, so a
# high byte reads as its unsigned value (255), not -1.
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

# --- barray: unsigned bytes, str_from, find_byte ------------------------
cat > "$work/basic.vr" <<'EOF'
fn main() {
    mut b = barray(8)
    b[0]=72  b[1]=105  b[2]=33  b[3]=255       // "Hi!" then a high byte
    print(b[0])                                 // 72
    print(b[3])                                 // 255  (unsigned load, not -1)
    print(str_from(b, 0, 3))                    // Hi!
    print(find_byte(b, 0, 33))                  // 2   ('!')
    print(find_byte(b, 0, 88))                  // -1  (absent)
    print(find_byte(b, 3, 255))                 // 3   (from-offset)
}
EOF
check "barray + unsigned + str_from + find_byte" "72
255
Hi!
2
-1
3" "$work/basic.vr"

# --- Array[Byte] parameter: a scan function -----------------------------
cat > "$work/scan.vr" <<'EOF'
fn count_byte(a: Array[Byte], needle: Int) -> Int {
    mut c = 0
    mut i = find_byte(a, 0, needle)
    while i >= 0 { c = c + 1  i = find_byte(a, i + 1, needle) }
    c
}
fn main() {
    mut b = barray(6)
    b[0]=97 b[1]=98 b[2]=97 b[3]=97 b[4]=98 b[5]=97   // a b a a b a
    print(count_byte(b, 97))                          // 4
}
EOF
check "Array[Byte] param scan (find_byte loop)" "4" "$work/scan.vr"

# --- soundness: range primitives clamp, no OOB read ---------------------
cat > "$work/clamp.vr" <<'EOF'
fn main() {
    mut b = barray(4)
    b[0]=65 b[1]=66 b[2]=67 b[3]=68
    print(str_from(b, 2, 100).len())     // 2   (len clamped to end)
    print(str_from(b, 10, 3).len())      // 0   (start past end → empty)
    print(find_byte(b, 10, 65))          // -1  (from past end)
}
EOF
check "str_from/find_byte clamp (sound)" "2
0
-1" "$work/clamp.vr"

# --- byte<->Int interop: a U8 element IS an Int (i64), 0..255 -----------
# Storing a computed Int into a byte array, loading a byte into an i64
# context (compare/min/assign), byte load as an Int argument, byte-load
# arithmetic, and a nested index freq[needle[k]] all must width-agree.
cat > "$work/interop.vr" <<'EOF'
fn g(c: Int) -> Int { if c >= 97 and c <= 122 { return 1 } 0 }
fn minb(a: Array[Byte], n: Int) -> Int {
    mut best = 999                      // i64 local
    mut i = 0
    while i < n { v = a[i]  if v < best { best = v } i = i + 1 }
    best
}
fn nested(freq: Array[Byte], nd: Array[Byte], n: Int) -> Int {
    mut k = 0  mut best = 999
    while k < n { fq = freq[nd[k]]  if fq < best { best = fq } k = k + 1 }
    best
}
fn main() {
    mut nd = barray(3)
    mut i = 0
    while i < 3 { v = 65 + i  nd[i] = v  i = i + 1 }   // store computed Int
    print(nd[0])                                        // 65
    print(nd[2])                                        // 67
    mut a = barray(2)  a[0]=5  a[1]=9
    print(minb(a, 2))                                   // 5  (byte in i64 ctx)
    print(g(a[0]))                                       // 0  (byte as Int arg; 5 not a-z)
    mut b = a[1] + 1                                    // byte-load arithmetic
    print(b)                                             // 10
    mut freq = barray(10)  freq[5]=3  freq[9]=7
    mut keys = barray(2)  keys[0]=5  keys[1]=9
    print(nested(freq, keys, 2))                        // 3  (nested index)
}
EOF
check "byte<->Int interop (store/compare/arg/arith/nested index)" "65
67
5
0
10
3" "$work/interop.vr"

# --- peek_u8: raw byte load over a Ptr (from cstr) ----------------------
cat > "$work/peek.vr" <<'EOF'
fn main() {
    mut p = cstr("ABC")
    print(peek_u8(p, 0))    // 65
    print(peek_u8(p, 2))    // 67
}
EOF
check "peek_u8 raw Ptr load" "65
67" "$work/peek.vr"

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
