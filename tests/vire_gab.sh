#!/bin/sh
# Vire → Gabbro backend (AGENTS.md §0: Gabbro ist das Target).
# Each case: `.vr` → `vire build --emit=gab` → `gabbro check` (0 errors,
# Black-Box oracle, Gabbro repo untouched) → output of `vire run` (LLVM path,
# execution oracle until Gabbro's validating compiler lands) must match.
# The C emitter (`gabbro build` via C11) is deprecated and not used.
# Rejects: unemittable constructs fail `--emit=gab` with an E5xx code.
#
# Needs a `gabbro` binary: $GABBRO_BIN or PATH. Skips gracefully without one.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }
if [ -n "${GABBRO_BIN:-}" ]; then gab="$GABBRO_BIN";
elif command -v gabbro >/dev/null 2>&1; then gab="gabbro";
else echo "skip vire_gab (no gabbro binary — set GABBRO_BIN)"; exit 0; fi

gab_case() { # gab_case <name> <want> <<vr
    name="$1"; want="$2"; f="$work/$name.vr"; cat > "$f"
    if ! "$vire" build "$f" --emit=gab > "$work/$name.gab" 2>"$work/e"; then
        echo "FAIL $name (gab emit): $(head -1 "$work/e")"; fail=$((fail+1)); return
    fi
    if ! "$gab" check "$work/$name.gab" > "$work/chk" 2>&1; then
        echo "FAIL $name (gabbro check): $(grep -m1 -E 'error' "$work/chk")"; fail=$((fail+1)); return
    fi
    if ! grep -q "0 errors" "$work/chk"; then
        echo "FAIL $name (gabbro check, no 0-errors line)"; fail=$((fail+1)); return
    fi
    # Execution truth via the LLVM oracle (C emitter deprecated).
    got="$("$vire" run "$f" 2>/dev/null)"
    want_l="$(printf '%s' "$want")"
    if [ "$got" != "$want_l" ]; then
        echo "FAIL $name (vire run: got [$got] want [$want_l])"; fail=$((fail+1)); return
    fi
    echo "ok   $name"; pass=$((pass+1))
}

gab_reject() { # gab_reject <name> <pattern> <<vr — --emit=gab must fail with pattern.
    name="$1"; want="$2"; f="$work/$name.vr"; cat > "$f"
    if "$vire" build "$f" --emit=gab > /dev/null 2>"$work/e"; then
        echo "FAIL $name (emitted — must be rejected)"; fail=$((fail+1)); return
    fi
    if grep -q "$want" "$work/e"; then echo "ok   $name (rejected)"; pass=$((pass+1));
    else echo "FAIL $name (wrong error: $(head -1 "$work/e"))"; fail=$((fail+1)); fi
}

# --- M1: straight-line integer arithmetic + print ---------------------------
gab_case gab_arith "$(printf '42')" <<'EOF'
fn add(a: Int, b: Int) -> Int {
    a + b
}
fn main() {
    mut x = add(40, 2)
    print(x)
}
EOF

# --- M1: if/else diamonds + calls + comparisons ------------------------------
gab_case gab_ifmax "$(printf '7\n9')" <<'EOF'
fn max2(a: Int, b: Int) -> Int {
    mut r = a
    if b > a {
        r = b
    }
    r
}
fn main() {
    print(max2(3, 7))
    print(max2(9, 2))
}
EOF

# --- M1: sub/mul/neg/negative literals ---------------------------------------
gab_case gab_negmul "$(printf '6\n42\n-1')" <<'EOF'
fn main() {
    mut a = 10 - 4
    mut b = 6 * 7
    mut c = 0 - 1
    print(a)
    print(b)
    print(c)
}
EOF

# --- M1: inlined multi-function program ---------------------------------------
gab_case gab_poly 52212 <<'EOF'
fn poly(x: Int) -> Int {
    mut y = x * x
    y = y + 2 * x
    y = y + 1
    mut z = y * y
    z = z + y
    z = z + x
    mut w = z * 2
    w = w + 3 * y
    w = w + 5 * x
    w = w + 7
    mut v = w * w
    v = v + w
    v
}
fn main() {
    print(poly(2))
}
EOF

