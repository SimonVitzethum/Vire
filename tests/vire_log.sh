#!/bin/sh
# Logger suite (Feature 6): compile-time level filter + structured `{}` fields.
# Disabled levels lower to zero instructions; the build-time level is `--log-level`.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

check() { # name expected file [extra vire args...]
    name="$1"; want="$2"; f="$3"; shift 3
    got="$("$vire" run "$@" "$f" 2>/dev/null)"
    if [ "$got" = "$want" ]; then echo "ok   $name"; pass=$((pass+1))
    else echo "FAIL $name (want [$want] got [$got])"; fail=$((fail+1)); fi
}

cat > "$work/fields.vr" <<'EOF'
fn main() {
    mut id = 42
    mut ms = 17
    log.info("login user={} ms={}", id, ms)
    log.warn("disk {}%", 5)
    log.info("plain")
    log.debug("hidden: {}", id)
}
EOF
check "structured fields, default level" "[INFO] login user=42 ms=17
[WARN] disk 5%
[INFO] plain" "$work/fields.vr"

# --log-level debug reveals the debug line too.
check "log-level debug reveals debug" "[INFO] login user=42 ms=17
[WARN] disk 5%
[INFO] plain
[DEBUG] hidden: 42" "$work/fields.vr" --log-level debug

# --log-level off suppresses everything (zero output).
check "log-level off suppresses all" "" "$work/fields.vr" --log-level off

# --log-level warn drops info/debug, keeps warn.
check "log-level warn keeps warn only" "[WARN] disk 5%" "$work/fields.vr" --log-level warn

# A placeholder/argument count mismatch is a compile error.
cat > "$work/bad.vr" <<'EOF'
fn main() { log.info("a={} b={}", 1) }
EOF
if "$vire" run "$work/bad.vr" 2>&1 | grep -q 'placeholder'; then
    echo "ok   placeholder/arg mismatch rejected"; pass=$((pass+1))
else
    echo "FAIL placeholder/arg mismatch rejected"; fail=$((fail+1))
fi

# `with log.span(k, v, …)` — scoped context prepended to every log inside; nests
# and unwinds (compile-time only, zero runtime cost).
cat > "$work/span.vr" <<'EOF'
fn main() {
    mut rid = 7
    log.info("start")
    with log.span("req", rid) {
        log.info("a")
        with log.span("user", "ann") { log.warn("slow {}ms", 12) }
        log.info("b")
    }
    log.info("end")
}
EOF
check "with log.span (nested context, unwinds)" "[INFO] start
[INFO] [req=7] a
[WARN] [req=7 user=ann] slow 12ms
[INFO] [req=7] b
[INFO] end" "$work/span.vr"

# `--log-color` sink: the level tag is wrapped in an ANSI colour (opt-in; plain
# otherwise). Check the escape appears (and NOT without the flag).
if "$vire" run --log-color "$work/span.vr" 2>/dev/null | grep -q "$(printf '\033')\[32m\[INFO\]"; then
    echo "ok   --log-color emits ANSI level colour"; pass=$((pass+1))
else
    echo "FAIL --log-color ANSI"; fail=$((fail+1))
fi
if "$vire" run "$work/span.vr" 2>/dev/null | grep -q "$(printf '\033')"; then
    echo "FAIL plain output contains ANSI"; fail=$((fail+1))
else
    echo "ok   default output stays plain"; pass=$((pass+1))
fi

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
