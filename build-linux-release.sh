#!/usr/bin/env bash
# Optimized release build for Linux x86_64, copied to dist/.
# Extra arguments are passed to cargo, e.g. ./build-linux-release.sh --offline
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

TARGET=x86_64-unknown-linux-gnu

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found. Install Rust from https://rustup.rs" >&2
    exit 1
fi

# Version from the [package] section of Cargo.toml.
VERSION=$(sed -n '/^\[package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' Cargo.toml | head -n1)

echo "==> Building jmx $VERSION (Linux x86_64, release)"
cargo build --release --target "$TARGET" "$@"

mkdir -p dist
OUT="dist/jmx-$VERSION-linux-x86_64"
cp "target/$TARGET/release/jmx" "$OUT"

echo
echo "Done: $(pwd)/$OUT ($(du -h "$OUT" | cut -f1))"
