#!/bin/sh
# Generics suite: monomorphization of trait-bounded generic functions must produce
# correct results (methods resolve to the concrete impl, inlined = direct dispatch),
# and a violated bound must be a precise compile error at the instantiation.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

ok_case() {   # ok_case <name> <expected-multiline-output> <<vr
    name="$1"; want="$2"; f="$work/$name.vr"; cat > "$f"
    out="$("$vire" run "$f" 2>/dev/null | grep -vE '^verify:')"
    if [ "$out" = "$want" ]; then echo "ok   $name"; pass=$((pass+1))
    else echo "FAIL $name (got '$out', want '$want')"; fail=$((fail+1)); fi
}

err_case() {  # err_case <name> <needle> <<vr   — build must fail mentioning <needle>
    name="$1"; needle="$2"; f="$work/$name.vr"; cat > "$f"
    err="$("$vire" build "$f" -o "$work/$name.bin" 2>&1)"
    if [ -f "$work/$name.bin" ]; then echo "FAIL $name (built — should reject)"; fail=$((fail+1))
    elif echo "$err" | grep -q "$needle"; then echo "ok   $name ($needle)"; pass=$((pass+1))
    else echo "FAIL $name (missing '$needle': $(echo "$err" | head -1))"; fail=$((fail+1)); fi
}

# single bound, two impls → one monomorph each, concrete method resolved
ok_case bounded_dispatch "$(printf '26\n13')" <<'EOF'
trait Shape { fn area(self) -> Int }
type Sq { s: Int }
impl Shape for Sq { fn area(self) -> Int { self.s * self.s } }
type Rec { w: Int h: Int }
impl Shape for Rec { fn area(self) -> Int { self.w * self.h } }
fn describe[T: Shape](x: T) -> Int { x.area() + 1 }
fn main() { print(describe(Sq(5)))  print(describe(Rec(3, 4))) }
EOF

# multiple bounds `T: A + B`, both methods used
ok_case multi_bound "$(printf '3\n-20')" <<'EOF'
trait Ord2 { fn less(self, o: Int) -> Bool }
trait Show { fn val(self) -> Int }
type Num { n: Int }
impl Ord2 for Num { fn less(self, o: Int) -> Bool { self.n < o } }
impl Show for Num { fn val(self) -> Int { self.n } }
fn pick[T: Ord2 + Show](x: T, limit: Int) -> Int {
    if x.less(limit) { x.val() } else { 0 - x.val() }
}
fn main() { print(pick(Num(3), 10))  print(pick(Num(20), 10)) }
EOF

# violated bound → precise error at the instantiation
err_case unsatisfied_bound "does not implement" <<'EOF'
trait Shape { fn area(self) -> Int }
type Sq { s: Int }
impl Shape for Sq { fn area(self) -> Int { self.s * self.s } }
fn describe[T: Shape](x: T) -> Int { x.area() + 1 }
type NoShape { v: Int }
fn main() { print(describe(NoShape(7))) }
EOF

# a second bound unsatisfied (first ok) → still rejected
err_case partial_bound "does not implement \`Show\`" <<'EOF'
trait A { fn a(self) -> Int }
trait Show { fn val(self) -> Int }
type Only { n: Int }
impl A for Only { fn a(self) -> Int { self.n } }
fn need[T: A + Show](x: T) -> Int { x.a() }
fn main() { print(need(Only(1))) }
EOF

# value generics: distinct monomorph per N; N substituted as a literal so
# `array(N)` becomes a constant-size (stack-promotable) array
ok_case value_generic "$(printf '30\n285')" <<'EOF'
fn build_sum[comptime N: Int]() -> Int {
    mut a = array(N)
    mut i = 0
    while i < N { a[i] = i * i  i = i + 1 }
    mut s = 0
    mut j = 0
    while j < N { s = s + a[j]  j = j + 1 }
    s
}
fn main() { print(build_sum[5]())  print(build_sum[10]()) }
EOF

# turbofish with a scalar arg, and type-only turbofish
ok_case turbofish_mixed "$(printf '28\n99')" <<'EOF'
fn repeat[comptime N: Int](x: Int) -> Int { mut s = 0  for i in 0..N { s = s + x }  s }
fn id[T](x: T) -> T { x }
fn main() { print(repeat[4](7))  print(id[Int](99)) }
EOF

# too many generic args → rejected
err_case turbofish_arity "generic arg" <<'EOF'
fn f[comptime N: Int]() -> Int { N }
fn main() { print(f[1, 2]()) }
EOF

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
