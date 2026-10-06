#!/usr/bin/env bash
# ==============================================================================
# Baut ein portables .AppImage-Paket für ink-notes (Zorin OS 18 / Linux x86_64)
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${ROOT_DIR}"

# 1. Version aus Cargo.toml ermitteln
VERSION=$(grep -m1 '^version = ' Cargo.toml | cut -d '"' -f2)
PKG_NAME="ink-notes"
ARCH="x86_64"
APPIMAGE_NAME="${PKG_NAME}_${VERSION}_${ARCH}.AppImage"
APPDIR="${ROOT_DIR}/target/AppDir"
CACHE_DIR="${ROOT_DIR}/target/tools"
APPIMAGETOOL="${CACHE_DIR}/appimagetool-x86_64.AppImage"

echo "=== [1/5] Stelle Release-Binary sicher (ink-notes v${VERSION}) ==="
if [ ! -f "target/release/ink-notes" ]; then
    cargo build --release
fi

# Sicherstellen, dass das PNG-Icon existiert
if [ ! -f "assets/ink-notes.png" ]; then
    echo "Generiere PNG-Icon..."
    python3 scripts/generate_png_icon.py
fi

echo "=== [2/5] Bereite AppDir-Struktur vor in ${APPDIR} ==="
rm -rf "${APPDIR}"
mkdir -p "${APPDIR}/usr/bin"
mkdir -p "${APPDIR}/usr/share/applications"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/scalable/apps"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/256x256/apps"
mkdir -p "${APPDIR}/usr/share/metainfo"

# Binaries & Assets kopieren
cp "target/release/ink-notes" "${APPDIR}/usr/bin/ink-notes"
chmod 755 "${APPDIR}/usr/bin/ink-notes"

cp "packaging/ink-notes.desktop" "${APPDIR}/usr/share/applications/ink-notes.desktop"
cp "packaging/ink-notes.desktop" "${APPDIR}/ink-notes.desktop"

cp "assets/ink-notes.svg" "${APPDIR}/usr/share/icons/hicolor/scalable/apps/ink-notes.svg"
cp "assets/ink-notes.png" "${APPDIR}/usr/share/icons/hicolor/256x256/apps/ink-notes.png"
cp "assets/ink-notes.png" "${APPDIR}/ink-notes.png"
ln -sf "ink-notes.png" "${APPDIR}/.DirIcon"

cp "packaging/ink-notes.metainfo.xml" "${APPDIR}/usr/share/metainfo/eu.pansi.ink-notes.metainfo.xml"

# AppRun Skript erstellen
cat <<'EOF' > "${APPDIR}/AppRun"
#!/usr/bin/env bash
SELF_DIR="$(dirname "$(readlink -f "$0")")"
export PATH="${SELF_DIR}/usr/bin:${PATH}"
export XDG_DATA_DIRS="${SELF_DIR}/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
export APPDIR="${SELF_DIR}"
exec "${SELF_DIR}/usr/bin/ink-notes" "$@"
EOF
chmod 755 "${APPDIR}/AppRun"

echo "=== [3/5] Hole appimagetool (gecacht in ${CACHE_DIR}) ==="
mkdir -p "${CACHE_DIR}"
if [ ! -f "${APPIMAGETOOL}" ]; then
    echo "Lade appimagetool herunter..."
    curl -fsSL -o "${APPIMAGETOOL}" "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"
    chmod +x "${APPIMAGETOOL}"
fi

echo "=== [4/5] Erstelle AppImage mit appimagetool ==="
# --appimage-extract-and-run erlaubt Ausführung auch ohne root / FUSE
# NO_APPSTREAM=1 verhindert Build-Abbruch bei privaten/nicht-erreichbaren URLs im Metainfo-XML
export NO_APPSTREAM=1
ARCH="${ARCH}" "${APPIMAGETOOL}" --appimage-extract-and-run "${APPDIR}" "${ROOT_DIR}/target/${APPIMAGE_NAME}"

echo "=== [5/5] AppImage erfolgreich erstellt! ==="
ls -lh "${ROOT_DIR}/target/${APPIMAGE_NAME}"
echo "Pfad: ${ROOT_DIR}/target/${APPIMAGE_NAME}"
