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

# Pinned appimagetool Release & SHA-256 Hash
APPIMAGETOOL_VERSION="1.9.1"
APPIMAGETOOL_URL="https://github.com/AppImage/appimagetool/releases/download/${APPIMAGETOOL_VERSION}/appimagetool-x86_64.AppImage"
APPIMAGETOOL_SHA256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"

echo "=== [1/5] Stelle Release-Binary sicher (ink-notes v${VERSION}) ==="
if [ ! -f "target/release/ink-notes" ]; then
    cargo build --release
fi

# Prüfe glibc-Mindestversion
echo "=== Prüfe glibc-Symbolversionen ==="
MAX_GLIBC=$(objdump -T "target/release/ink-notes" | grep "GLIBC_" | sed -e 's/.*GLIBC_//' | cut -d')' -f1 | sort -V | tail -n 1 || echo "unknown")
echo "Höchste benötigte glibc-Symbolversion: GLIBC_${MAX_GLIBC}"
echo "Hinweis: ink-notes nutzt den Host-Grafikstack (Vulkan ICD, Wayland/X11) und glibc >= ${MAX_GLIBC}."

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

# Desktop-Datei
cp "packaging/eu.pansi.ink-notes.desktop" "${APPDIR}/usr/share/applications/eu.pansi.ink-notes.desktop"
cp "packaging/eu.pansi.ink-notes.desktop" "${APPDIR}/eu.pansi.ink-notes.desktop"
ln -sf "eu.pansi.ink-notes.desktop" "${APPDIR}/ink-notes.desktop"

# Icons
cp "assets/ink-notes.svg" "${APPDIR}/usr/share/icons/hicolor/scalable/apps/eu.pansi.ink-notes.svg"
cp "assets/ink-notes.png" "${APPDIR}/usr/share/icons/hicolor/256x256/apps/eu.pansi.ink-notes.png"
cp "assets/ink-notes.png" "${APPDIR}/eu.pansi.ink-notes.png"
ln -sf "eu.pansi.ink-notes.png" "${APPDIR}/.DirIcon"

# Metainfo
cp "packaging/eu.pansi.ink-notes.metainfo.xml" "${APPDIR}/usr/share/metainfo/eu.pansi.ink-notes.metainfo.xml"
cp "packaging/eu.pansi.ink-notes.metainfo.xml" "${APPDIR}/usr/share/metainfo/eu.pansi.ink-notes.appdata.xml"

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

echo "=== [3/5] Hole & verifiziere gepinntes appimagetool v${APPIMAGETOOL_VERSION} ==="
mkdir -p "${CACHE_DIR}"

need_download=true
if [ -f "${APPIMAGETOOL}" ]; then
    CURRENT_HASH=$(sha256sum "${APPIMAGETOOL}" | cut -d' ' -f1)
    if [ "${CURRENT_HASH}" = "${APPIMAGETOOL_SHA256}" ]; then
        need_download=false
    else
        echo "Gecachtes appimagetool hat ungültigen Hash. Lade neu herunter..."
        rm -f "${APPIMAGETOOL}"
    fi
fi

if [ "${need_download}" = true ]; then
    echo "Lade appimagetool v${APPIMAGETOOL_VERSION} herunter..."
    curl -fsSL -o "${APPIMAGETOOL}.tmp" "${APPIMAGETOOL_URL}"
    DOWNLOAD_HASH=$(sha256sum "${APPIMAGETOOL}.tmp" | cut -d' ' -f1)
    if [ "${DOWNLOAD_HASH}" != "${APPIMAGETOOL_SHA256}" ]; then
        echo "FEHLER: SHA256-Prüfsumme für appimagetool stimmt nicht überein!"
        echo "Erwartet: ${APPIMAGETOOL_SHA256}"
        echo "Erhalten: ${DOWNLOAD_HASH}"
        rm -f "${APPIMAGETOOL}.tmp"
        exit 1
    fi
    mv "${APPIMAGETOOL}.tmp" "${APPIMAGETOOL}"
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
