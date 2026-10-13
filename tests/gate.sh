#!/bin/sh
# Master gate — the single "is the tree green" check. Runs every test STAGE and
# fails if any stage fails. Created because three cargo unit tests rotted red on
# HEAD unnoticed: the cargo layer was not part of any gate, so stale safety-tests
# (asserting a deliberately-lifted capsule restriction) became indistinguishable
# from real regressions. This ties the layers together so that can't recur.
#
# Usage:  sh tests/gate.sh            (from the project root)
#
# Stages, cheapest first (fail fast):
#   0. runtime.c syntax (clang -fsyntax-only)
#   1. cargo test  — unit/lowering/inference tests (the layer that rotted)
#   2. vire_*.sh   — Vire language + heap-balance suites
#   3. examples    — every examples/vire/*.vr builds, runs, matches its output
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

# 0. C syntax of the embedded runtime sources. cargo only `include_str!`s them,
#    so a C syntax error sails green through the whole gate on any machine
#    where no program build exercises them. -fsyntax-only closes that gap.
if command -v clang >/dev/null 2>&1; then
    stage "runtime.c syntax"    clang -fsyntax-only "$root/crates/runtime/runtime.c"
fi

# 1. cargo — build release first (the suites need the binary), then test.
stage "cargo build --release" cargo build --release
stage "cargo test --release"  cargo test --release --workspace

# 2. Vire shell suites.
for t in "$root"/tests/vire_*.sh; do
    stage "$(basename "$t")" sh "$t"
done

# 3. Examples.
stage "examples/vire" sh "$root/examples/vire/run.sh"

printf '\n========================================\n'
if [ -z "$stages_failed" ]; then
    echo "GATE GREEN — all stages passed"
    exit 0
else
    echo "GATE RED — failed stages:$stages_failed"
    exit 1
fi
