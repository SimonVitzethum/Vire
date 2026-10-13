#!/bin/sh
# Go-style error handling: Result[T,E], `?` propagation, `.wrap(msg)` (context + chain),
# and `.error()` (the message, Go's err.Error()). Errors are string messages (the common
# case); typed sum-type errors go through `match`. Exercises the backend fix that stores a
# pointer payload (a string literal) into the i64-erased Result slot via ptrtoint.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

check() { # name expected file
    got="$("$vire" run "$3" 2>/dev/null)"
    if [ "$got" = "$2" ]; then echo "ok   $1"; pass=$((pass+1));
    else echo "FAIL $1 (want [$2] got [$got])"; fail=$((fail+1)); fi
}

# --- .wrap adds context, ? propagates, .error() reads the message ---------
cat > "$work/wrap.vr" <<'EOF'
fn read_file(ok: Bool) -> Result[Str, Str] {
    if ok { Ok("contents") } else { Err("file not found") }
}
fn load(ok: Bool) -> Result[Str, Str] {
    mut raw = read_file(ok).wrap("Config app.cfg")?   // ? returns early on Err, with context
    Ok(raw)
}
fn main() {
    print(load(false).error())          // Config app.cfg: file not found
    print(load(true).error())           // (empty — Ok has no error)
    // nested chain: outermost wrap first in the message
    print(read_file(false).wrap("outer").wrap("outermost").error())
}
EOF
check ".wrap context + ? + .error()" "Config app.cfg: file not found

outermost: outer: file not found" "$work/wrap.vr"

# --- match still branches on Ok/Err (typed handling) --------------------
cat > "$work/match.vr" <<'EOF'
fn parse(n: Int) -> Result[Int, Str] {
    if n > 0 { Ok(n + n) } else { Err("must be positive") }
}
fn main() {
    match parse(21) { Ok(v) -> print(v)  Err(e) -> print(e) }   // 42
    match parse(0)  { Ok(v) -> print(v)  Err(e) -> print(0) }   // 0 (Err branch)
    print(parse(0).error())                                     // must be positive
}
EOF
check "Result match Ok/Err + .error()" "42
0
must be positive" "$work/match.vr"

# --- typed payloads: Err(e) binds typed, ? propagates typed ----------------
cat > "$work/typed.vr" <<'EOF'
type MyErr { code: Int }
fn parse(s: Str) -> Result[Int, MyErr] {
    if s == "ok" { Ok(42) } else { Err(MyErr(7)) }
}
fn doubled(s: Str) -> Result[Int, MyErr] {
    mut v = parse(s)?
    Ok(v + v)
}
fn main() {
    match parse("ok") { Ok(v) -> print(v)  Err(e) -> print(0 - 1) }
    match parse("xx") { Ok(v) -> print(0 - 1)  Err(e) -> print(e.code) }
    match doubled("ok") { Ok(v) -> print(v)  Err(e) -> print(0 - 1) }
    match doubled("xx") { Ok(v) -> print(0 - 1)  Err(e) -> print(e.code + 100) }
}
EOF
check "typed Result payloads (? + match + field)" "42
7
84
107" "$work/typed.vr"

cat > "$work/floatres.vr" <<'EOF'
fn half(x: Float) -> Result[Float, Str] {
    if x > 0.0 { Ok(x / 2.0) } else { Err("negative") }
}
fn main() {
    match half(9.0) { Ok(v) -> print(v)  Err(e) -> print(e) }
    print(half(0.0 - 1.0).error())
}
EOF
check "Float Result payloads" "4.5
negative" "$work/floatres.vr"

cat > "$work/annres.vr" <<'EOF'
fn main() {
    mut r: Result[Int, Str] = Ok(5)
    match r { Ok(v) -> print(v + 1)  Err(e) -> print(e) }
}
EOF
check "annotated Result construction" "6" "$work/annres.vr"

# --- mismatches are loud, not miscompiled ------------------------------------
cat > "$work/mismatch.vr" <<'EOF'
fn f() -> Result[Int, Str] { Err("x") }
fn g() -> Result[Int, Int] {
    mut v = f()?
    Ok(v)
}
fn main() { print(0) }
EOF
if "$vire" build "$work/mismatch.vr" -o "$work/mismatch.bin" >/dev/null 2>"$work/e"; then
    echo "FAIL ? E-mismatch (compiled — must be rejected)"; fail=$((fail+1))
elif grep -q "error types differ" "$work/e"; then
    echo "ok   ? E-mismatch rejected"; pass=$((pass+1))
else
    echo "FAIL ? E-mismatch (wrong error: $(head -1 "$work/e"))"; fail=$((fail+1))
fi

cat > "$work/wraperr.vr" <<'EOF'
type MyErr { code: Int }
fn f() -> Result[Int, MyErr] { Err(MyErr(1)) }
fn main() { print(f().wrap("ctx").error()) }
EOF
if "$vire" build "$work/wraperr.vr" -o "$work/wraperr.bin" >/dev/null 2>"$work/e"; then
    echo "FAIL wrap-non-Str (compiled — must be rejected)"; fail=$((fail+1))
elif grep -q "needs \`E = Str\`" "$work/e"; then
    echo "ok   wrap-non-Str rejected"; pass=$((pass+1))
else
    echo "FAIL wrap-non-Str (wrong error: $(head -1 "$work/e"))"; fail=$((fail+1))
fi

cat > "$work/uninf.vr" <<'EOF'
fn main() {
    mut r = Ok(5)
    print(0)
}
EOF
if "$vire" build "$work/uninf.vr" -o "$work/uninf.bin" >/dev/null 2>"$work/e"; then
    echo "FAIL uninferred-E (compiled — must be rejected)"; fail=$((fail+1))
elif grep -q "cannot infer \`E\` of \`Ok\`" "$work/e"; then
    echo "ok   uninferred-E rejected (no silent Int)"; pass=$((pass+1))
else
    echo "FAIL uninferred-E (wrong error: $(head -1 "$work/e"))"; fail=$((fail+1))
fi

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
