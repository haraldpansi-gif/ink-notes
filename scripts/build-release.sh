#!/usr/bin/env bash
# ==============================================================================
# Haupt-Release-Build-Skript für ink-notes (Zorin OS 18 / Linux)
# Erstellt .deb, .AppImage, SHA256-Prüfsummen und deren Minisign-Signatur in target/
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

# 4. Prüfsummen signieren (Pflicht für In-App-Updates)
# Signiert wird mit rsign2 (minisign-kompatibles Format): Der Schlüssel wurde mit
# `rsign generate -W` erzeugt; das minisign-CLI kann ihn ohne Terminal nicht laden.
SIGNATURE_FILE="${CHECKSUM_FILE}.minisig"
rm -f "${SIGNATURE_FILE}"
# Schlüssel kann von rustfmt in die Folgezeile umgebrochen sein
EMBEDDED_PUBKEY=$(grep -m1 -A1 'pub const UPDATE_SIGNING_PUBLIC_KEY' "${ROOT_DIR}/src/updater.rs" | grep -o '"[^"]*"' | head -n1 | tr -d '"')
if [ -n "${MINISIGN_SECRET_KEY:-}" ]; then
    if [ -z "${EMBEDDED_PUBKEY}" ]; then
        echo "FEHLER: UPDATE_SIGNING_PUBLIC_KEY in src/updater.rs ist leer – diese Version könnte keine Updates prüfen."
        exit 1
    fi
    if ! command -v rsign >/dev/null 2>&1; then
        echo "FEHLER: rsign nicht gefunden (Installation: cargo install rsign2 --locked)."
        exit 1
    fi
    echo "=== Signiere ${CHECKSUM_FILE} mit rsign2 ==="
    KEY_FILE="$(mktemp)"
    trap 'rm -f "${KEY_FILE}"' EXIT
    printf '%s\n' "${MINISIGN_SECRET_KEY}" > "${KEY_FILE}"
    rsign sign -W -s "${KEY_FILE}" -t "ink-notes v${VERSION}" -x "${SIGNATURE_FILE}" "${CHECKSUM_FILE}"
    rm -f "${KEY_FILE}"
    # Stellt sicher, dass der geheime Schlüssel zum eingebauten öffentlichen Schlüssel passt
    # rsign beendet sich bei falscher Key-ID mit Exit-Code 0, daher zusätzlich die Meldung prüfen
    if ! VERIFY_OUT=$(rsign verify -P "${EMBEDDED_PUBKEY}" -x "${SIGNATURE_FILE}" "${CHECKSUM_FILE}" 2>&1) \
        || ! grep -q "Signature and comment signature verified" <<<"${VERIFY_OUT}"; then
        echo "${VERIFY_OUT}"
        echo "FEHLER: Signatur passt nicht zum eingebauten öffentlichen Schlüssel (UPDATE_SIGNING_PUBLIC_KEY)."
        rm -f "${SIGNATURE_FILE}"
        exit 1
    fi
    echo "${VERIFY_OUT}"
elif [ "${INK_NOTES_REQUIRE_SIGNATURE:-0}" = "1" ]; then
    echo "FEHLER: MINISIGN_SECRET_KEY ist nicht gesetzt, eine Signatur ist aber Pflicht."
    exit 1
else
    echo "WARNUNG: MINISIGN_SECRET_KEY nicht gesetzt – ${CHECKSUM_FILE} bleibt unsigniert."
    echo "         Installierte Apps lehnen ein solches Release als Update ab."
fi

echo "======================================================================"
echo "Release-Artefakte für v${VERSION} erfolgreich erstellt:"
echo "======================================================================"
ls -lh "${DEB_NAME}" "${APPIMAGE_NAME}" "${CHECKSUM_FILE}"
if [ -f "${SIGNATURE_FILE}" ]; then ls -lh "${SIGNATURE_FILE}"; fi
echo ""
echo "Inhalt von target/${CHECKSUM_FILE}:"
cat "${CHECKSUM_FILE}"
echo "======================================================================"