# --- M1: surviving cross-function call (>16 stmts, not inlined) ---------------
gab_case gab_bigcall 60362514 <<'EOF'
fn big(x: Int) -> Int {
    mut y = x * x
    y = y + 2 * x
    y = y + 1
    mut z = y * y
    z = z + y
    z = z + x
    mut w = z * 2
    w = w + 3 * y
    w = w + 5 * x
    w = w + 7
    mut v = w * w
    v = v + w
    mut a = v * 3
    a = a + 11
    a = a * 5
    a = a + 13
    a = a * 7
    a = a + 17
    a = a * 11
    a = a + 19
    a = a + v
    a
}
fn main() {
    print(big(2))
}
EOF

# --- M2b: counted for-loop (retry bounded/until/on_exceeded abort) -----------
gab_case gab_forloop "$(printf '45')" <<'EOF'
fn main() {
    mut s = 0
    for i in 0..10 {
        s = s + i
    }
    print(s)
}
EOF

# --- M2b: counted while-loop (tmp+copy step chase) ---------------------------
gab_case gab_while "$(printf '45')" <<'EOF'
fn main() {
    mut i = 0
    mut s = 0
    while i < 10 {
        s = s + i
        i = i + 1
    }
    print(s)
}
EOF

# --- M2b: while with break under if (leave) ----------------------------------
gab_case gab_break "$(printf '45')" <<'EOF'
fn main() {
    mut i = 0
    mut s = 0
    while i < 100 {
        if i >= 10 {
            break
        }
        s = s + i
        i = i + 1
    }
    print(s)
}
EOF

# --- M2b: while with continue under if (next) --------------------------------
gab_case gab_continue "$(printf '45')" <<'EOF'
fn main() {
    mut i = 0
    mut s = 0
    while i < 10 {
        i = i + 1
        if i < 5 {
            continue
        }
        s = s + i
    }
    print(s)
}
EOF

# --- M2b: nested counted loops ------------------------------------------------
gab_case gab_nested "$(printf '9')" <<'EOF'
fn main() {
    mut i = 0
    mut s = 0
    while i < 3 {
        mut j = 0
        while j < 3 {
            s = s + 1
            j = j + 1
        }
        i = i + 1
    }
    print(s)
}
EOF

# --- M2b: sequential loops (break-loop then for-loop) -------------------------
gab_case gab_seq "$(printf '55')" <<'EOF'
fn main() {
    mut i = 0
    mut s = 0
    while i < 100 {
        if i >= 10 {
            break
        }
        s = s + i
        i = i + 1
    }
    for k in 0..5 {
        s = s + k
    }
    print(s)
}
EOF

# --- M2b: zero-trip loop (empty range skips the body) -------------------------
gab_case gab_zerotrip "$(printf '7')" <<'EOF'
fn main() {
    mut s = 7
    for i in 5..5 {
        s = s + i
    }
    print(s)
}
EOF

# --- M2b: affine-shifted counter (bound moves exactly, Gabbro verifies) ----
gab_case gab_affine "$(printf '36')" <<'EOF'
fn main() {
    mut s = 0
    mut i = 0
    while i + 1 < 10 {
        s = s + i
        i = i + 1
    }
    print(s)
}
EOF

gab_case gab_affine_right "$(printf '36')" <<'EOF'
fn main() {
    mut s = 0
    mut i = 0
    while 10 > i + 1 {
        s = s + i
        i = i + 1
    }
    print(s)
}
EOF

# --- decline: multiplicative counter shape (E501, later step) -----------------
gab_reject gab_affine_mul_e501 "E501" <<'EOF'
fn main() {
    mut s = 0
    mut i = 0
    while i * 2 < 20 {
        s = s + i
        i = i + 1
    }
    print(s)
}
EOF

