#!/usr/bin/env bash
# ==============================================================================
# Builds the portable Windows program target/ink-notes_<version>_x86_64.exe (Issue #48)
# by cross-compiling on Linux. Needs the MinGW-w64 compiler
# (Debian/Ubuntu: gcc-mingw-w64-x86-64) for the C parts of the dependencies.
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${ROOT_DIR}"

TARGET="x86_64-pc-windows-gnu"
VERSION=$(grep -m1 '^version = ' Cargo.toml | cut -d '"' -f2)
EXE_NAME="ink-notes_${VERSION}_x86_64.exe"

if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
    echo "FEHLER: x86_64-w64-mingw32-gcc nicht gefunden (Paket gcc-mingw-w64-x86-64)."
    exit 1
fi
if command -v rustup >/dev/null 2>&1; then
    rustup target add "${TARGET}"
fi

echo "=== Baue ${EXE_NAME} ==="
cargo build --release --target "${TARGET}"
cp "${CARGO_TARGET_DIR:-target}/${TARGET}/release/ink-notes.exe" "target/${EXE_NAME}"
ls -lh "target/${EXE_NAME}"
