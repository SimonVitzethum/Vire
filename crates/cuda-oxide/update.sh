#!/bin/sh
# Refresh the vendored cuda-oxide snapshot from upstream.
#
#   sh crates/cuda-oxide/update.sh [GIT_REF]
#
# Pulls the latest (or a given tag/branch/commit) of NVlabs/cuda-oxide, drops the
# VCS metadata, and replaces this directory's contents — while PRESERVING our
# attribution note (NOTICE.md) and this script. cuda-oxide is kept for
# reference/attribution only (Apache-2.0); it is `exclude`d from the workspace in
# the root Cargo.toml and never built. See NOTICE.md.
set -eu

REPO="https://github.com/NVlabs/cuda-oxide"
REF="${1:-}"
here="$(cd "$(dirname "$0")" && pwd)"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Cloning $REPO${REF:+ @ $REF} ..."
if [ -n "$REF" ]; then
    git clone --depth 1 --branch "$REF" "$REPO" "$tmp/co" 2>/dev/null \
        || { git clone "$REPO" "$tmp/co"; git -C "$tmp/co" checkout "$REF"; }
else
    git clone --depth 1 "$REPO" "$tmp/co"
fi

rev="$(git -C "$tmp/co" rev-parse --short HEAD)"
rm -rf "$tmp/co/.git"

# Preserve our files, replace everything else with the fresh upstream tree.
keep="$tmp/keep"
mkdir -p "$keep"
cp "$here/NOTICE.md" "$keep/NOTICE.md" 2>/dev/null || true
cp "$here/update.sh" "$keep/update.sh" 2>/dev/null || true

find "$here" -mindepth 1 -maxdepth 1 ! -name update.sh -exec rm -rf {} +
cp -a "$tmp/co/." "$here/"
cp "$keep/NOTICE.md" "$here/NOTICE.md" 2>/dev/null || true

echo "Updated crates/cuda-oxide to upstream $rev."
echo "Upstream LICENSE preserved; NOTICE.md (attribution) kept."
echo "Reminder: it stays \`exclude\`d in the root Cargo.toml (not built)."
