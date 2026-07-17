#!/usr/bin/env bash
# Reproduziert M0.1 (adversarialer Objektgraph, FastLLVM auto-RC vs Rust-Indizes).
# Vollbericht: ../../sprache/M0-MESSUNG.md
set -e
fj="$(cd "$(dirname "$0")/../.." && pwd)/target/debug/fastjavac"
javac -d . Graph.java
"$fj" -o graph_fl Graph.class 'Graph$Node.class'
rustc -O graph_idx.rs -o graph_idx
one(){ s=$(date +%s.%N); timeout 90 "$@" >/dev/null 2>&1; e=$(date +%s.%N); echo "$(echo "$e-$s"|bc)s"; }
echo "N=200000: FastLLVM(auto-RC) $(one ./graph_fl)   Rust(Indizes) $(one ./graph_idx)"
echo "(Erwartung: FastLLVM Timeout/Segfault, Rust ~0.06s — s. Bericht)"