# --- M3a: strings (bounded literals, concat, len, ==, print) -----------------
gab_case gab_str "$(printf 'hi!\n3\n0')" <<'EOF'
fn main() {
    mut a = "hi"
    mut b = a + "!"
    print(b)
    print(b.len())
    print(b == "hi!")
}
EOF

# --- M3a: string predicates (isEmpty/equals as 0/1) --------------------------
gab_case gab_strpred "$(printf '0\n1\n0')" <<'EOF'
fn main() {
    mut a = "hi"
    print(a.isEmpty())
    print(a.equals("hi"))
    print(a.equals("x"))
}
EOF

# --- M3b: const arrays (tables, literal indices, len) -------------------------
gab_case gab_array "$(printf '3\n20\n99')" <<'EOF'
fn main() {
    mut xs = [10, 20, 30]
    print(xs.len())
    print(xs[1])
    xs[0] = 99
    print(xs[0])
}
EOF

# --- M3b: dynamic indices (narrow-guard in a loop) ---------------------------
gab_case gab_arrdyn "$(printf '60')" <<'EOF'
fn main() {
    mut xs = [10, 20, 30]
    mut i = 0
    mut s = 0
    while i < 3 {
        s = s + xs[i]
        i = i + 1
    }
    print(s)
}
EOF

# --- M3c: Option match (sums as tag/value pairs) ------------------------------
gab_case gab_option "$(printf '5')" <<'EOF'
fn f(x: Int) -> Option[Int] {
    if x > 0 {
        Some(x)
    } else {
        None
    }
}
fn main() {
    match f(5) {
        Some(v) -> print(v)
        None -> print(0 - 1)
    }
}
EOF

# --- M3c: Result match (int payloads) ----------------------------------------
gab_case gab_result "$(printf '5')" <<'EOF'
fn main() {
    mut x = 5
    mut r: Result[Int, Int] = if x > 0 { Ok(x) } else { Err(0 - 1) }
    match r {
        Ok(v) -> print(v)
        Err(e) -> print(e)
    }
}
EOF

# --- decline: string sum payloads (E501 — Gabbro N465, later step) ------------
gab_reject gab_sumstr_e501 "E501" <<'EOF'
fn f(x: Int) -> Result[Int, Str] {
    if x > 0 {
        Ok(x)
    } else {
        Err("neg")
    }
}
fn main() {
    match f(5) {
        Ok(v) -> print(v)
        Err(e) -> print(e)
    }
}
EOF

# --- decline: sum parameters need monomorphized signatures (E500, M3c) --------
gab_reject gab_sumparam_e500 "E500" <<'EOF'
fn f(o: Option[Int]) -> Int {
    mut a = 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    match o {
        Some(v) -> a + v
        None -> a
    }
}
fn main() {
    print(f(Some(5)))
}
EOF

# --- decline: const index past the length (E501, bound solver step) ----------
gab_reject gab_arroob_e501 "E501" <<'EOF'
fn main() {
    mut xs = [10, 20, 30]
    print(xs[5])
}
EOF

# --- decline: non-i64 array elements (E501, later step) ----------------------
gab_reject gab_arrelem_e501 "E501" <<'EOF'
fn main() {
    mut xs = ["a", "b"]
    print(xs.len())
}
EOF

# --- decline: string parameters need monomorphized bounds (E500, M3c) ---------
gab_reject gab_strparam_e500 "E500" <<'EOF'
fn f(s: Str) -> Int {
    mut a = 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + s.len()
    a
}
fn main() {
    print(f("hi"))
}
EOF

# --- decline: string methods need dedicated lowering (E501, later step) ------
gab_reject gab_strmethod_e501 "E501" <<'EOF'
fn main() {
    mut a = "hello"
    print(a.substring(0, 2))
}
EOF

# --- decline: dynamic loop bound (E501, bound solver step) --------------------
gab_reject gab_dynbound_e501 "E501" <<'EOF'
fn f(n: Int) -> Int {
    mut i = 0
    while i < n {
        i = i + 1
    }
    i
}
fn main() {
    print(f(10))
}
EOF

