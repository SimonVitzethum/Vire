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

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
