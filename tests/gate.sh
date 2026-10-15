#!/bin/sh
# Master gate — the single "is the tree green" check.
# Stages, cheapest first (fail fast):
#   0. runtime.c syntax (clang -fsyntax-only; the LLVM oracle embeds it)
#   1. cargo build + cargo test (unit/lowering/inference tests)
#   2. vire_heap.sh  — LLVM-oracle heap-balance gate
#   3. vire_gab.sh   — Gabbro target gate (`gabbro check`, needs a gabbro binary)
#   4. examples      — every examples/vire/*.vr builds, runs, matches its output
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root" || exit 2
stages_failed=""

stage() {  # stage <name> <command...>
    name="$1"; shift
    printf '\n=== %s ===\n' "$name"
    if "$@"; then printf '  [PASS] %s\n' "$name"
    else printf '  [FAIL] %s\n' "$name"; stages_failed="$stages_failed $name"; fi
}

if command -v clang >/dev/null 2>&1; then
    stage "runtime.c syntax"    clang -fsyntax-only "$root/crates/runtime/runtime.c"
fi

stage "cargo build --release" cargo build --release
stage "cargo test --release"  cargo test --release --workspace

stage "vire_heap" sh "$root/tests/vire_heap.sh"
stage "vire_gab" sh "$root/tests/vire_gab.sh"

stage "examples/vire" sh "$root/examples/vire/run.sh"

printf '\n========================================\n'
if [ -z "$stages_failed" ]; then
    echo "GATE GREEN — all stages passed"
    exit 0
else
    echo "GATE RED — failed stages:$stages_failed"
    exit 1
fi
