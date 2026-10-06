#!/usr/bin/env bash
# ==============================================================================
# Baut ein natives .deb-Paket für Zorin OS 18 / Ubuntu / Debian
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${ROOT_DIR}"

# 1. Version aus Cargo.toml ermitteln
VERSION=$(grep -m1 '^version = ' Cargo.toml | cut -d '"' -f2)
PKG_NAME="ink-notes"
ARCH="amd64"
DEB_NAME="${PKG_NAME}_${VERSION}_${ARCH}.deb"
STAGING_DIR="${ROOT_DIR}/target/deb_staging"

echo "=== [1/4] Baue Release-Binary mit Cargo (ink-notes v${VERSION}) ==="
cargo build --release

# Sicherstellen, dass das PNG-Icon existiert
if [ ! -f "assets/ink-notes.png" ]; then
    echo "Generiere PNG-Icon..."
    python3 scripts/generate_png_icon.py
fi

echo "=== [2/4] Erstelle Paketstruktur in ${STAGING_DIR} ==="
rm -rf "${STAGING_DIR}"
mkdir -p "${STAGING_DIR}/DEBIAN"
mkdir -p "${STAGING_DIR}/usr/bin"
mkdir -p "${STAGING_DIR}/usr/share/applications"
mkdir -p "${STAGING_DIR}/usr/share/icons/hicolor/scalable/apps"
mkdir -p "${STAGING_DIR}/usr/share/icons/hicolor/256x256/apps"
mkdir -p "${STAGING_DIR}/usr/share/metainfo"

# 2. Dateien kopieren
cp "target/release/ink-notes" "${STAGING_DIR}/usr/bin/ink-notes"
chmod 755 "${STAGING_DIR}/usr/bin/ink-notes"

cp "packaging/ink-notes.desktop" "${STAGING_DIR}/usr/share/applications/ink-notes.desktop"
chmod 644 "${STAGING_DIR}/usr/share/applications/ink-notes.desktop"

cp "assets/ink-notes.svg" "${STAGING_DIR}/usr/share/icons/hicolor/scalable/apps/ink-notes.svg"
chmod 644 "${STAGING_DIR}/usr/share/icons/hicolor/scalable/apps/ink-notes.svg"

cp "assets/ink-notes.png" "${STAGING_DIR}/usr/share/icons/hicolor/256x256/apps/ink-notes.png"
chmod 644 "${STAGING_DIR}/usr/share/icons/hicolor/256x256/apps/ink-notes.png"

cp "packaging/ink-notes.metainfo.xml" "${STAGING_DIR}/usr/share/metainfo/eu.pansi.ink-notes.metainfo.xml"
chmod 644 "${STAGING_DIR}/usr/share/metainfo/eu.pansi.ink-notes.metainfo.xml"

# 3. DEBIAN/control Datei generieren
cat <<EOF > "${STAGING_DIR}/DEBIAN/control"
Package: ${PKG_NAME}
Version: ${VERSION}
Architecture: ${ARCH}
Maintainer: Harald <harald@pansi.eu>
Installed-Size: $(du -ks "${STAGING_DIR}/usr" | cut -f1)
Depends: libc6, libx11-6, libxcursor1, libxrandr2, libxi6, libxkbcommon0
Section: utils
Priority: optional
Homepage: https://farmersdog.pansi.eu/harald/ink-notes
Description: Latenzoptimierte Vektor-Notizbuch- und Inking-App für Linux
 ink-notes kombiniert minimale Stylus-Latenz mit GPU-Hardwarebeschleunigung (wgpu),
 stufenlosem Viewport, Geometrie- und Formerkennung, Objekt-Transformationen,
 Multi-Dokumenten-Verwaltung und Vektor-PDF-Export.
EOF

echo "=== [3/4] Schnüre .deb-Paket mit dpkg-deb ==="
dpkg-deb --build --root-owner-group "${STAGING_DIR}" "${ROOT_DIR}/target/${DEB_NAME}"

echo "=== [4/4] Paket erfolgreich erstellt: target/${DEB_NAME} ==="
ls -lh "${ROOT_DIR}/target/${DEB_NAME}"
dpkg-deb -I "${ROOT_DIR}/target/${DEB_NAME}"
echo "Fertig!"
