#!/bin/sh
# Build + run every Vire example and check its output (LLVM execution oracle).
set -u
root="$(cd "$(dirname "$0")/../.." && pwd)"
vire="$root/target/release/vire"
dir="$root/examples/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

check() {  # check <name> <expected-output>
    name="$1"; want="$2"
    if ! "$vire" build "$dir/$name.vr" -o "$work/$name" >/dev/null 2>"$work/e"; then
        echo "FAIL $name (build): $(head -1 "$work/e")"; fail=$((fail+1)); return
    fi
    got="$("$work/$name" 2>/dev/null)"
    if [ "$got" = "$want" ]; then echo "ok   $name"; pass=$((pass+1))
    else echo "FAIL $name (got '$got', want '$want')"; fail=$((fail+1)); fi
}

check generics "$(printf '25\n12')"

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
