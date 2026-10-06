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

echo "=== [1/5] Baue Release-Binary mit Cargo (ink-notes v${VERSION}) ==="
cargo build --release

# Sicherstellen, dass das PNG-Icon existiert
if [ ! -f "assets/ink-notes.png" ]; then
    echo "Generiere PNG-Icon..."
    python3 scripts/generate_png_icon.py
fi

echo "=== [2/5] Ermittle tatsächliche Abhängigkeiten (ldd & dpkg-shlibdeps) ==="
echo "Linker-Abhängigkeiten (ldd):"
ldd target/release/ink-notes

if command -v dpkg-shlibdeps >/dev/null 2>&1; then
    echo "Ermittle automatische shlibdeps via dpkg-shlibdeps..."
    TEMP_CONTROL_DIR="$(mktemp -d)"
    mkdir -p "${TEMP_CONTROL_DIR}/debian"
    echo "Source: ${PKG_NAME}" > "${TEMP_CONTROL_DIR}/debian/control"
    (
        cd "${TEMP_CONTROL_DIR}"
        dpkg-shlibdeps -O "${ROOT_DIR}/target/release/ink-notes" 2>/dev/null || true
    )
    rm -rf "${TEMP_CONTROL_DIR}"
fi

echo "=== [3/5] Erstelle Paketstruktur in ${STAGING_DIR} ==="
rm -rf "${STAGING_DIR}"
mkdir -p "${STAGING_DIR}/DEBIAN"
mkdir -p "${STAGING_DIR}/usr/bin"
mkdir -p "${STAGING_DIR}/usr/share/applications"
mkdir -p "${STAGING_DIR}/usr/share/icons/hicolor/scalable/apps"
mkdir -p "${STAGING_DIR}/usr/share/icons/hicolor/256x256/apps"
mkdir -p "${STAGING_DIR}/usr/share/metainfo"
mkdir -p "${STAGING_DIR}/usr/share/doc/${PKG_NAME}"

# Dateien kopieren
cp "target/release/ink-notes" "${STAGING_DIR}/usr/bin/ink-notes"

# Desktop-Dateien (sowohl mit AppStream ID als auch Legacy Name)
cp "packaging/eu.pansi.ink-notes.desktop" "${STAGING_DIR}/usr/share/applications/eu.pansi.ink-notes.desktop"
cp "packaging/eu.pansi.ink-notes.desktop" "${STAGING_DIR}/usr/share/applications/ink-notes.desktop"

# Icons (eu.pansi.ink-notes & ink-notes)
cp "assets/ink-notes.svg" "${STAGING_DIR}/usr/share/icons/hicolor/scalable/apps/eu.pansi.ink-notes.svg"
cp "assets/ink-notes.svg" "${STAGING_DIR}/usr/share/icons/hicolor/scalable/apps/ink-notes.svg"
cp "assets/ink-notes.png" "${STAGING_DIR}/usr/share/icons/hicolor/256x256/apps/eu.pansi.ink-notes.png"
cp "assets/ink-notes.png" "${STAGING_DIR}/usr/share/icons/hicolor/256x256/apps/ink-notes.png"

# AppStream Metainfo
cp "packaging/eu.pansi.ink-notes.metainfo.xml" "${STAGING_DIR}/usr/share/metainfo/eu.pansi.ink-notes.metainfo.xml"

# Debian doc: copyright
cat <<'EOF' > "${STAGING_DIR}/usr/share/doc/ink-notes/copyright"
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: ink-notes
Upstream-Contact: Harald <harald@pansi.eu>
Source: https://gitea.pansi.eu/harald/ink-notes

Files: *
Copyright: 2026 Harald Pansi <harald@pansi.eu>
License: MIT

License: MIT
 Permission is hereby granted, free of charge, to any person obtaining a copy
 of this software and associated documentation files (the "Software"), to deal
 in the Software without restriction, including without limitation the rights
 to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 copies of the Software, and to permit persons to whom the Software is
 furnished to do so, subject to the following conditions:
 .
 The above copyright notice and this permission notice shall be included in all
 copies or substantial portions of the Software.
 .
 THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 SOFTWARE.
EOF

# Debian doc: changelog.gz
CHANGELOG_TMP="$(mktemp)"
cat <<EOF > "${CHANGELOG_TMP}"
ink-notes (${VERSION}-1) stable; urgency=medium

  * Release v${VERSION}.
  * Low-latency vector inking with two-stage pipeline.
  * Shape recognition and infinite canvas with zoom/pan.
  * Multi-notebook management and PDF export.

 -- Harald <harald@pansi.eu>  $(date -R)
EOF
gzip -n -9 -c "${CHANGELOG_TMP}" > "${STAGING_DIR}/usr/share/doc/ink-notes/changelog.gz"
rm -f "${CHANGELOG_TMP}"

# Berechtigungen penibel setzen
find "${STAGING_DIR}" -type d -exec chmod 755 {} +
find "${STAGING_DIR}/usr/share" -type f -exec chmod 644 {} +
chmod 755 "${STAGING_DIR}/DEBIAN"
chmod 755 "${STAGING_DIR}/usr/bin/ink-notes"

# DEBIAN/control Datei generieren
cat <<EOF > "${STAGING_DIR}/DEBIAN/control"
Package: ${PKG_NAME}
Version: ${VERSION}
Architecture: ${ARCH}
Maintainer: Harald <harald@pansi.eu>
Installed-Size: $(du -ks "${STAGING_DIR}/usr" | cut -f1)
Depends: libc6 (>= 2.38), libgcc-s1, libvulkan1, mesa-vulkan-drivers | vulkan-icd, libwayland-client0, libx11-6, libxcursor1, libxrandr2, libxi6, libxkbcommon0, xdg-utils
Section: utils
Priority: optional
Homepage: https://gitea.pansi.eu/harald/ink-notes
Description: Latenzoptimierte Vektor-Notizbuch- und Inking-App für Linux
 ink-notes kombiniert minimale Stylus-Latenz mit GPU-Hardwarebeschleunigung (wgpu),
 stufenlosem Viewport, Geometrie- und Formerkennung, Objekt-Transformationen,
 Multi-Dokumenten-Verwaltung und Vektor-PDF-Export.
EOF
chmod 644 "${STAGING_DIR}/DEBIAN/control"

echo "=== [4/5] Schnüre .deb-Paket mit dpkg-deb ==="
dpkg-deb --build --root-owner-group "${STAGING_DIR}" "${ROOT_DIR}/target/${DEB_NAME}"

echo "=== [5/5] Paket erfolgreich erstellt: target/${DEB_NAME} ==="
ls -lh "${ROOT_DIR}/target/${DEB_NAME}"
dpkg-deb -I "${ROOT_DIR}/target/${DEB_NAME}"

if command -v lintian >/dev/null 2>&1; then
    echo "=== Prüfe Paket mit lintian ==="
    lintian "${ROOT_DIR}/target/${DEB_NAME}" || true
fi

echo "Fertig!"
