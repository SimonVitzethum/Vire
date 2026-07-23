#!/bin/sh
# FASTLLVM_GUARD_FREE temporal-safety probe — two halves that must BOTH hold:
#   (1) Positive control: the guard actually catches a premature free. A tiny C
#       harness allocates via jrt_alloc, releases (rc->0, poisoned), then reads the
#       dangling reference. Under the guard that MUST be a SIGSEGV (exit 139); with
#       the guard off the same read silently returns recycled bytes (exit 0). A guard
#       that never fires would make every "clean" run below meaningless.
#   (2) Ownership programs stay clean AND correct under the guard — no premature free
#       in the RC-managed object-graph residual (where the 0-live oracle is blind to
#       *when* the free happened).
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }
command -v clang >/dev/null 2>&1 || { echo "skip vire_guard (no clang)"; exit 0; }
work="$(mktemp -d)"; pass=0; fail=0

# --- (1) positive control ------------------------------------------------------
cat > "$work/pc.c" <<'EOF'
#include <stdint.h>
extern void *jrt_alloc(int64_t);
extern void jrt_release(void *);
int main(void) {
    volatile int64_t *p = (int64_t *)jrt_alloc(64);
    p[2] = 0x1234;              /* a field, while alive */
    jrt_release((void *)p);     /* rc 1->0 -> guard poisons the page */
    return (int)(p[2] & 1);     /* USE-AFTER-FREE: SIGSEGV under the guard */
}
EOF
if clang -O2 -w -DFASTLLVM_NO_CYCLES -c "$root/crates/driver/src/runtime.c" -o "$work/rt.o" 2>/dev/null \
   && clang -O2 -w "$work/pc.c" "$work/rt.o" -lm -o "$work/pc" 2>/dev/null; then
    # Capture via command substitution so the shell's "Segmentation fault" notice for
    # the (expected) crash under the guard lands in a discarded variable, not the log.
    _=$("$work/pc" 2>&1); off=$?
    _=$(FASTLLVM_GUARD_FREE=1 "$work/pc" 2>&1); on=$?
    if [ "$off" -eq 0 ] && [ "$on" -eq 139 ]; then
        echo "ok   guard_catches_premature_free (off=$off on=SIGSEGV)"; pass=$((pass+1))
    else
        echo "FAIL guard_catches_premature_free (off=$off on=$on; want off=0 on=139)"; fail=$((fail+1))
    fi
else
    echo "skip guard positive-control (runtime.c build failed)"
fi

# --- (2) ownership programs clean + correct under the guard --------------------
case_() {  # case_ <name> <want>  (reads .vr on stdin)
    name="$1"; want="$2"; f="$work/$name.vr"; cat > "$f"
    if ! "$vire" build "$f" -o "$work/$name" >/dev/null 2>"$work/e"; then
        echo "FAIL $name (build): $(head -1 "$work/e")"; fail=$((fail+1)); return
    fi
    out="$(FASTLLVM_GUARD_FREE=1 "$work/$name" 2>/dev/null | grep -v '^\[' | tr '\n' ' ')"
    code=$?
    out="$(printf '%s' "$out" | sed 's/ *$//')"
    if [ "$code" -eq 139 ] || [ "$code" -eq 134 ]; then
        echo "FAIL $name (SIGSEGV under guard — premature free)"; fail=$((fail+1))
    elif [ "$out" != "$want" ]; then
        echo "FAIL $name (got '$out', want '$want')"; fail=$((fail+1))
    else echo "ok   $name (guard-clean)"; pass=$((pass+1)); fi
}

# a small binary tree (heap, 0 retain, one release/object — the thesis config)
case_ guard_btree 1533 <<'EOF'
type Tree { l: Tree r: Tree }
fn make(d: Int) -> Tree { if d == 0 { Tree(null, null) } else { Tree(make(d - 1), make(d - 1)) } }
fn check(t: Tree, d: Int) -> Int { if d == 0 { 1 } else { 1 + check(t.l, d - 1) + check(t.r, d - 1) } }
fn main() {
    mut sum = 0
    mut n = 0
    while n < 3 { sum = sum + check(make(8), 8) n = n + 1 }
    print(sum)
}
EOF

# a shared heap child with two independent parents (genuine sharing → real retains
# in the IR, elided in the shipping binary — must not be a premature free)
case_ guard_shared_child 198 <<'EOF'
type Pair { a: Node b: Node }
type Node { val: Int next: Node }
fn build() -> Pair {
    mut child = Node(99, null)
    mut p1 = Node(1, child)
    mut p2 = Node(2, child)
    Pair(p1, p2)
}
fn main() {
    mut pr = build()
    print(pr.a.next.val + pr.b.next.val)
}
EOF

# list traversal via a reassigned reference (the most common ref idiom)
case_ guard_list_traversal 6 <<'EOF'
type Node { val: Int nxt: Node }
fn main() {
    mut head = Node(1, Node(2, Node(3, null)))
    mut cur = head
    mut sum = 0
    while cur != null { sum = sum + cur.val  cur = cur.nxt }
    print(sum)
}
EOF

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
