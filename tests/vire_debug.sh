#!/bin/sh
# Debug backtraces (--backtrace, off by default). A program that hits an uncaught
# runtime exception must, when built with --backtrace, print a native backtrace
# with symbol names; the default build must NOT (zero overhead) and must be
# otherwise identical.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

cat > "$work/crash.vr" <<'EOF'
fn compute(n: Int) -> Int {
    mut a = array(3)
    a[0] = 1
    a[n]
}
fn main() { print(compute(10)) }
EOF

# default build: an AIOOBE, but no backtrace section.
"$vire" build "$work/crash.vr" -o "$work/def" >/dev/null 2>&1
out_def="$("$work/def" 2>&1)"
if echo "$out_def" | grep -q 'ArrayIndexOutOfBounds' && ! echo "$out_def" | grep -qi 'backtrace'; then
    echo "ok   default (exception, no backtrace)"; pass=$((pass+1))
else
    echo "FAIL default (unexpected: $out_def)"; fail=$((fail+1))
fi

# --backtrace build: same exception PLUS a backtrace with a resolved symbol.
"$vire" build "$work/crash.vr" --backtrace -o "$work/dbg" >/dev/null 2>&1
out_dbg="$("$work/dbg" 2>&1)"
if echo "$out_dbg" | grep -qi 'backtrace' && echo "$out_dbg" | grep -q '+0x'; then
    echo "ok   --backtrace (prints symbolized backtrace)"; pass=$((pass+1))
else
    echo "FAIL --backtrace (no backtrace: $out_dbg)"; fail=$((fail+1))
fi

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
