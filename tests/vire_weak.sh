#!/bin/sh
# `weak f: T` — the non-owning reference field (DYNAMIC-VIRE-PLAN.md §6).
#
# The point of `weak` is NOT convenience, it is the no-GC gate: RC alone cannot reclaim a
# reference cycle, and Vire refuses to answer that with a tracing collector. So a shape
# that closes a cycle must declare its back-edge `weak` — a field that is stored without
# retain, dropped without release and never traced. Then the ownership graph is acyclic
# again, the acyclicity analysis fires, and the binary links NO cycle collector at all
# while still ending 0-live. That last pair is what this suite pins: same program, same
# value, same 0-live — but `weak` removes the collector from the binary and the owning
# variant keeps it. Everything else here guards the edges of the modifier: it is
# CONTEXTUAL (a field named `weak` still parses), and it is rejected — with a readable
# message, not a syntax error — where step 2b cannot honour it (primitives, generics,
# sum-type payloads).
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }
have_nm=0; command -v nm >/dev/null 2>&1 && have_nm=1

# run <name> <want-out> <collector: yes|no> <<vr…
#   builds, runs with the heap oracle (value + 0 live) and checks whether the cycle
#   collector was linked into the binary. `no` is the interesting one: RC-only.
run() {
    name="$1"; want="$2"; want_coll="$3"; f="$work/$name.vr"; cat > "$f"
    if ! "$vire" build "$f" -o "$work/$name.bin" >/dev/null 2>"$work/e"; then
        echo "FAIL $name (build): $(head -1 "$work/e")"; fail=$((fail+1)); return
    fi
    out="$(FASTLLVM_HEAPSTATS=1 "$work/$name.bin" 2>&1)"
    val="$(printf '%s\n' "$out" | grep -v '^\[heap\]' | head -1)"
    heap="$(printf '%s\n' "$out" | grep '^\[heap\]')"
    if [ "$val" != "$want" ]; then
        echo "FAIL $name (got '$val', want '$want')"; fail=$((fail+1)); return; fi
    if [ -n "$heap" ] && ! printf '%s' "$heap" | grep -q '0 still live'; then
        echo "FAIL $name (leak: $heap)"; fail=$((fail+1)); return; fi
    coll=skipped
    if [ "$have_nm" -eq 1 ]; then
        if nm "$work/$name.bin" 2>/dev/null | grep -q 'jrt_collect_step'; then coll=yes; else coll=no; fi
        if [ "$coll" != "$want_coll" ]; then
            echo "FAIL $name (cycle collector linked=$coll, want $want_coll)"; fail=$((fail+1)); return; fi
    fi
    echo "ok   $name (out=$val, 0-live, collector=$coll)"; pass=$((pass+1))
}

# reject <name> <expected message fragment> <<vr…   — must NOT compile.
reject() {
    name="$1"; want="$2"; f="$work/$name.vr"; cat > "$f"
    if "$vire" build "$f" -o "$work/$name.bin" >"$work/o" 2>&1; then
        echo "FAIL $name (compiled, expected rejection)"; fail=$((fail+1)); return
    fi
    if ! grep -q "$want" "$work/o"; then
        echo "FAIL $name (wrong message: $(head -1 "$work/o"))"; fail=$((fail+1)); return
    fi
    echo "ok   $name (rejected: $(head -1 "$work/o"))"; pass=$((pass+1))
}

# --- 1. the cycle, weak back-edge: RC-only, collector-free, 0-live ------------
# Parent owns Child, Child points BACK at Parent — a cycle in the reference graph, but
# not in the OWNERSHIP graph, because the back-edge is weak. 100 pairs built behind a
# `dynamic fn` seam (so they are really heap objects, not stack-allocated).
run weak_cycle 4950 no <<'EOF'
type Parent { name: Int  child: Child }
type Child { id: Int  weak parent: Parent }
dynamic fn mk(n: Int) -> Parent {
    mut p = Parent(n, null)
    p.child = Child(n + 1, p)
    p
}
fn main() {
    mut acc = 0
    mut t = 0
    while t < 100 { mut p = mk(t)  acc = acc + p.child.parent.name  t = t + 1 }
    print(acc)
}
EOF

