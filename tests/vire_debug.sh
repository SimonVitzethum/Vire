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

# --debug build embeds DWARF referencing the .vr source file.
"$vire" build "$work/crash.vr" --debug -o "$work/dw" >/dev/null 2>&1
if command -v readelf >/dev/null 2>&1; then
    if readelf --debug-dump=info "$work/dw" 2>/dev/null | grep -q 'crash\.vr'; then
        echo "ok   --debug (DWARF references crash.vr)"; pass=$((pass+1))
    else
        echo "FAIL --debug (no .vr in DWARF)"; fail=$((fail+1))
    fi
else
    echo "skip --debug DWARF (no readelf)"
fi

# --debug --backtrace: a crash address resolves to the EXACT .vr line (the out-of-
# bounds access `a[n]` is on line 4 of crash.vr — per-statement precision).
if command -v addr2line >/dev/null 2>&1; then
    "$vire" build "$work/crash.vr" --debug --backtrace -o "$work/dwbt" >/dev/null 2>&1
    bt="$("$work/dwbt" 2>&1)"
    got=""
    for a in $(echo "$bt" | grep -oE '\[0x[0-9a-f]+\]' | tr -d '[]'); do
        r="$(addr2line -e "$work/dwbt" "$a" 2>/dev/null)"
        case "$r" in *crash.vr:*) got="$r"; break;; esac
    done
    case "$got" in
        *crash.vr:4) echo "ok   addr2line (crash → crash.vr:4, exact line)"; pass=$((pass+1));;
        *crash.vr:*) echo "ok   addr2line (crash → $got, .vr line)"; pass=$((pass+1));;
        *) echo "FAIL addr2line (no .vr:line resolved)"; fail=$((fail+1));;
    esac
else
    echo "skip addr2line (not installed)"
fi

# inlinedAt: `compute` is inlined into `main`, so addr2line -i shows BOTH the
# crash line in compute (crash.vr:4) and the call site in main (crash.vr:6).
if command -v addr2line >/dev/null 2>&1; then
    "$vire" build "$work/crash.vr" --debug --backtrace -o "$work/dwi" >/dev/null 2>&1
    bti="$("$work/dwi" 2>&1)"
    chain=""
    for a in $(echo "$bti" | grep -oE '\[0x[0-9a-f]+\]' | tr -d '[]'); do
        r="$(addr2line -i -e "$work/dwi" "$a" 2>/dev/null)"
        case "$r" in *crash.vr:4*) chain="$r"; break;; esac
    done
    if echo "$chain" | grep -q 'crash.vr:4' && echo "$chain" | grep -q 'crash.vr:6'; then
        echo "ok   inlinedAt (chain shows crash.vr:4 inlined at :6)"; pass=$((pass+1))
    else
        echo "FAIL inlinedAt (no inline chain: $(echo "$chain" | tr '\n' ' '))"; fail=$((fail+1))
    fi
else
    echo "skip inlinedAt (no addr2line)"
fi

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
