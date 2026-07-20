#!/usr/bin/env python3
# Differential compiler fuzzer for Vire.
#
# Emits the SAME randomly-generated integer program in two syntaxes — Vire (.vr)
# and C (.c) — then a runner compiles both and diffs stdout. Any divergence is a
# miscompilation (in one of the two backends; C via clang -O2 is the trusted
# oracle). The Vire binary is also run under FASTLLVM_HEAPSTATS: a non-"0 live"
# result is a memory-safety bug (leak / double-count) even when the value matches.
#
# Semantics are kept bit-identical across Vire's checked i64 and C int64_t by
# construction: every value lives in [0, M) (M prime), every binary op is wrapped
# back into that range, division/modulo guard the divisor (never 0, operands
# non-negative → truncation agrees), and all loops are counted (termination).
# Multiplication intermediates are < M*M ≈ 1e12 < i64 max, so nothing overflows
# and Vire's overflow checks never trap. Helper fK may only call fJ with J<K, so
# the call graph is a DAG (no unbounded recursion).
import random, sys, re

M = 1000003          # prime modulus; values stay in [0, M)
SIZE = 64            # array length
random.seed(int(sys.argv[1]) if len(sys.argv) > 1 else 0)
NH = random.randint(2, 5)          # helper functions f0..f{NH-1}

class Gen:
    def __init__(self, fn_index, params, nvars):
        self.fn_index = fn_index      # which helper (for DAG call restriction); None=main
        self.params = params          # list of param names in scope
        self.vars = []                # declared locals so far
        self.nvars = nvars

    def term(self):
        # A shallow, non-recursive value: constant or in-scope var (used as an
        # array index so leaf/expr cannot recurse unboundedly).
        choices = list(self.params) + list(self.vars)
        if random.random() < 0.4 or not choices:
            return (str(random.randint(0, SIZE * 4)),) * 2
        v = random.choice(choices)
        return (v, v)

    def leaf(self):
        choices = list(self.params) + list(self.vars)
        r = random.random()
        if r < 0.35 or not choices:
            return (str(random.randint(0, M - 1)),) * 2               # constant
        if r < 0.7:
            v = random.choice(choices)
            return (v, v)                                             # var
        # array read: arr[idx % SIZE], index is a shallow term (no recursion)
        iv, ic = self.term()
        return (f"arr[({iv}) % {SIZE}]", f"arr[({ic}) % {SIZE}]")

    def expr(self, depth):
        if depth >= 3 or random.random() < 0.35:
            return self.leaf()
        r = random.random()
        lv, lc = self.expr(depth + 1)
        rv, rc = self.expr(depth + 1)
        if r < 0.2:   return (f"(({lv} + {rv}) % {M})", f"(({lc} + {rc}) % {M})")
        if r < 0.4:   return (f"(({lv} - {rv} + {M}) % {M})", f"(({lc} - {rc} + {M}) % {M})")
        if r < 0.55:  return (f"(({lv} * {rv}) % {M})", f"(({lc} * {rc}) % {M})")
        if r < 0.68:  return (f"({lv} / (({rv}) % {M} + 1))", f"({lc} / (({rc}) % {M} + 1))")
        if r < 0.8:   return (f"({lv} % (({rv}) % {M} + 1))", f"({lc} % (({rc}) % {M} + 1))")
        if r < 0.9:   # comparison → 0/1
            op = random.choice(["<", "<=", "==", "!="])
            return (f"(if {lv} {op} {rv} {{ 1 }} else {{ 0 }})", f"(({lc} {op} {rc}) ? 1 : 0)")
        # call an earlier helper (DAG); main can call any
        callable_fns = [i for i in range(NH) if self.fn_index is None or i < self.fn_index]
        if not callable_fns:
            return (f"(({lv} + {rv}) % {M})", f"(({lc} + {rc}) % {M})")
        k = random.choice(callable_fns)
        return (f"f{k}({lv}, {rv}, arr)", f"f{k}({lc}, {rc}, arr)")

    def stmts(self, indent, allow_writes):
        vr, c = [], []
        pad = "    " * indent
        for i in range(self.nvars):
            name = f"v{self.fn_index if self.fn_index is not None else 'm'}_{i}"
            ev, ec = self.expr(0)
            vr.append(f"{pad}mut {name} = {ev}")
            c.append(f"{pad}int64_t {name} = {ec};")
            self.vars.append(name)
            # occasionally a counted loop that mutates the array (exercises stores +
            # bounds). ONLY at top level (main): array writes are side effects, and a
            # side effect inside an expression would make C's unspecified evaluation
            # order diverge from Vire's — so helpers stay PURE (read-only).
            if allow_writes and random.random() < 0.4:
                lv, lc = self.expr(0)
                cnt = random.randint(2, 6)
                idx = f"({name} + i) % {SIZE}"
                vr.append(f"{pad}mut i = 0")
                vr.append(f"{pad}while i < {cnt} {{ arr[{idx}] = ({lv}) % {M}  i = i + 1 }}")
                c.append(f"{pad}for (int64_t i = 0; i < {cnt}; i++) {{ arr[{idx}] = ({lc}) % {M}; }}")
        return vr, c

