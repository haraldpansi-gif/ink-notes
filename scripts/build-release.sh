#!/usr/bin/env bash
# ==============================================================================
# Haupt-Release-Build-Skript für ink-notes (Zorin OS 18 / Linux)
# Erstellt .deb, .AppImage und SHA256-Prüfsummen in target/
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${ROOT_DIR}"

VERSION=$(grep -m1 '^version = ' Cargo.toml | cut -d '"' -f2)
DEB_NAME="ink-notes_${VERSION}_amd64.deb"
APPIMAGE_NAME="ink-notes_${VERSION}_x86_64.AppImage"
CHECKSUM_FILE="SHA256SUMS.txt"

echo "======================================================================"
echo "Starte Release-Build für ink-notes v${VERSION}"
echo "======================================================================"

# 1. Debian-Paket bauen (.deb)
"${SCRIPT_DIR}/package-deb.sh"

# 2. AppImage bauen (.AppImage)
"${SCRIPT_DIR}/package-appimage.sh"

# 3. Checksums berechnen
echo "=== Berechne SHA256-Prüfsummen in target/${CHECKSUM_FILE} ==="
cd "${ROOT_DIR}/target"
sha256sum "${DEB_NAME}" "${APPIMAGE_NAME}" > "${CHECKSUM_FILE}"

echo "======================================================================"
echo "Release-Artefakte für v${VERSION} erfolgreich erstellt:"
echo "======================================================================"
ls -lh "${DEB_NAME}" "${APPIMAGE_NAME}" "${CHECKSUM_FILE}"
echo ""
echo "Inhalt von target/${CHECKSUM_FILE}:"
cat "${CHECKSUM_FILE}"
echo "======================================================================"
