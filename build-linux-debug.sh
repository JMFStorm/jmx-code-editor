#!/usr/bin/env bash
# Debug build for Linux. Extra arguments are passed to cargo, e.g. ./build-linux-debug.sh --offline
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found. Install Rust from https://rustup.rs" >&2
    exit 1
fi

echo "==> Building jdc (Linux, debug)"
cargo build "$@"

echo
echo "Done: $(pwd)/target/debug/jdc"
