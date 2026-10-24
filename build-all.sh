#!/usr/bin/env bash
# Release builds for every supported platform (Linux x86_64, Windows x86_64), copied to dist/.
# Windows is cross-compiled with MinGW. A failing target does not stop the others;
# the script exits non-zero if any target failed.
# Extra arguments are passed to cargo, e.g. ./build-all.sh --offline
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found. Install Rust from https://rustup.rs" >&2
    exit 1
fi

VERSION=$(sed -n '/^\[package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' Cargo.toml | head -n1)

# target triple | output file name | linker the target needs | how to get the linker
TARGETS=(
    "x86_64-unknown-linux-gnu|jmx-$VERSION-linux-x86_64|cc|sudo apt install build-essential"
    "x86_64-pc-windows-gnu|jmx-$VERSION-windows-x86_64.exe|x86_64-w64-mingw32-gcc|sudo apt install mingw-w64"
)

mkdir -p dist
declare -a RESULTS=()
FAILED=0

# Check a target's requirements; prints every missing one. Returns 1 if not buildable.
check_target() {
    local triple=$1 linker=$2 hint=$3 ok=0
    if command -v rustup >/dev/null 2>&1 && ! rustup target list --installed | grep -qx "$triple"; then
        echo "    installing Rust target $triple"
        rustup target add "$triple" || { echo "    missing: Rust target (rustup target add $triple)"; ok=1; }
    fi
    if ! command -v "$linker" >/dev/null 2>&1; then
        echo "    missing: linker '$linker' (install with: $hint)"
        ok=1
    fi
    return $ok
}

for entry in "${TARGETS[@]}"; do
    IFS='|' read -r triple name linker hint <<<"$entry"
    echo "==> $triple"
    if ! check_target "$triple" "$linker" "$hint"; then
        RESULTS+=("FAILED|$triple|missing requirements (see above)")
        FAILED=1
        echo
        continue
    fi
    ext=""
    [[ $name == *.exe ]] && ext=".exe"
    if cargo build --release --target "$triple" "$@"; then
        cp "target/$triple/release/jmx$ext" "dist/$name"
        RESULTS+=("OK|$triple|dist/$name ($(du -h "dist/$name" | cut -f1))")
    else
        RESULTS+=("FAILED|$triple|cargo build failed")
        FAILED=1
    fi
    echo
done

echo "==> Summary (jmx $VERSION)"
for r in "${RESULTS[@]}"; do
    IFS='|' read -r status triple detail <<<"$r"
    printf '  %-7s %-26s %s\n' "$status" "$triple" "$detail"
done
exit $FAILED
