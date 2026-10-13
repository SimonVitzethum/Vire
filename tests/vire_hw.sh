#!/bin/sh
# Hardware without inline asm (language/HW-SAFE.md H2): declared `syscall`
# items, `Mmio.bind` + readN/writeN against `--hw-profile`, `port_inb/outb`,
# `fence()`. No user-written asm, no integer addresses in user code, no new
# runtime: syscalls go via libc, MMIO/fences are generated per program, port
# helpers are a generated per-program TU. Prozedural statt OO: plain items and
# free functions, no classes.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

check() { # name expected file [extra-flags...]
    name="$1"; want="$2"; file="$3"; shift 3
    if ! "$vire" run "$file" "$@" 2>/dev/null | grep -vE '^verify:' > "$work/got"; then
        echo "FAIL $name (run failed)"; fail=$((fail+1)); return
    fi
    printf '%s\n' "$want" > "$work/want"
    if cmp -s "$work/got" "$work/want"; then echo "ok   $name"; pass=$((pass+1));
    else echo "FAIL $name (want [$want] got [$(cat "$work/got")])"; fail=$((fail+1)); fi
}
build_ok() { # name file [extra-flags...] — must compile (for un-runnable HW)
    name="$1"; file="$2"; shift 2
    if "$vire" build "$file" -o "$work/$name.bin" "$@" >/dev/null 2>"$work/e"; then
        echo "ok   $name"; pass=$((pass+1))
    else echo "FAIL $name (build): $(head -1 "$work/e")"; fail=$((fail+1)); fi
}
reject() { # name pattern file [extra-flags...] — must be rejected with pattern
    name="$1"; want="$2"; file="$3"; shift 3
    if "$vire" build "$file" -o "$work/$name.bin" "$@" >/dev/null 2>"$work/e"; then
        echo "FAIL $name (compiled — must be rejected)"; fail=$((fail+1)); return
    fi
    if grep -q "$want" "$work/e"; then echo "ok   $name"; pass=$((pass+1));
    else echo "FAIL $name (wrong error: $(head -1 "$work/e"))"; fail=$((fail+1)); fi
}
llgrep() { # name pattern file [extra-flags...] — pattern must occur in --emit=llvm
    name="$1"; want="$2"; file="$3"; shift 3
    if "$vire" build "$file" -o "$work/$1.o" --emit=llvm "$@" 2>/dev/null | grep -q "$want"; then
        echo "ok   $name"; pass=$((pass+1))
    else echo "FAIL $name (no [$want] in LLVM)"; fail=$((fail+1)); fi
}

# A profile with a scratch region (addresses never touch real HW here; the
# vsyscall read below faults where the page is unmapped — mechanism proven,
# execution asserted only on safe syscalls).
printf 'uart0 0x10000000 0x100\nvtest 0xffffffffff600000 8\n' > "$work/hw.profile"
PROF="--hw-profile $work/hw.profile"

# --- syscalls execute for real -------------------------------------------
cat > "$work/pid.vr" <<'EOF'
syscall sys_getpid() -> Int = 39
fn main() {
    mut a = sys_getpid()
    mut b = sys_getpid()
    print(a == b)
    print(a > 0)
}
EOF
check "syscall getpid (stable, positive)" "1
1" "$work/pid.vr"

cat > "$work/write.vr" <<'EOF'
syscall sys_write(fd: Int, buf: Array[Byte], n: Int) -> Int = 1
fn main() {
    mut msg = barray(3)
    msg[0] = 104  msg[1] = 105  msg[2] = 10
    mut n = sys_write(1, msg, 3)
    print(n)
}
EOF
# NOTE: run() captures stdout, so the hi\n lands in `got` before the `3`.
check "syscall write via Array[Byte] buffer" "hi
3" "$work/write.vr"

# --- syscall shape rejections (E421/E100) ---------------------------------
cat > "$work/bad7.vr" <<'EOF'
syscall s7(a: Int, b: Int, c: Int, d: Int, e: Int, f: Int, g: Int) -> Int = 99
fn main() { print(s7(1, 2, 3, 4, 5, 6, 7)) }
EOF
reject "syscall 7 params rejected" "E421" "$work/bad7.vr"

cat > "$work/badty.vr" <<'EOF'
syscall sstr(s: Str) -> Int = 99
fn main() { print(sstr("x")) }
EOF
reject "syscall Str param rejected" "E421" "$work/badty.vr"

