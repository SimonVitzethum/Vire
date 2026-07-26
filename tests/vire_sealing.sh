#!/bin/sh
# Sealing model — the optimizer-scoping foundation of the dynamic surface
# (DYNAMIC-VIRE-PLAN.md §2/§6). Sealed is the keyword-free default; `dynamic fn` (and
# `open fn`) declare a runtime-overridable SEAM. A call to a seam is a hard black box to
# region inference — a future override could escape/retain its args — so the loop-arena
# MUST decline at a seam, while an identical SEALED builder still gets the arena. Both
# compute the same value and stay 0-live (the seam falls back to sound RC). This pins the
# memory-safety-critical boundary before any runtime override is wired.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

# arena <name> <fire|decline> <expected-out> <<vr…
arena() {
    name="$1"; want_dir="$2"; want="$3"; f="$work/$name.vr"; cat > "$f"
    if ! "$vire" build "$f" -o "$work/$name.bin" >/dev/null 2>"$work/e"; then
        echo "FAIL $name (build): $(head -1 "$work/e")"; fail=$((fail+1)); return
    fi
    pushes="$("$vire" build --emit=llvm "$f" 2>/dev/null | grep -c 'call void @jrt_arena_push')"
    if [ "$want_dir" = fire ] && [ "$pushes" -lt 1 ]; then
        echo "FAIL $name (expected arena, none emitted)"; fail=$((fail+1)); return
    fi
    if [ "$want_dir" = decline ] && [ "$pushes" -ne 0 ]; then
        echo "FAIL $name (arena WRONGLY fired at a seam: $pushes push(es))"; fail=$((fail+1)); return
    fi
    out="$(FASTLLVM_HEAPSTATS=1 "$work/$name.bin" 2>&1)"
    val="$(printf '%s\n' "$out" | grep -v '^\[heap\]' | head -1)"
    heap="$(printf '%s\n' "$out" | grep '^\[heap\]')"
    if [ "$val" != "$want" ]; then echo "FAIL $name (got '$val', want '$want')"; fail=$((fail+1)); return; fi
    if [ -n "$heap" ] && ! printf '%s' "$heap" | grep -q '0 still live'; then
        echo "FAIL $name (heap leak: $heap)"; fail=$((fail+1)); return; fi
    echo "ok   $name ($want_dir, pushes=$pushes, out=$val)"; pass=$((pass+1))
}

# SEALED builder: the loop-arena fires (build/use/drop per iteration).
arena sealed fire 210000 <<'EOF'
type Node { id: Int  next: Node }
fn build(n: Int) -> Node { if n == 0 { null } else { Node(n, build(n - 1)) } }
fn sum(x: Node) -> Int { if x == null { 0 } else { x.id + sum(x.next) } }
fn main() {
    mut acc = 0
    mut t = 0
    while t < 1000 { mut h = build(20)  acc = (acc + sum(h)) % 1000000007  t = t + 1 }
    print(acc)
}
EOF

# The SAME builder as a `dynamic fn` SEAM: the arena must decline (a runtime override
# could escape the graph); RC keeps it 0-live and the value is identical.
arena seam_dynamic decline 210000 <<'EOF'
type Node { id: Int  next: Node }
dynamic fn build(n: Int) -> Node { if n == 0 { null } else { Node(n, build(n - 1)) } }
fn sum(x: Node) -> Int { if x == null { 0 } else { x.id + sum(x.next) } }
fn main() {
    mut acc = 0
    mut t = 0
    while t < 1000 { mut h = build(20)  acc = (acc + sum(h)) % 1000000007  t = t + 1 }
    print(acc)
}
EOF

# `open fn` is the same seam as `dynamic fn`.
arena seam_open decline 210000 <<'EOF'
type Node { id: Int  next: Node }
open fn build(n: Int) -> Node { if n == 0 { null } else { Node(n, build(n - 1)) } }
fn sum(x: Node) -> Int { if x == null { 0 } else { x.id + sum(x.next) } }
fn main() {
    mut acc = 0
    mut t = 0
    while t < 1000 { mut h = build(20)  acc = (acc + sum(h)) % 1000000007  t = t + 1 }
    print(acc)
}
EOF

# `open trait` / `open type` parse and the program still runs.
arena open_decls fire 5 <<'EOF'
open type Shape { area: Int }
open trait Draw { fn draw(self) -> Int }
fn make() -> Shape { Shape(5) }
fn main() {
    mut acc = 0
    mut t = 0
    while t < 1 { mut s = make()  acc = s.area  t = t + 1 }
    print(acc)
}
EOF

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
