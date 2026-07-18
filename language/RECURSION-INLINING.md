# Why g++ is ~1.8x faster on fib — and how LLVM can beat it

*User question: "look into why g++ is so much faster and whether one can adopt
that with LLVM."*

## The finding (disassembled + measured)
naive fib(38): **Vire 0.080 · clang++ 0.077 · g++ 0.042**. Vire == clang (both
LLVM); g++ is the outlier. Disassembly: **g++ inlines the recursion flat into
itself** (large stack frame, nested unfolding of several levels) → each real
`call` computes several fib values inline → **~half as many calls =
~1.8x constant factor**. clang/LLVM do NOT inline recursion by default, and
**no clang flag** enables it (tested: `-finline-functions`,
`-inline-threshold=2000/5000`, `-funroll-loops`, `-O3`, `__attribute__((const))` —
all stay at ~0.077).

## The lever is larger than g++ (via LLVM-CSE)
Manually self-inlined ONE recursion level (`fib(n-1)`/`fib(n-2)` each unfolded one
step), semantically identical, measured in Vire: **0.0047 s — 17x faster
than g++.** Reason: the inlining exposes a **duplicate subcall** —
`fib(n-1)` and `fib(n-2)` BOTH call `fib(n-3)`. LLVM's GVN/CSE merges the two
identical calls (fib is side-effect-free → LLVM infers `readnone`) → **the
branching factor drops** (φ=1.618 → ~1.47), and this compounds recursively across the
frames. **g++ does NOT capture this CSE branching reduction** (stays at 1.8x) — LLVM's
CSE is stronger here, once the inlining makes the duplicates visible.

## Two separate effects (honest assessment)
1. **Call-overhead halving (~1.8x):** applies to ANY recursion-heavy function
   (even without overlapping subproblems, e.g. `check` in binary-trees). Pure
   constant factor from fewer `call`s.
2. **Branching reduction (up to ~17x on fib):** ONLY for **overlapping**
   subproblems (fib, naive DP), where inlining exposes duplicate pure subcalls
   that LLVM-CSE merges. Not for disjoint tree recursion.

## Can Vire adopt it? YES — as a solver pass "recursion inlining"
Vire can **inline a small, pure, self-recursive function 1–2 levels into itself**
(with the base-case guard preserved → termination), then LLVM does the rest:
- Call-overhead halving accrues immediately (~1.8x on recursion, ≥5% threshold clearly
  exceeded).
- For overlapping recursion, LLVM-CSE merges the exposed duplicates → the large
  additional gain — FOR FREE, because LLVM already treats fib as `readnone`.

**Conditions (sound, conservative):** only self-recursive function; small (inline
budget); pure body (no side effects/allocation — otherwise no CSE + semantics);
base case preserved as the recursion floor (the inline-expanded body calls, at
its bottom, the real `fib` again). Risk: code bloat (limit depth to 1–2)
and compile time.

## Recommendation
The pass has a **measured, large ceiling** (fib 0.080→0.0047; generally ~1.8x on
recursion) and is a genuine "AOT does what the programmer did not." Build it as a
focused step: **shallow self-recursive inlining** in the Vire lowering
(or as a solver pass on the IR), depth 1–2, only pure small self-recursive fns.
This is the ONLY case found where Vire lagged behind g++ — and it is
catchable AND beatable. (Priority after the explicitly requested RAM/C++
points.)