# --- M4a: objects as static table rows (O29 bounded scope) --------------------
gab_case gab_ref "$(printf '42')" <<'EOF'
type Node { val: Int }
fn main() {
    mut n = Node(42)
    print(n.val)
}
EOF

# --- M4a: builder + mutation + aliasing + i64 values + null test -------------
gab_case gab_objects "$(printf '5000000009\n5000000009\n1')" <<'EOF'
type Node { val: Int next: Node }
fn mk(v: Int) -> Node {
    Node(v, null)
}
fn main() {
    mut n = mk(7)
    n.val = 9
    mut m = n
    m.val = m.val + 5000000000
    print(m.val)
    print(n.val)
    if m.next == null {
        print(1)
    } else {
        print(0)
    }
}
EOF

# --- decline: repeated construction (E501, row linearization later) -----------
gab_reject gab_objloop_e501 "E501" <<'EOF'
type Node { val: Int }
fn main() {
    mut head = Node(0)
    mut i = 1
    while i < 3 {
        mut x = Node(i)
        head = x
        i = i + 1
    }
    print(head.val)
}
EOF

# --- M4b: reference parameters (body-use class + call-site union) ------------
gab_case gab_objparam "$(printf '18')" <<'EOF'
type Node { val: Int }
fn get(v: Node) -> Int {
    mut a = v.val
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a
}
fn main() {
    print(get(Node(1)))
}
EOF

# --- M4b: predicate over a nullable parameter (null checked, never derefed) --
gab_case gab_objpred "$(printf '18\n17')" <<'EOF'
type Node { val: Int }
fn isNil(n: Node) -> Int {
    mut r = 0
    if n == null {
        r = 1
    } else {
        r = 0
    }
    r = r + 1
    r = r + 1
    r = r + 1
    r = r + 1
    r = r + 1
    r = r + 1
    r = r + 1
    r = r + 1
    r = r + 1
    r = r + 1
    r = r + 1
    r = r + 1
    r = r + 1
    r = r + 1
    r = r + 1
    r = r + 1
    r = r + 1
    r
}
fn main() {
    print(isNil(null))
    print(isNil(Node(7)))
}
EOF

# --- M4b: surviving method call (mapped into vire_method_ namespace) ---------
gab_case gab_objmethod "$(printf '160')" <<'EOF'
type Vec2 { x: Int y: Int }
impl Vec2 {
    fn len(self) -> Int {
        mut a = self.x + self.y
        a = a + 0
        a = a + 1
        a = a + 2
        a = a + 3
        a = a + 4
        a = a + 5
        a = a + 6
        a = a + 7
        a = a + 8
        a = a + 9
        a = a + 10
        a = a + 11
        a = a + 12
        a = a + 13
        a = a + 14
        a = a + 15
        a = a + 16
        a = a + 17
        a
    }
}
fn main() {
    mut v = Vec2(3, 4)
    print(v.len())
}
EOF

# --- decline: null-only call leaves the class unknown (E500 M4b) -------------
gab_reject gab_objparamnull_e500 "E500" <<'EOF'
type Node { val: Int }
fn ghost(x: Node) -> Int {
    mut a = 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a
}
fn main() {
    print(ghost(null))
}
EOF

# --- decline: non-Int fields (E500, later step) --------------------------------
gab_reject gab_boolfield_e500 "E500" <<'EOF'
type Flag { b: Bool }
fn main() {
    mut f = Flag(true)
    print(1)
}
EOF

# --- decline: reference traversal (E501, exact-trip reasoning later) ----------
gab_reject gab_traverse_e501 "E501" <<'EOF'
type Node { val: Int next: Node }
fn sum(head: Node) -> Int {
    mut s = 0
    mut c = head
    while c != null {
        s = s + c.val
        c = c.next
    }
    s
}
fn main() {
    mut z = Node(5, null)
    print(sum(z))
}
EOF

