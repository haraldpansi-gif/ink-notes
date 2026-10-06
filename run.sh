#!/usr/bin/env bash
# Starter-Skript für ink-notes (konfiguriert X11/Xwayland & xkb-Bibliotheken)
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export LD_LIBRARY_PATH="$HOME/.local/usr/lib/x86_64-linux-gnu:${LD_LIBRARY_PATH}"
export WAYLAND_DISPLAY=""

exec "$SCRIPT_DIR/target/debug/ink-notes" "$@"
