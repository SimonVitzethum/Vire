#!/bin/sh
# Master gate — the single "is the tree green" check. Runs every test STAGE and
# fails if any stage fails. Created because three cargo unit tests rotted red on
# HEAD unnoticed: the cargo layer was not part of any gate, so stale safety-tests
# (asserting a deliberately-lifted capsule restriction) became indistinguishable
# from real regressions. This ties the layers together so that can't recur.
#
# Usage:  sh tests/gate.sh            (from the project root)
#         sh tests/gate.sh --fast     (skip the slow Java heap oracle)
#
# Stages, cheapest first (fail fast):
#   1. cargo test  — unit/lowering/inference tests (the layer that rotted)
#   2. vire_*.sh   — Vire language + heap-balance + vulkan suites
#   3. examples    — every examples/vire/*.vr builds, runs, matches its output
#   4. run.sh      — the Java AOT heap-balance oracle (0 live), the paramount gate
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root" || exit 2
fast=0; [ "${1:-}" = "--fast" ] && fast=1
stages_failed=""

stage() {  # stage <name> <command...>
    name="$1"; shift
    printf '\n=== %s ===\n' "$name"
    if "$@"; then printf '  [PASS] %s\n' "$name"
    else printf '  [FAIL] %s\n' "$name"; stages_failed="$stages_failed $name"; fi
}

# 0. C syntax of the embedded runtime sources. cargo only `include_str!`s them — clang
#    doesn't touch vk_runtime.c until a Vire @vulkan program is built WITH a device, so a
#    C syntax error there sails green through the whole gate on any deviceless machine
#    (CI, GPU busy). -fsyntax-only needs no GPU and closes that gap.
if command -v clang >/dev/null 2>&1; then
    stage "vk_runtime.c syntax" clang -fsyntax-only "$root/crates/driver/src/vk_runtime.c"
    stage "runtime.c syntax"    clang -fsyntax-only "$root/crates/driver/src/runtime.c"
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

# 4. Java heap-balance oracle (slow — ~minutes). Skippable with --fast.
if [ "$fast" -eq 0 ]; then
    stage "run.sh (Java heap oracle)" sh "$root/tests/run.sh"
else
    printf '\n(skipping the Java heap oracle — --fast)\n'
fi

printf '\n========================================\n'
if [ -z "$stages_failed" ]; then
    echo "GATE GREEN — all stages passed"
    exit 0
else
    echo "GATE RED — failed stages:$stages_failed"
    exit 1
fi
