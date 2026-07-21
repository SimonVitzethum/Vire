#!/bin/sh
# Rebuild the bundled Vire frontend wasm (diagnostics + hover + go-to-definition)
# and copy it into the extension. Run from the repo root or anywhere.
#
#   sh editors/vscode-vire/build-wasm.sh
#
# Requires the wasm32-wasip1 target: `rustup target add wasm32-wasip1`.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
cd "$root"
rustup target list --installed 2>/dev/null | grep -q wasm32-wasip1 || rustup target add wasm32-wasip1
cargo build -p vire-wasm --target wasm32-wasip1 --release
mkdir -p "$here/wasm"
cp target/wasm32-wasip1/release/vire-check.wasm "$here/wasm/vire-check.wasm"
echo "updated $here/wasm/vire-check.wasm ($(wc -c < "$here/wasm/vire-check.wasm") bytes)"
