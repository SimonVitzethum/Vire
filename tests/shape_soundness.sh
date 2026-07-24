#!/bin/sh
# Shape/freshness analysis soundness (crates/solver/src/lib.rs `shape_proves_acyclic`).
# A self-referential type's cycle collector may be dropped ONLY when every cyclic-slot
# store is null or a fresh+linear value (so no runtime cycle can form). This suite pins
# both directions on the Java path (where cyclic types are detected):
#   * a pure TREE  → collector dropped, and still 0 live (no leak).
#   * a real CYCLE → collector KEPT, and still 0 live (the collector reclaims it).
# A regression either way (dropping a needed collector = leak; keeping an unneeded one =
# slow) fails here.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
fj="$root/target/debug/fastjavac"
work="$(mktemp -d)"; pass=0; fail=0
command -v javac >/dev/null 2>&1 || { echo "javac missing — skipping shape suite"; exit 0; }
[ -x "$fj" ] || { echo "fastjavac missing — run 'cargo build' first"; exit 1; }

# case <name> <drop|keep> <expected-stdout> <<java...
case_() {
    name="$1"; want_collector="$2"; want_out="$3"
    src="$work/$name.java"; sed "s/__CLASS__/$name/g" > "$src"
    if ! javac -d "$work" "$src" 2>"$work/jerr"; then
        echo "FAIL $name (javac): $(head -1 "$work/jerr")"; fail=$((fail+1)); return
    fi
    classes=$(ls "$work/$name.class" "$work/$name"'$'*.class 2>/dev/null)
    if ! "$fj" --stats -o "$work/$name.bin" $classes >/dev/null 2>"$work/ferr"; then
        echo "FAIL $name (fastjavac): $(head -1 "$work/ferr")"; fail=$((fail+1)); return
    fi
    # Collector kept iff the solver reports it required. This reads the solver's own
    # decision (`--stats`, stderr) rather than grepping `collect_cycles` in the binary:
    # under `-flto` the collector is inlined into its caller, so the standalone symbol
    # is gone even though the collector functionally runs — the symbol grep gave a
    # false "drop" for every real cycle.
    if grep -q 'Zyklen-Collector: required' "$work/ferr"; then have=keep; else have=drop; fi
    out="$(FASTLLVM_HEAPSTATS=1 "$work/$name.bin" 2>&1)"
    val="$(printf '%s\n' "$out" | grep -v '^\[heap\]' | head -1)"
    live_ok=1; echo "$out" | grep -q '\[heap\]' && ! echo "$out" | grep -q '0 still live' && live_ok=0
    if [ "$have" = "$want_collector" ] && [ "$val" = "$want_out" ] && [ "$live_ok" = 1 ]; then
        echo "ok   $name (collector=$have, out=$val, 0-live)"; pass=$((pass+1))
    else
        echo "FAIL $name (collector=$have want=$want_collector, out=$val want=$want_out, live_ok=$live_ok)"; fail=$((fail+1))
    fi
}

# A pure tree — every child is a fresh, linearly-consumed make() result → DROP collector.
case_ ShapeTree drop 66759344 <<'EOF'
public class __CLASS__ {
    static class Node { Node l, r; }
    static int check(Node n) { return n.l == null ? 1 : 1 + check(n.l) + check(n.r); }
    static Node make(int d) { Node n = new Node(); if (d > 0) { n.l = make(d-1); n.r = make(d-1); } return n; }
    public static void main(String[] a) {
        int max = 18; long sum = 0;
        for (int depth = 4; depth <= max; depth += 2) {
            int it = 1 << (max - depth + 4); long chk = 0;
            for (int i = 0; i < it; i++) chk += check(make(depth));
            sum += chk;
        }
        System.out.println(sum);
    }
}
EOF

# An escaping a<->b cycle — `b.next=a` stores a non-fresh value → KEEP collector.
case_ ShapeCycle keep 40000000000 <<'EOF'
public class __CLASS__ {
    static class Node { Node next; int v; }
    static Node makeCycle(int v) { Node a = new Node(); a.v = v; Node b = new Node(); b.v = v+1; a.next = b; b.next = a; return a; }
    public static void main(String[] args) {
        long sum = 0;
        for (int i = 0; i < 200000; i++) { Node c = makeCycle(i); sum += c.v + c.next.v; }
        System.out.println(sum);
    }
}
EOF

# A doubly-linked list — `x.prev = cur` stores a non-fresh value (2-cycles) → KEEP.
case_ ShapeDll keep 0 <<'EOF'
public class __CLASS__ {
    static class Node { Node next, prev; int v; }
    static Node build(int n) { Node head = new Node(); Node cur = head; for (int i = 1; i < n; i++) { Node x = new Node(); x.v = i; cur.next = x; x.prev = cur; cur = x; } return head; }
    public static void main(String[] args) { long s = 0; for (int i = 0; i < 100000; i++) { Node h = build(4); s += h.next.prev.v; } System.out.println(s); }
}
EOF

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
