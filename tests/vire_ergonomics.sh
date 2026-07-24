#!/bin/sh
# Ergonomics (C1–C4): string interpolation "{expr}", `else` on its own line,
# if/else in statement position with mixed branch types, and source spans on
# inference diagnostics. All were friction points found while writing real code.
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
errck() { # name pattern file — the compile must emit `pattern` (with a line:col)
    if "$vire" run "$3" 2>&1 | grep -q "$2"; then echo "ok   $1"; pass=$((pass+1));
    else echo "FAIL $1 (no '$2': $("$vire" run "$3" 2>&1 | head -1))"; fail=$((fail+1)); fi
}

# --- C1: string interpolation ------------------------------------------------
cat > "$work/interp.vr" <<'EOF'
fn main() {
    mut x = 42
    mut name = "Ann"
    print("x is {x}!")                    // x is 42!
    print("{name} has {x - 2} pts")       // Ann has 40 pts
    print("literal {{braces}} and {}")    // literal {braces} and {}  ({} stays for the logger)
    print("plain")                        // plain
}
EOF
check "C1 string interpolation (expr + escapes + empty {} passthrough)" "x is 42!
Ann has 40 pts
literal {braces} and {}
plain" "$work/interp.vr"

# --- C2: `else` / `elif` may start on the line after `}` ---------------------
cat > "$work/elseline.vr" <<'EOF'
fn classify(x: Int) -> Int {
    if x > 10 {
        2
    }
    elif x > 0 {
        1
    }
    else {
        0
    }
}
fn main() {
    print(classify(50))   // 2
    print(classify(5))    // 1
    print(classify(0 - 3))// 0
}
EOF
check "C2 else/elif on its own line" "2
1
0" "$work/elseline.vr"

# --- C3: if/else in STATEMENT position needs no branch-type agreement --------
cat > "$work/stmtif.vr" <<'EOF'
fn f(x: Int) -> Int {
    if x > 0 { print(1) } else { 5 }   // value discarded → Unit/Int mix is fine
    x
}
fn main() { print(f(9)) }
EOF
check "C3 statement-position if/else (mixed branch types ok)" "1
9" "$work/stmtif.vr"

# C3 must still REJECT a mismatched if whose VALUE is used (with a span).
cat > "$work/stmtif_bad.vr" <<'EOF'
fn main() { mut z = if true { 1 } else { print(9) }  print(z) }
EOF
errck "C3 used-value mismatch still rejected (with span)" "conflict: Int vs Unit" "$work/stmtif_bad.vr"

# --- C4: inference conflicts carry a source line:col -------------------------
cat > "$work/span.vr" <<'EOF'
fn f(x: Int) -> Int {
    mut y = x
    y = true
    y
}
fn main() { print(f(1)) }
EOF
errck "C4 type conflict has line:col" "Error 3:" "$work/span.vr"

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