cat > "$work/badret.vr" <<'EOF'
syscall snoret() = 99
fn main() { print(snoret()) }
EOF
reject "syscall missing ret rejected" "E421" "$work/badret.vr"

cat > "$work/badun.vr" <<'EOF'
syscall sun(x) -> Int = 99
fn main() { print(sun(1)) }
EOF
reject "syscall unannotated param rejected" "E100" "$work/badun.vr"

# --- MMIO: rejects ---------------------------------------------------------
cat > "$work/noprof.vr" <<'EOF'
fn main() {
    mut u = Mmio.bind("uart0")
    print(u.read32(0))
}
EOF
reject "bind without profile rejected" "E400" "$work/noprof.vr"

cat > "$work/unknown.vr" <<'EOF'
fn main() {
    mut u = Mmio.bind("nope")
    print(u.read32(0))
}
EOF
reject "bind unknown region rejected" "E400.*unknown hardware region" "$work/unknown.vr" $PROF

cat > "$work/oob.vr" <<'EOF'
fn main() {
    mut u = Mmio.bind("uart0")
    print(u.read32(253))
}
EOF
reject "offset+width past end rejected" "E400" "$work/oob.vr" $PROF

cat > "$work/dynoff.vr" <<'EOF'
fn main() {
    mut u = Mmio.bind("uart0")
    mut i = 4
    print(u.read32(i))
}
EOF
reject "computed offset rejected" "E400" "$work/dynoff.vr" $PROF

cat > "$work/badop.vr" <<'EOF'
fn main() {
    mut u = Mmio.bind("uart0")
    print(u.read33(0))
}
EOF
reject "unknown MMIO op rejected" "E400" "$work/badop.vr" $PROF

# --- MMIO: codegen properties (volatile + descriptor, no asm/runtime) -------
cat > "$work/rw.vr" <<'EOF'
fn main() {
    mut u = Mmio.bind("uart0")
    u.write32(0, 96)
    mut v = u.read32(0)
    mut w = u.read8(7)
    print(v + w)
}
EOF
llgrep "volatile load emitted" "load volatile i32" "$work/rw.vr" $PROF
llgrep "volatile store emitted" "store volatile i32" "$work/rw.vr" $PROF
llgrep "immortal descriptor emitted" "@.mmio.uart0 = internal unnamed_addr constant { i64, ptr, i64, i64 } { i64 -1" "$work/rw.vr" $PROF
build_ok "mmio program links" "$work/rw.vr" $PROF

# --- fences + ports: compile, link, fence executes --------------------------
cat > "$work/fence.vr" <<'EOF'
fn main() {
    fence()
    fence_acq()
    fence_rel()
    print(7)
}
EOF
check "fences execute (no-op)" "7" "$work/fence.vr"

cat > "$work/port.vr" <<'EOF'
fn main() {
    port_outb(1024, 0)
    fence()
    print(3)
}
EOF
build_ok "port_outb links (needs IOPL to run — compile-only)" "$work/port.vr"

cat > "$work/portbad.vr" <<'EOF'
fn main() { port_outb(1) }
EOF
reject "port_outb arity rejected" "E421" "$work/portbad.vr"

cat > "$work/fencebad.vr" <<'EOF'
fn main() { fence(1) }
EOF
reject "fence arity rejected" "E421" "$work/fencebad.vr"

# --- QEMU execution: volatile accessors really run ---------------------------
# A tmpfs file stands in for the device, mmap'd MAP_FIXED at the profile base
# by an LD_PRELOAD shim (constructor) — under qemu-x86_64 (user-mode) AND
# natively. The Vire program reads/writes through the generated volatile
# accessors; the harness checks program output AND the file bytes (write path).
# This executes the exact emitted machine code under QEMU.
if command -v qemu-x86_64 >/dev/null 2>&1 && command -v clang >/dev/null 2>&1; then
    QEMU_BASE=0x4000000000
    QEMU_LEN=8192
    printf 'qdev 0x4000000000 8192\n' > "$work/qemu.profile"
    dd if=/dev/zero of="$work/qdev.bin" bs=8192 count=1 status=none
    cat > "$work/preload.c" <<'EOF'