# --- 2. the SAME program with an owning back-edge: the collector comes back ----
# The contrast that gives case 1 its meaning: drop `weak` and nothing else, and the
# acyclicity analysis loses the shape — the binary now carries a tracing cycle collector
# to stay 0-live. Same output either way; the difference is what got linked.
run strong_cycle 4950 yes <<'EOF'
type Parent { name: Int  child: Child }
type Child { id: Int  parent: Parent }
dynamic fn mk(n: Int) -> Parent {
    mut p = Parent(n, null)
    p.child = Child(n + 1, p)
    p
}
fn main() {
    mut acc = 0
    mut t = 0
    while t < 100 { mut p = mk(t)  acc = acc + p.child.parent.name  t = t + 1 }
    print(acc)
}
EOF

# --- 3. drop/trace/store, at the IR level -------------------------------------
# The runtime oracle above proves the *effect*; this proves the *mechanism*, so a
# regression can't be masked by a collector quietly cleaning up after it.
cat > "$work/ir.vr" <<'EOF'
type Parent { name: Int  child: Child }
type Child { id: Int  weak parent: Parent }
dynamic fn mk(n: Int) -> Parent {
    mut p = Parent(n, null)
    p.child = Child(n + 1, p)
    p
}
fn main() { mut p = mk(1)  print(p.child.parent.name) }
EOF
ir="$work/ir.ll"
if ! "$vire" build --emit=llvm "$work/ir.vr" >"$ir" 2>/dev/null; then
    echo "FAIL ir (emit)"; fail=$((fail+1))
else
    # drop.Child must release NOTHING (its only ref field is weak), while drop.Parent
    # must release its owning child — otherwise "excluded from drop" would just mean
    # "drop is broken".
    dropc="$(sed -n '/define internal void @drop.Child(/,/^}/p' "$ir" | grep -c 'jrt_release')"
    dropp="$(sed -n '/define internal void @drop.Parent(/,/^}/p' "$ir" | grep -c 'jrt_release')"
    tracec="$(sed -n '/define internal void @trace.Child(/,/^}/p' "$ir" | grep -c 'call')"
    tracep="$(sed -n '/define internal void @trace.Parent(/,/^}/p' "$ir" | grep -c 'call')"
    if [ "$dropc" -eq 0 ] && [ "$dropp" -ge 1 ]; then
        echo "ok   ir_drop (drop.Child releases 0, drop.Parent releases $dropp)"; pass=$((pass+1))
    else
        echo "FAIL ir_drop (drop.Child=$dropc releases, drop.Parent=$dropp)"; fail=$((fail+1))
    fi
    if [ "$tracec" -eq 0 ] && [ "$tracep" -ge 1 ]; then
        echo "ok   ir_trace (trace.Child visits 0, trace.Parent visits $tracep)"; pass=$((pass+1))
    else
        echo "FAIL ir_trace (trace.Child=$tracec calls, trace.Parent=$tracep)"; fail=$((fail+1))
    fi
    # A store INTO the weak field takes no +1: the field slot is written raw, with no
    # retain of the new value and no release of the old one.
    weakstore="$(grep -A1 'getelementptr %class.Child, ptr .*, i32 0, i32 3' "$ir" | grep -c 'jrt_retain\|jrt_release')"
    if [ "$weakstore" -eq 0 ]; then
        echo "ok   ir_store (weak field stored without retain/release)"; pass=$((pass+1))
    else
        echo "FAIL ir_store ($weakstore RC op(s) around the weak field slot)"; fail=$((fail+1))
    fi
fi

# --- 4. `weak` is contextual, not a keyword ------------------------------------
# It binds only in front of `<ident> :`. A field actually NAMED weak keeps working —
# adding the modifier must not silently break existing source.
run weak_is_not_a_keyword 5 no <<'EOF'
type A { weak: Int }
fn main() { mut a = A(5)  print(a.weak) }
EOF

# --- 5. where `weak` is refused, with a message that says why ------------------
# Silently ignoring the modifier would be the dangerous outcome: an "owning weak field"
# would make the acyclicity gate lie about a shape it can no longer reclaim.
reject weak_on_primitive 'only a reference field can be weak' <<'EOF'
type A { weak n: Int }
fn main() { print(1) }
EOF

reject weak_on_generic 'not supported on a generic type' <<'EOF'
type Box[T] { weak v: T }
fn main() { print(1) }
EOF

# The payload case must reach LOWERING, not die in the parser: `weak` parses inside a
# variant precisely so this diagnostic is the one the user sees.
reject weak_on_sum_payload 'not supported on a sum-type payload' <<'EOF'
type N { x: Int }
type S {
    A(weak p: N)
    B
}
fn main() { print(1) }
EOF

rm -rf "$work"
echo "weak: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