def helper(k):
    g = Gen(k, ["a", "b"], random.randint(1, 4))
    vr, c = g.stmts(1, allow_writes=False)
    tv, tc = g.expr(0)
    vr_s = "\n".join(vr)
    c_s = "\n".join(c)
    vire = f"fn f{k}(a: Int, b: Int, arr: Array[Int]) -> Int {{\n{vr_s}\n    ({tv}) % {M}\n}}"
    cc = f"int64_t f{k}(int64_t a, int64_t b, int64_t* arr) {{\n{c_s}\n    return ({tc}) % {M};\n}}"
    return vire, cc

def main():
    g = Gen(None, [], random.randint(3, 8))
    vr, c = g.stmts(1, allow_writes=True)
    # accumulate several helper calls into acc
    acc_v, acc_c = [], []
    for _ in range(random.randint(3, 8)):
        ev, ec = g.expr(0)
        acc_v.append(f"    acc = (acc + ({ev})) % {M}")
        acc_c.append(f"    acc = (acc + ({ec})) % {M};")
    vr_body = "\n".join(vr)
    c_body = "\n".join(c)
    av = "\n".join(acc_v); ac = "\n".join(acc_c)
    vire_main = (
        "fn main() {\n"
        f"    mut arr = array({SIZE})\n"
        f"    mut i0 = 0\n"
        f"    while i0 < {SIZE} {{ arr[i0] = (i0 * 2654435761 + 1) % {M}  i0 = i0 + 1 }}\n"
        "    mut acc = 0\n"
        f"{vr_body}\n{av}\n"
        "    print(acc)\n"
        "}\n")
    c_main = (
        "int main(void) {\n"
        f"    int64_t arr[{SIZE}];\n"
        f"    for (int64_t i0 = 0; i0 < {SIZE}; i0++) arr[i0] = (i0 * 2654435761 + 1) % {M};\n"
        "    int64_t acc = 0;\n"
        f"{c_body}\n{ac}\n"
        '    printf("%lld\\n", (long long)acc);\n'
        "    return 0;\n"
        "}\n")
    helpers_v, helpers_c = [], []
    for k in range(NH):
        hv, hc = helper(k)
        helpers_v.append(hv); helpers_c.append(hc)
    vire = "\n".join(helpers_v) + "\n" + vire_main
    c = "#include <stdio.h>\n#include <stdint.h>\n" + "\n".join(helpers_c) + "\n" + c_main
    return vire, c

if __name__ == "__main__":
    vire, c = main()
    # C integer literals default to `int` (32-bit) → const*const overflows before
    # the `% M` wrap (UB, and clang -O2 exploits it), diverging from Vire's i64.
    # Suffix every standalone numeric literal with `LL` so all C arithmetic is
    # int64_t like Vire. The lookarounds skip digits inside identifiers (`v0_2`,
    # `int64_t`) — only bare numbers get the suffix.
    c = re.sub(r'(?<![\w.])(\d+)(?![\w.])', r'\g<1>LL', c)
    with open(sys.argv[2], "w") as f: f.write(vire)
    with open(sys.argv[3], "w") as f: f.write(c)