#define _GNU_SOURCE
#include <sys/mman.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include <stdio.h>
__attribute__((constructor)) static void go(void) {
    const char *f = getenv("VIRE_HW_FILE");
    const char *b = getenv("VIRE_HW_BASE");
    const char *l = getenv("VIRE_HW_LEN");
    if (!f || !b || !l) return;
    unsigned long base = strtoul(b, 0, 0), len = strtoul(l, 0, 0);
    int fd = open(f, O_RDWR);
    if (fd < 0) _exit(41);
    void *p = mmap((void*)base, len, PROT_READ|PROT_WRITE, MAP_SHARED|MAP_FIXED, fd, 0);
    if (p == MAP_FAILED) _exit(42);
}
EOF
    if clang -shared -fPIC -O2 "$work/preload.c" -o "$work/pre.so" 2>/dev/null; then
        cat > "$work/qmmio.vr" <<'EOF'
fn main() {
    mut d = Mmio.bind("qdev")
    d.write32(0, 305419896)
    d.write8(4, 66)
    d.write16(6, 512)
    d.write64(4096, 123456789)
    mut a = d.read32(0)
    mut b = d.read8(4)
    mut c = d.read16(6)
    mut e = d.read64(4096)
    print(a)
    print(b)
    print(c)
    print(e)
}
EOF
        if "$vire" build "$work/qmmio.vr" -o "$work/qmmio" --hw-profile "$work/qemu.profile" >/dev/null 2>"$work/e"; then
            export VIRE_HW_FILE="$work/qdev.bin" VIRE_HW_BASE="$QEMU_BASE" VIRE_HW_LEN="$QEMU_LEN"
            export LD_PRELOAD="$work/pre.so"
            got="$(qemu-x86_64 "$work/qmmio" 2>/dev/null)"
            unset LD_PRELOAD; unset VIRE_HW_FILE VIRE_HW_BASE VIRE_HW_LEN
            if [ "$got" = "$(printf '305419896\n66\n512\n123456789')" ]; then
                echo "ok   qemu-user MMIO roundtrip (volatile R/W under QEMU)"; pass=$((pass+1))
            else
                echo "FAIL qemu-user MMIO roundtrip (got [$got])"; fail=$((fail+1))
            fi
            # Write path cross-check: the file itself must carry the pattern
            # (little-endian 0x12345678 at 0, 'B' at 4).
            if [ "$(od -A d -t u1 -j 0 -N 5 "$work/qdev.bin" | head -1 | awk '{print $2, $3, $4, $5, $6}')" = "120 86 52 18 66" ]; then
                echo "ok   qemu-user MMIO write reached the device file"; pass=$((pass+1))
            else
                echo "FAIL qemu-user MMIO write bytes: $(od -A d -t u1 -j 0 -N 5 "$work/qdev.bin" | head -1)"; fail=$((fail+1))
            fi
        else
            echo "FAIL qemu-user MMIO build: $(head -1 "$work/e")"; fail=$((fail+1))
        fi
    else
        echo "skip qemu-user MMIO (preload shim would not build)"
    fi
else
    echo "skip qemu-user MMIO (no qemu-x86_64 or clang)"
fi

# --- qemu-system (KVM): the SAME test in a full guest -------------------------
# Custom initramfs (test binary + preload shim + device file + profile + libc)
# with a static /init launcher; the guest kernel runs it on KVM. Asserts the
# guest output over the serial console. Needs /dev/kvm + kernel + cpio.
if [ -c /dev/kvm ] && command -v qemu-system-x86_64 >/dev/null 2>&1 && command -v cpio >/dev/null 2>&1 \
   && [ -f /boot/vmlinuz-linux ] && [ -f "$work/qmmio" ]; then
    rm -rf "$work/guest" && mkdir -p "$work/guest"
    cp "$work/qmmio" "$work/pre.so" "$work/qdev.bin" "$work/qemu.profile" "$work/guest/"
    for lib in /lib64/ld-linux-x86-64.so.2 /usr/lib/libc.so.6 /usr/lib/libm.so.6; do
        mkdir -p "$work/guest/$(dirname "$lib")"
        cp "$lib" "$work/guest/$lib" 2>/dev/null || true
    done
    cat > "$work/guest_init.c" <<'EOF'
