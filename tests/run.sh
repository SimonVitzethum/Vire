#!/bin/sh
# Regression test runner: compiles each example with javac + fastjavac,
# runs it and checks exit code as well as heap balance (0 live). The compiler
# works closed-world, so exactly the necessary classes are passed per test;
# the java.util stubs when needed.
#
# Usage:  sh tests/run.sh   (from the project root directory)
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
ex="$root/examples"
fastjavac="$root/target/debug/fastjavac"
stdlib="$root/stdlib/out/java/util/*.class $root/stdlib/out/java/util/function/*.class $root/stdlib/out/java/util/stream/*.class"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

[ -x "$fastjavac" ] || { echo "fastjavac missing — run 'cargo build' first"; exit 1; }
sh "$root/stdlib/build.sh" >/dev/null 2>&1

pass=0; fail=0

# test <name> <exit-expected> <mainclass> <classes…with optional @stdlib>
# Helper classes can be inline (in the main file) or in their own files;
# we compile the main file and pull missing classes from
# identically-named files.
run() {
    name="$1"; want="$2"; main="$3"; shift 3
    usestd=0
    rm -f "$work"/*.class
    # Sources to compile: Main + optional "+file" tokens (for classes
    # that are inline in a differently-named file). -sourcepath pulls in
    # identically-named classes automatically.
    srcs="$ex/$main.java"
    for a in "$@"; do
        case "$a" in +*) srcs="$srcs $ex/${a#+}.java";; esac
    done
    if ! javac -sourcepath "$ex" -d "$work" $srcs >/dev/null 2>"$work/err"; then
        echo "FAIL $name (javac): $(head -1 "$work/err")"; fail=$((fail+1)); return
    fi
    classes="$work/$main.class"
    # Automatically include synthetic/inner classes of the main class (e.g. the
    # enum-switch $SwitchMap helper Main$1) as closed-world input.
    for f in "$work/$main"\$*.class; do
        [ -e "$f" ] && classes="$classes $f"
    done
    for a in "$@"; do
        case "$a" in
            @stdlib) usestd=1; continue;;
            +*) continue;;
            "$main") continue;;
        esac
        classes="$classes $work/$a.class"
    done
    [ $usestd -eq 1 ] && classes="$classes $stdlib"
    if ! $fastjavac -o "$work/$main.bin" $classes >/dev/null 2>"$work/err"; then
        echo "FAIL $name (fastjavac): $(head -1 "$work/err")"; fail=$((fail+1)); return
    fi
    out="$(FASTLLVM_HEAPSTATS=1 "$work/$main.bin" 2>&1)"; code=$?
    if [ "$code" != "$want" ]; then
        echo "FAIL $name (exit $code, expected $want)"; fail=$((fail+1)); return
    fi
    # Heap balance: if a [heap] line is present, it must show 0 live
    # (except on abrupt exit != 0, where stack cleanup is skipped).
    if [ "$code" = "0" ] && echo "$out" | grep -q '\[heap\]' && ! echo "$out" | grep -q '0 still live'; then
        echo "FAIL $name (heap leak): $(echo "$out" | grep '\[heap\]')"; fail=$((fail+1)); return
    fi
    echo "ok   $name"; pass=$((pass+1))
}

# --- Basics ---
run hello         0 Hello Hello
run arith         1 Arith Arith            # uncaught ArithmeticException
run stack         0 Stack Stack Point
run app           0 App App Lib

# --- Objects / Inheritance / Interfaces ---
run shapes        0 Shapes Shapes Shape Circle Rect
run mono          1 Mono Shape Circle Rect +Shapes
run interfaces    0 Interfaces Interfaces Animal Named Dog Bird
run equals        0 Equals Equals Point Plain

# --- Memory management ---
run rc            0 Rc Rc Box
run cycle         0 Cycle Cycle Box
run cycle3        0 Cycle3 Cycle3 Box
run cyclestress   0 CycleStress CycleStress

# --- Arrays / Numbers / Strings ---
run arr2          0 Arr2 Arr2 Box
run bounds        0 Bounds Bounds
run nums          0 Nums Nums
run floats        0 Floats Floats
run strings       0 Strings Strings
run concat        0 Concat Concat
run sb            0 SB SB

# --- Reflection ---
run refl          0 Refl Refl Widget
run refl2         0 Refl2 Refl2 Animal Dog
run inner         0 Inner Inner

# --- Exceptions ---
run exc           0 Exc Exc MyException
run catch         0 Catch Catch ErrorA ErrorB ErrorC
run finally       0 Finally Finally MyException
run arrexc        0 ArrExc ArrExc
run npeexc        0 NpeExc NpeExc Node
run strnpe        0 StrNpe StrNpe
run arith2        0 Arith2 Arith2

# --- Autoboxing ---
run boxing        0 Boxing Boxing MiniHashMap
run boxing2       0 Boxing2 Boxing2 MiniHashMap

# --- Collections (Mini) ---
run collections   0 Collections Collections MiniList Box
run maps          0 Maps Maps MiniMap
run hashmaps      0 HashMaps HashMaps MiniHashMap

# --- Lambdas / Method references / Streams ---
run lambdas       0 Lambdas Lambdas IntOp IntBiOp
run methodref     0 MethodRef MethodRef IntBiOp StrLen Maker Box MathU
run unbox         0 Unbox Unbox U IntF StrF @stdlib

# --- real java.util (stubs) ---
run foreach       0 ForEach ForEach @stdlib
run stdlibdemo    0 StdlibDemo StdlibDemo @stdlib
run colldemo      0 CollDemo CollDemo @stdlib
run streams       0 Streams Streams @stdlib
run streams2      0 Streams2 Streams2 @stdlib
run intstreams    0 IntStreams IntStreams @stdlib
run arraysdemo   0 ArraysDemo ArraysDemo @stdlib

# --- Language features ---
run switch        0 Switch Switch
run format        0 Format Format
run enum          0 Enum1 Enum1 Color
run twr           0 Twr Twr Res MyException
run messages      0 Messages Messages Boom
run enumswitch    0 EnumSwitch EnumSwitch Dir
run escfields     0 EscapeFields EscapeFields Node2
run ipesc         0 IpEsc IpEsc Vec2
run loopcarry     0 LoopCarry LoopCarry Node
run benchalloc    0 BenchAlloc BenchAlloc Node
run intrinsics    0 Intrinsics Intrinsics
run primarr       0 PrimArr PrimArr
run cmp           0 Cmp Cmp
run seal          0 Seal Seal
run rec           0 Rec Rec
run genmax        0 GenMax GenMax @stdlib
run sync          0 Sync Sync
run threads_seq   0 Threads Threads
run strs          0 Strs Strs

# --- JAR ingestion: classes + manifest Main-Class from a JAR ---
jartest() {
    name="$1"; want="$2"; main="$3"; shift 3
    rm -f "$work"/*.class "$work"/app.jar
    srcs="$ex/$main.java"
    if ! javac -sourcepath "$ex" -d "$work" $srcs >/dev/null 2>"$work/err"; then
        echo "FAIL $name (javac): $(head -1 "$work/err")"; fail=$((fail+1)); return
    fi
    printf 'Main-Class: %s\n' "$main" > "$work/manifest.txt"
    ( cd "$work" && jar cfm app.jar manifest.txt *.class >/dev/null 2>&1 )
    if ! $fastjavac -o "$work/$main.bin" "$work/app.jar" >/dev/null 2>"$work/err"; then
        echo "FAIL $name (fastjavac): $(head -1 "$work/err")"; fail=$((fail+1)); return
    fi
    out="$(FASTLLVM_HEAPSTATS=1 "$work/$main.bin" 2>&1)"; code=$?
    if [ "$code" != "$want" ]; then
        echo "FAIL $name (exit $code, expected $want)"; fail=$((fail+1)); return
    fi
    if [ "$code" = "0" ] && echo "$out" | grep -q '\[heap\]' && ! echo "$out" | grep -q '0 still live'; then
        echo "FAIL $name (heap leak)"; fail=$((fail+1)); return
    fi
    echo "ok   $name"; pass=$((pass+1))
}
jartest jar          0 Shapes

# --- Freestanding/seL4: libc-free object, linked with bare-metal shim ---
fstest() {
    name="$1"; want="$2"; main="$3"; shift 3
    rm -f "$work"/*.class "$work/app.o" "$work/app_fs"
    if ! javac -sourcepath "$ex" -d "$work" "$ex/$main.java" >/dev/null 2>"$work/err"; then
        echo "FAIL $name (javac)"; fail=$((fail+1)); return
    fi
    cls="$work/$main.class"; for c in "$@"; do cls="$cls $work/$c.class"; done
    if ! $fastjavac --freestanding -o "$work/app.o" $cls >/dev/null 2>"$work/err"; then
        echo "FAIL $name (fastjavac): $(head -1 "$work/err")"; fail=$((fail+1)); return
    fi
    # no libc undef in the object?
    if nm -u "$work/app.o" 2>/dev/null | grep -qiE "printf|malloc|calloc| free|fwrite|__stack"; then
        echo "FAIL $name (libc undef in freestanding object)"; fail=$((fail+1)); return
    fi
    if ! clang -nostdlib -static -fno-stack-protector -ffreestanding \
            "$root/sel4/bringup.c" "$work/app.o" -o "$work/app_fs" 2>"$work/err"; then
        echo "FAIL $name (link): $(head -1 "$work/err")"; fail=$((fail+1)); return
    fi
    out="$("$work/app_fs" 2>&1)"; code=$?
    if [ "$code" != "$want" ]; then
        echo "FAIL $name (exit $code, expected $want)"; fail=$((fail+1)); return
    fi
    echo "ok   $name"; pass=$((pass+1))
}
fstest freestanding  0 Cycle Box

# --- Real concurrency: --threads (pthreads + monitor + atomic RC) ---
thtest() {
    name="$1"; want_out="$2"; main="$3"; shift 3
    rm -f "$work"/*.class "$work/th.bin"
    if ! javac -sourcepath "$ex" -d "$work" "$ex/$main.java" >/dev/null 2>"$work/err"; then
        echo "FAIL $name (javac)"; fail=$((fail+1)); return
    fi
    if ! $fastjavac --threads -o "$work/th.bin" "$work"/*.class >/dev/null 2>"$work/err"; then
        echo "FAIL $name (fastjavac): $(head -1 "$work/err")"; fail=$((fail+1)); return
    fi
    out="$(FASTLLVM_HEAPSTATS=1 "$work/th.bin" 2>&1)"; code=$?
    got="$(echo "$out" | grep -v '\[heap\]' | head -1)"
    if [ "$got" != "$want_out" ]; then
        echo "FAIL $name (output '$got', expected '$want_out' — race?)"; fail=$((fail+1)); return
    fi
    if [ "$code" = "0" ] && echo "$out" | grep -q '\[heap\]' && ! echo "$out" | grep -q '0 still live'; then
        echo "FAIL $name (heap leak)"; fail=$((fail+1)); return
    fi
    echo "ok   $name"; pass=$((pass+1))
}
thtest threads_par   200000 Threads

echo "---"
echo "$pass passed, $fail failed"
[ $fail -eq 0 ]
