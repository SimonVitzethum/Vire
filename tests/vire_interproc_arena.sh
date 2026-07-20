#!/bin/sh
# Interprocedural loop-arena soundness suite (Vire path).
#
# Pins the `while_arena_safe` interprocedural escape analysis in crates/vire/src/
# lower.rs in BOTH directions, exactly like tests/shape_soundness.sh does for the
# Java shape analysis:
#
#   PROMOTE cases  — a factory/consume pattern whose allocations provably die with
#                    the iteration MUST be arena-promoted (`jrt_arena_push` emitted),
#                    compute the right value, and stay heap-balanced (0 live). This
#                    is the interprocedural win: a callee's `return`/`break`/
#                    `continue` no longer disqualifies the arena.
#   DECLINE cases  — a pattern where an arena reference could outlive the iteration
#                    (stored to an outer var — even inside a nested `if` — into a
#                    passed object's field by a callee, or a control-flow exit that
#                    skips the en-bloc pop) MUST NOT be promoted. A wrong promotion
#                    is a use-after-free (or a pop-skipping leak), so each decline
#                    case also reads the escaped value AFTER the loop: if it were
#                    wrongly arena'd, the read would see freed memory.
#
# Every case additionally asserts heap balance (0 still live) — no promotion may
# leak. Run: sh tests/vire_interproc_arena.sh   (needs target/release/vire).
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

# check <name> <promote|decline> <expected-output> <<vr…
#   promote → jrt_arena_push MUST be emitted; decline → it must NOT.
#   Always: output == expected AND (no heap line OR '0 still live').
check() {
    name="$1"; want_dir="$2"; want="$3"; f="$work/$name.vr"; cat > "$f"
    if ! "$vire" build "$f" -o "$work/$name.bin" >/dev/null 2>"$work/e"; then
        echo "FAIL $name (build): $(head -1 "$work/e")"; fail=$((fail+1)); return
    fi
    # Promotion direction from the emitted LLVM IR (arena push present or not).
    pushes="$("$vire" build --emit=llvm "$f" 2>/dev/null | grep -c 'call void @jrt_arena_push')"
    if [ "$want_dir" = promote ] && [ "$pushes" -lt 1 ]; then
        echo "FAIL $name (expected arena promotion, none emitted)"; fail=$((fail+1)); return
    fi
    if [ "$want_dir" = decline ] && [ "$pushes" -ne 0 ]; then
        echo "FAIL $name (arena WRONGLY emitted: $pushes push(es) — would be a use-after-free)"; fail=$((fail+1)); return
    fi
    # Value + heap balance.
    out="$(FASTLLVM_HEAPSTATS=1 "$work/$name.bin" 2>&1)"
    val="$(printf '%s\n' "$out" | grep -v '^\[heap\]' | head -1)"
    heap="$(printf '%s\n' "$out" | grep '^\[heap\]')"
    if [ "$val" != "$want" ]; then
        echo "FAIL $name (got '$val', want '$want')"; fail=$((fail+1)); return
    fi
    if [ -n "$heap" ] && ! printf '%s' "$heap" | grep -q '0 still live'; then
        echo "FAIL $name (heap leak: $heap)"; fail=$((fail+1)); return
    fi
    echo "ok   $name ($want_dir, pushes=$pushes)"; pass=$((pass+1))
}

# ── PROMOTE 1: the core interprocedural win. A recursive factory `build` returns a
#    heap AST into the loop body; `eval` traverses it and uses `return`. The tree
#    dies with the iteration. Complete binary tree depth 10 → 2^10 = 1024 leaves of
#    value 1 → eval = 1024; ×1000 iterations = 1024000. `eval`'s `return` must NOT
#    disqualify the arena (it returns to a caller still inside the arena).
check interproc_build_eval promote 1024000 <<'EOF'
type N { k: Int  v: Int  l: N  r: N }
fn leaf() -> N { N(0, 1, null, null) }
fn build(d: Int) -> N {
    if d <= 0 { leaf() } else { N(1, 0, build(d - 1), build(d - 1)) }
}
fn eval(n: N) -> Int {
    if n.k == 0 { return n.v }
    eval(n.l) + eval(n.r)
}
fn main() {
    mut acc = 0
    mut i = 0
    while i < 1000 { mut t = build(10)  acc = acc + eval(t)  i = i + 1 }
    print(acc)
}
EOF