#define _GNU_SOURCE
#include <sys/mman.h>
#include <sys/reboot.h>
#include <sys/wait.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
extern char **environ;
int main(void) {
    int fd = open("/qdev.bin", O_RDWR);
    if (fd < 0) return 41;
    void *p = mmap((void*)0x4000000000UL, 8192, PROT_READ|PROT_WRITE,
                   MAP_SHARED|MAP_FIXED, fd, 0);
    if (p == MAP_FAILED) return 42;
    pid_t c = fork();
    if (c == 0) {
        setenv("LD_PRELOAD", "/pre.so", 1);
        setenv("VIRE_HW_FILE", "/qdev.bin", 1);
        setenv("VIRE_HW_BASE", "0x4000000000", 1);
        setenv("VIRE_HW_LEN", "8192", 1);
        char *const av[] = { "/qmmio", 0 };
        execve("/qmmio", av, environ);
        return 43;
    }
    int st = 0;
    waitpid(c, &st, 0);
    sync();
    reboot(RB_POWER_OFF);
    return 44;
}
EOF
    if clang -static -O2 "$work/guest_init.c" -o "$work/guest/init" 2>/dev/null \
       && (cd "$work/guest" && find . | cpio -o -H newc > "$work/guest.cpio" 2>/dev/null); then
        timeout 60 qemu-system-x86_64 -enable-kvm -m 512 -nographic -no-reboot \
            -kernel /boot/vmlinuz-linux \
            -initrd "$work/guest.cpio" \
            -append "console=ttyS0 rdinit=/init panic=5" \
            -serial file:"$work/guest.log" >/dev/null 2>&1 || true
        if grep -q "305419896" "$work/guest.log" 2>/dev/null && grep -q "123456789" "$work/guest.log" 2>/dev/null; then
            echo "ok   qemu-system MMIO roundtrip in guest (KVM)"; pass=$((pass+1))
        else
            echo "FAIL qemu-system MMIO (guest log tail: $(tail -c 300 "$work/guest.log" 2>/dev/null | tr '\n' '|'))"; fail=$((fail+1))
        fi
    else
        echo "skip qemu-system MMIO (initramfs build failed)"
    fi
else
    echo "skip qemu-system MMIO (needs /dev/kvm + kernel + cpio)"
fi

# --- sockets in userspace: socketpair/send/recv/close over AF_UNIX --------
cat > "$work/sock.vr" <<'EOF'
syscall sys_socketpair(domain: Int, typ: Int, proto: Int, fds: Array[Byte]) -> Int = 53
syscall sys_sendto(fd: Int, buf: Array[Byte], n: Int, flags: Int, addr: Int, addrlen: Int) -> Int = 44
syscall sys_recvfrom(fd: Int, buf: Array[Byte], n: Int, flags: Int, addr: Int, addrlen: Int) -> Int = 45
syscall sys_close(fd: Int) -> Int = 3

fn read_i32_le(a: Array[Byte], off: Int) -> Int {
    a[off] + a[off + 1] * 256 + a[off + 2] * 65536 + a[off + 3] * 16777216
}

fn main() {
    mut fds = barray(8)
    print(sys_socketpair(1, 1, 0, fds))
    mut fd0 = read_i32_le(fds, 0)
    mut fd1 = read_i32_le(fds, 4)
    mut msg = barray(5)
    msg[0] = 104  msg[1] = 101  msg[2] = 108  msg[3] = 108  msg[4] = 111
    print(sys_sendto(fd0, msg, 5, 0, 0, 0))
    mut out = barray(5)
    print(sys_recvfrom(fd1, out, 5, 0, 0, 0))
    mut ok = 1
    mut i = 0
    while i < 5 {
        if out[i] != msg[i] { ok = 0 }
        i = i + 1
    }
    print(ok)
    print(sys_close(fd0))
    print(sys_close(fd1))
}
EOF
check "socketpair/send/recv/close roundtrip" "0
5
5
1
0
0" "$work/sock.vr"

# --- E422: handles never cross a capsule boundary ---------------------------
cat > "$work/capin.vr" <<'EOF'
fn main() {
    mut u = Mmio.bind("uart0")
    mut x = capsule(u) { 1 }
    print(x)
}
EOF
reject "capsule input handle rejected" "E422" "$work/capin.vr" $PROF

cat > "$work/capout.vr" <<'EOF'
fn main() {
    mut x = capsule() {
        mut u = Mmio.bind("uart0")
        u
    }
    print(0)
}
EOF
reject "capsule result handle rejected" "E422" "$work/capout.vr" $PROF

echo "---"
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
