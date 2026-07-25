#!/bin/sh
# Freestanding (libc-free) crash backtrace — Feature 8.
# A `--freestanding` build has no execinfo/DWARF, so the backend emits a compact
# symbol table (`jrt_symtab`: generated function address → name) + a frame pointer on
# every generated function, and the runtime walks the rbp chain on a fatal exception,
# naming each frame by its nearest preceding symbol. This pins that a bare-metal crash
# (linked with sel4/bringup.c, run via raw syscalls) prints a backtrace that names the
# faulting Vire/Java function and `main`.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
fj="$root/target/debug/fastjavac"
work="$(mktemp -d)"; pass=0; fail=0
command -v javac >/dev/null 2>&1 || { echo "javac missing — skipping"; exit 0; }
command -v clang >/dev/null 2>&1 || { echo "clang missing — skipping"; exit 0; }
[ -x "$fj" ] || { echo "fastjavac missing — run 'cargo build' first"; exit 1; }

cat > "$work/Crash.java" <<'EOF'
public class Crash {
    static int boom(int[] a, int i) { return a[i]; }
    public static void main(String[] args) {
        int[] a = new int[3];
        System.out.println(boom(a, 10));   // out-of-bounds → fatal
    }
}
EOF
javac -d "$work" "$work/Crash.java" 2>"$work/e" || { echo "FAIL (javac): $(head -1 "$work/e")"; exit 1; }

# Freestanding object → link with the bare-metal bringup shim → run.
if ! "$fj" --freestanding -o "$work/crash.o" "$work/Crash.class" >/dev/null 2>"$work/e"; then
    echo "FAIL (fastjavac --freestanding): $(head -1 "$work/e")"; exit 1
fi
if ! clang -nostdlib -static -fno-stack-protector -ffreestanding -fno-omit-frame-pointer \
        "$work/crash.o" "$root/sel4/bringup.c" -o "$work/crash.bin" 2>"$work/e"; then
    echo "FAIL (link): $(head -1 "$work/e")"; exit 1
fi
out="$("$work/crash.bin" 2>&1)"

check() { # <desc> <grep-pattern>
    if printf '%s\n' "$out" | grep -q "$2"; then echo "ok   $1"; pass=$((pass+1));
    else echo "FAIL $1 (output: $(printf '%s' "$out" | tr '\n' '|'))"; fail=$((fail+1)); fi
}
check "reports the OOB exception"        "ArrayIndexOutOfBoundsException"
check "prints a backtrace"               "^backtrace"
check "names the faulting function boom" "boom"
check "names the entry as main"          "at main"

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
