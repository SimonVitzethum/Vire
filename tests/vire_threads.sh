#!/bin/sh
# Vire concurrency suite: `spawn` + `join` + `Atomic`, safe by construction.
# A correct atomic must give a deterministic total across threads (a data race
# would lose increments); the Send check must reject sharing bare mutable state.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
work="$(mktemp -d)"; pass=0; fail=0
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

ok_case() {   # ok_case <name> <expected> [runs] <<vr
    name="$1"; want="$2"; runs="${3:-1}"; f="$work/$name.vr"; cat > "$f"
    if ! "$vire" build "$f" -o "$work/$name.bin" >/dev/null 2>"$work/e"; then
        echo "FAIL $name (build): $(head -1 "$work/e")"; fail=$((fail+1)); return
    fi
    i=0
    while [ "$i" -lt "$runs" ]; do
        out="$("$work/$name.bin" 2>/dev/null | grep -vE '^verify:')"
        if [ "$out" != "$want" ]; then
            echo "FAIL $name (run $i: got '$out', want '$want' — race?)"; fail=$((fail+1)); return
        fi
        i=$((i+1))
    done
    echo "ok   $name (x$runs)"; pass=$((pass+1))
}

err_case() {  # err_case <name> <needle> <<vr
    name="$1"; needle="$2"; f="$work/$name.vr"; cat > "$f"; rm -f "$work/$name.bin"
    err="$("$vire" build "$f" -o "$work/$name.bin" 2>&1)"
    if [ -f "$work/$name.bin" ]; then echo "FAIL $name (built — should reject)"; fail=$((fail+1))
    elif echo "$err" | grep -q "$needle"; then echo "ok   $name ($needle)"; pass=$((pass+1))
    else echo "FAIL $name (missing '$needle': $(echo "$err" | head -1))"; fail=$((fail+1)); fi
}

# shared Atomic counter across two threads — deterministic total (no lost adds).
# Repeated to make a race, if any, surface.
ok_case atomic_counter 200000 20 <<'EOF'
fn worker(c: Atomic) -> Int {
    for i in 0..100000 { c.fetch_add(1) }
    0
}
fn main() {
    mut c = Atomic(0)
    mut h1 = spawn worker(c)
    mut h2 = spawn worker(c)
    join(h1)
    join(h2)
    print(c.load())
}
EOF

# scalar argument spawn + join returning the worker result
ok_case scalar_spawn 85 <<'EOF'
fn sq(n: Int) -> Int { n * n }
fn main() { mut a = spawn sq(6)  mut b = spawn sq(7)  print(join(a) + join(b)) }
EOF

# multi-argument worker (id + shared Atomic), packed via an env buffer
ok_case multi_arg_spawn 300000 20 <<'EOF'
fn worker(id: Int, c: Atomic) -> Int {
    mut i = 0
    while i < 50000 { c.fetch_add(id)  i = i + 1 }
    0
}
fn main() {
    mut c = Atomic(0)
    mut h1 = spawn worker(1, c)
    mut h2 = spawn worker(2, c)
    mut h3 = spawn worker(3, c)
    join(h1)  join(h2)  join(h3)
    print(c.load())
}
EOF

# Mutex: lock-guarded read-modify-write is race-free (a bare += would lose adds)
ok_case mutex_guard 20000 20 <<'EOF'
fn worker(m: Mutex) -> Int {
    mut i = 0
    while i < 10000 { m.lock()  m.set(m.get() + 1)  m.unlock()  i = i + 1 }
    0
}
fn main() {
    mut m = Mutex(0)
    mut h1 = spawn worker(m)
    mut h2 = spawn worker(m)
    join(h1)  join(h2)
    print(m.get())
}
EOF

# fetch_add returns the previous value
ok_case fetch_add_prev 0 <<'EOF'
fn main() { mut c = Atomic(41)  print(c.fetch_add(1) - 41) }
EOF

# Send check: a bare mutable record may not cross a spawn boundary
err_case send_reject_record "cannot send" <<'EOF'
type Counter { n: Int }
fn worker(c: Counter) -> Int { c.n }
fn main() { mut c = Counter(0)  join(spawn worker(c))  print(c.n) }
EOF

# two scalar arguments, joined result
ok_case two_scalar_spawn 3 <<'EOF'
fn w(a: Int, b: Int) -> Int { a + b }
fn main() { print(join(spawn w(1, 2))) }
EOF

# arity mismatch (worker takes 2, called with 1) → rejected
err_case arity_reject "takes 2 argument" <<'EOF'
fn w(a: Int, b: Int) -> Int { a + b }
fn main() { print(join(spawn w(1))) }
EOF

# parallel_for: fork n threads over 0..n, join all
ok_case parallel_for 5050 20 <<'EOF'
fn work(i: Int, total: Atomic) -> Int { total.fetch_add(i + 1)  0 }
fn main() {
    mut total = Atomic(0)
    parallel_for(100, total, work)
    print(total.load())
}
EOF

# parallel_for shared must be a Sync type
err_case parallel_for_send "cannot send" <<'EOF'
type Bag { n: Int }
fn work(i: Int, b: Bag) -> Int { b.n }
fn main() { mut b = Bag(0)  parallel_for(4, b, work)  print(b.n) }
EOF

# Per-thread region stacks: 8 workers each run an arena-promoted loop (allocations
# go into a bump region, not the heap). The region is thread-local, so concurrent
# workers do not share/race on it — the total must be deterministic across runs.
ok_case per_thread_arena 80002800000 20 <<'EOF'
fn compute(base: Int) -> Int {
    mut s = 0
    mut k = 0
    while k < 100000 { mut a = array(4)  a[0] = base + k  a[1] = k  s = s + a[0] + a[1]  k = k + 1 }
    s
}
fn main() {
    mut h1 = spawn compute(1)  mut h2 = spawn compute(2)
    mut h3 = spawn compute(3)  mut h4 = spawn compute(4)
    mut h5 = spawn compute(5)  mut h6 = spawn compute(6)
    mut h7 = spawn compute(7)  mut h8 = spawn compute(8)
    print(join(h1)+join(h2)+join(h3)+join(h4)+join(h5)+join(h6)+join(h7)+join(h8))
}
EOF

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
