#!/bin/sh
# Build + run every Vire example and check its output. Doubles as a smoke test:
# the threading examples must be deterministic (a race would fail the check).
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
    got="$("$work/$name" 2>/dev/null | grep -vE '^verify:')"
    if [ "$got" = "$want" ]; then echo "ok   $name"; pass=$((pass+1))
    else echo "FAIL $name (got '$got', want '$want')"; fail=$((fail+1)); fi
}

check threads_atomic        2000000
check threads_workers       "$(printf '250000\n10')"
check threads_mutex         300000
check threads_parallel_sum  500000500000
check threads_parallel_for  5050
check threads_channel       385
check value_generics        "$(printf '30\n285\n42')"
check generics              "$(printf '25\n12')"
check collections           "$(printf '55\n111\n2\n2\n1\n11\nHELLO, VIRE\nVire\n1\n7')"
check iterators             "$(printf '5050\n120\n385\n30\n165')"
check compile_time          "$(printf '120\n10\nVersion(1, 4)\n1\n-1\nLogin(42)\n{"Login": [42]}\n"Tick"\n7')"
check inferred              "$(printf '25\n3')"
check object_graph          "$(printf '5120\n5120')"

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