# --- decline: division (E500, narrow-guard step) --------------------------------
gab_reject gab_div_e500 "E500" <<'EOF'
fn div2(a: Int, b: Int) -> Int { a / b }
fn main() {
    print(div2(7, 2))
}
EOF

# --- decline: floats (E500, range-model step) -----------------------------------
gab_reject gab_float_e500 "E500" <<'EOF'
fn fadd(a: Float, b: Float) -> Float { a + b }
fn main() {
    fadd(1.5, 2.5)
    print(1)
}
EOF

# --- M3d: explicit region(max) → arena with visible ceiling -------------------
gab_case gab_region "$(printf '45')" <<'EOF'
fn main() {
    mut s = region(64) {
        mut t = 0
        mut i = 0
        while i < 10 {
            t = t + i
            i = i + 1
        }
        t
    }
    print(s)
}
EOF

# The ceiling must stay user-visible in the output (never solver-hidden).
f="$work/gab_region.vr"
if ! "$vire" build "$f" --emit=gab > "$work/gab_region_ceiling.gab" 2>/dev/null; then
    echo "FAIL gab_region_ceiling (gab emit)"; fail=$((fail+1))
elif ! grep -q "pub arena .* capacity 0 .. 64 of i64" "$work/gab_region_ceiling.gab"; then
    echo "FAIL gab_region_ceiling (no visible capacity 0 .. 64)"; fail=$((fail+1))
elif ! grep -q "reset " "$work/gab_region_ceiling.gab"; then
    echo "FAIL gab_region_ceiling (no reset)"; fail=$((fail+1))
else echo "ok   gab_region_ceiling"; pass=$((pass+1)); fi

# --- decline: dynamic region ceiling (E501, bound solver step) ----------------
gab_reject gab_region_dyn_e501 "E501" <<'EOF'
fn f(n: Int) -> Int {
    mut s = region(n) {
        mut t = 0
        t = t + 1
        t
    }
    s
}
fn main() {
    print(f(64))
}
EOF

# --- M3e: scalar extern "C" → Gabbro extern fn heads ---------------------------
# (Gabbro has no `extern "C"` form — the contract is signature + effects +
# costs, body is machine held at `gabbro link`. Full parity: same .vr runs
# via the LLVM oracle against a C object.)
if command -v cc >/dev/null 2>&1 || command -v clang >/dev/null 2>&1; then
    cat > "$work/gab_ext.vr" <<'EOF'
extern "C" {
    fn c_add(a: Int, b: Int) -> Int
}
fn main() {
    print(c_add(40, 2))
}
EOF
    cat > "$work/c_add.c" <<'EOF'
long c_add(long a, long b) { return a + b; }
EOF
    cc_cmd="cc"; command -v cc >/dev/null 2>&1 || cc_cmd="clang"
    if "$cc_cmd" -c "$work/c_add.c" -o "$work/c_add.o" 2>/dev/null \
        && "$vire" build "$work/gab_ext.vr" --emit=gab > "$work/gab_ext.gab" 2>"$work/e" \
        && "$gab" check "$work/gab_ext.gab" > "$work/chk" 2>&1 \
        && grep -q "0 errors" "$work/chk" \
        && grep -q "extern fn c_add(a0 : i64, a1 : i64) -> i64" "$work/gab_ext.gab" \
        && [ "$("$vire" run "$work/gab_ext.vr" --obj "$work/c_add.o" 2>/dev/null)" = "42" ]; then
        echo "ok   gab_extern"; pass=$((pass+1))
    else echo "FAIL gab_extern ($(head -1 "$work/e" "$work/chk" 2>/dev/null))"; fail=$((fail+1)); fi
else echo "skip gab_extern (no C compiler)"; fi

# --- decline: Ptr-crossing extern (E500, no int->ptr) --------------------------
gab_reject gab_extptr_e500 "E500" <<'EOF'
extern "C" {
    fn c_buf() -> Ptr
}
fn main() {
    c_buf()
    print(1)
}
EOF