# ── PROMOTE 2: a NESTED loop with `break`/`continue` inside the arena body still
#    promotes — the break targets the inner loop, not the arena loop, so it does
#    not skip the pop. Guards against over-restriction of the break/continue rule.
#    Inner loop adds t.v (=i) for j=0..4 then breaks → 5*i per iteration.
#    5 * sum_{i=0}^{999} i = 5 * 499500 = 2497500.
check nested_loop_break promote 2497500 <<'EOF'
type N { k: Int  v: Int  l: N  r: N }
fn make(x: Int) -> N { N(0, x, null, null) }
fn work(n: Int) -> Int {
    mut acc = 0
    mut i = 0
    while i < n {
        mut t = make(i)
        mut j = 0
        while j < 10 { if j == 5 { break }  acc = acc + t.v  j = j + 1 }
        i = i + 1
    }
    acc
}
fn main() { print(work(1000)) }
EOF

# ── DECLINE 1: a callee-returned ref stored into an OUTER variable at the top
#    level → escapes the iteration. Read after the loop (keep.v = 999). Wrong
#    promotion ⇒ keep dangles into a freed arena.
check escape_outer_toplevel decline 999 <<'EOF'
type N { k: Int  v: Int  l: N  r: N }
fn make(x: Int) -> N { N(0, x, null, null) }
fn main() {
    mut keep = make(0)
    mut i = 0
    while i < 1000 { mut t = make(i)  keep = t  i = i + 1 }
    print(keep.v)
}
EOF

# ── DECLINE 2: the SAME escape, but the store to the outer var is nested inside an
#    `if` (was a latent use-after-free — the old `top`-gated check only fired at the
#    loop-body top level and missed this). Must decline; keep.v = 999 after the loop.
check escape_outer_in_if decline 999 <<'EOF'
type N { k: Int  v: Int  l: N  r: N }
fn make(x: Int) -> N { N(0, x, null, null) }
fn main() {
    mut keep = make(0)
    mut i = 0
    while i < 1000 { mut t = make(i)  if i == 999 { keep = t }  i = i + 1 }
    print(keep.v)
}
EOF

# ── DECLINE 3: a CALLEE stores a fresh ref into a field of a passed-in object that
#    outlives the arena. The base-insensitive field-write rule must catch it even
#    though the write is inside `stash`, not the loop body. b.item.v = 999 after.
check callee_field_store decline 999 <<'EOF'
type N { k: Int  v: Int  l: N  r: N }
type Box { item: N }
fn make(x: Int) -> N { N(0, x, null, null) }
fn stash(b: Box, x: Int) { b.item = make(x) }
fn main() {
    mut b = Box(null)
    mut i = 0
    while i < 1000 { stash(b, i)  i = i + 1 }
    print(b.item.v)
}
EOF

# ── DECLINE 4: a `return` in the loop's OWN function (inside an `if`) bypasses the
#    en-bloc pop → must stay blocked. acc = sum 0..999 = 499500 (guard never trips).
check loop_return decline 499500 <<'EOF'
type N { k: Int  v: Int  l: N  r: N }
fn make(x: Int) -> N { N(0, x, null, null) }
fn work(n: Int) -> Int {
    mut acc = 0
    mut i = 0
    while i < n {
        mut t = make(i)
        acc = acc + t.v
        if acc > 1000000000 { return 0 - 1 }
        i = i + 1
    }
    acc
}
fn main() { print(work(1000)) }
EOF

# ── DECLINE 5: a `continue` in the loop's own function (inside an `if`) skips the
#    pop → must stay blocked (else the arena stack grows unbalanced = leak). Sum of
#    odd i in 1..1000 = 250000.
check loop_continue decline 250000 <<'EOF'
type N { k: Int  v: Int  l: N  r: N }
fn make(x: Int) -> N { N(0, x, null, null) }
fn work(n: Int) -> Int {
    mut acc = 0
    mut i = 0
    while i < n {
        i = i + 1
        if i % 2 == 0 { continue }
        mut t = make(i)
        acc = acc + t.v
    }
    acc
}
fn main() { print(work(1000)) }
EOF

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