# --- H1: syscalls → extern fn heads (number rides as data) ------------------
# getpid(39) is side-effect-free enough to run under the oracle (exit 0 only —
# the pid itself varies, so no output parity asserted here).
cat > "$work/gab_syscall.vr" <<'EOF'
syscall getpid() -> Int = 39
fn main() {
    mut p = getpid()
    print(0)
}
EOF
if "$vire" build "$work/gab_syscall.vr" --emit=gab > "$work/gab_syscall.gab" 2>"$work/e" \
    && "$gab" check "$work/gab_syscall.gab" > "$work/chk" 2>&1 \
    && grep -q "0 errors" "$work/chk" \
    && grep -q "extern fn vire_syscall_39_0(a0 : i64)" "$work/gab_syscall.gab" \
    && "$vire" run "$work/gab_syscall.vr" >/dev/null 2>&1; then
    echo "ok   gab_syscall"; pass=$((pass+1))
else echo "FAIL gab_syscall ($(head -1 "$work/e" "$work/chk" 2>/dev/null))"; fail=$((fail+1)); fi

# --- H1: fence runs under both (single LLVM fence, no asm) --------------------
gab_case gab_fence "$(printf '7')" <<'EOF'
fn main() {
    fence()
    print(7)
}
EOF

# --- H1: MMIO + port are compile-only (the host cannot execute them) ----------
# The hw-profile declares the region; offsets/widths are checked at lowering
# (E400), heads are per-site externs, `gabbro check` holds the rest.
printf 'uart0 0x1000 256\n' > "$work/hw.profile"
cat > "$work/gab_mmio.vr" <<'EOF'
fn main() {
    mut u = Mmio.bind("uart0")
    u.write32(0, 65)
    mut v = u.read32(0)
    print(v)
}
EOF
if "$vire" build "$work/gab_mmio.vr" --emit=gab --hw-profile "$work/hw.profile" > "$work/gab_mmio.gab" 2>"$work/e" \
    && "$gab" check "$work/gab_mmio.gab" > "$work/chk" 2>&1 \
    && grep -q "0 errors" "$work/chk" \
    && grep -q "extern fn vire_mmio_w32_1000_0(a0 : i64)" "$work/gab_mmio.gab" \
    && grep -q "extern fn vire_mmio_r32_1000_0() -> i64" "$work/gab_mmio.gab"; then
    echo "ok   gab_mmio"; pass=$((pass+1))
else echo "FAIL gab_mmio ($(head -1 "$work/e" "$work/chk" 2>/dev/null))"; fail=$((fail+1)); fi

cat > "$work/gab_port.vr" <<'EOF'
fn main() {
    port_outb(96, 65)
    print(1)
}
EOF
if "$vire" build "$work/gab_port.vr" --emit=gab > "$work/gab_port.gab" 2>"$work/e" \
    && "$gab" check "$work/gab_port.gab" > "$work/chk" 2>&1 \
    && grep -q "0 errors" "$work/chk" \
    && grep -q "extern fn vire_port_outb(" "$work/gab_port.gab"; then
    echo "ok   gab_port"; pass=$((pass+1))
else echo "FAIL gab_port ($(head -1 "$work/e" "$work/chk" 2>/dev/null))"; fail=$((fail+1)); fi

# --- decline: user fn shadowing the intrinsic namespace (E500) -----------------
gab_reject gab_syscall_shadow_e500 "E500" <<'EOF'
fn __vire_port_outb(x: Int) -> Int {
    mut a = x
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a = a + 1
    a
}
fn main() {
    print(__vire_port_outb(1))
}
EOF

# --- runtime-freedom: no jrt_* may leak into any emitted .gab ----------------
if grep -l "jrt_" "$work"/*.gab >/dev/null 2>&1; then
    echo "FAIL gab_no_runtime (jrt_ reference in .gab: $(grep -l "jrt_" "$work"/*.gab | head -1))"; fail=$((fail+1))
else echo "ok   gab_no_runtime"; pass=$((pass+1)); fi

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
