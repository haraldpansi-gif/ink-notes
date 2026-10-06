#!/usr/bin/env bash
# ==============================================================================
# Veröffentlicht ein Release auf dem Gitea-Server (gitea.pansi.eu)
# Lädt .deb, .AppImage und SHA256SUMS.txt als Release-Assets hoch.
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${ROOT_DIR}"

GITEA_SERVER="${GITEA_SERVER:-https://gitea.pansi.eu}"
REPO_OWNER="${REPO_OWNER:-harald}"
REPO_NAME="${REPO_NAME:-ink-notes}"

# Token aus Argument oder Umgebungsvariablen ermitteln
TOKEN="${1:-${INK_NOTES_GITEA_TOKEN:-${GITEA_TOKEN:-${GITHUB_TOKEN:-}}}}"

if [ -z "${TOKEN}" ]; then
    echo "FEHLER: Kein Gitea-API-Token angegeben."
    echo "Verwendung: $0 <GITEA_API_TOKEN>"
    echo "Oder setze die Umgebungsvariable INK_NOTES_GITEA_TOKEN."
    exit 1
fi

VERSION=$(grep -m1 '^version = ' Cargo.toml | cut -d '"' -f2)
TAG_NAME="v${VERSION}"
RELEASE_TITLE="ink-notes ${TAG_NAME}"

DEB_FILE="${ROOT_DIR}/target/ink-notes_${VERSION}_amd64.deb"
APPIMAGE_FILE="${ROOT_DIR}/target/ink-notes_${VERSION}_x86_64.AppImage"
CHECKSUMS_FILE="${ROOT_DIR}/target/SHA256SUMS.txt"

# Sicherstellen, dass die Artefakte existieren
if [ ! -f "${DEB_FILE}" ] || [ ! -f "${APPIMAGE_FILE}" ] || [ ! -f "${CHECKSUMS_FILE}" ]; then
    echo "Artefakte nicht vollständig gefunden. Führe build-release.sh aus..."
    "${SCRIPT_DIR}/build-release.sh"
fi

echo "======================================================================"
echo "Veröffentliche Release ${TAG_NAME} auf ${GITEA_SERVER}/${REPO_OWNER}/${REPO_NAME}"
echo "======================================================================"

RELEASE_BODY=$(cat <<EOF
## ink-notes ${TAG_NAME}

Latenzoptimierte Vektor-Notizbuch- und Inking-Anwendung für Linux (Zorin OS 18 / Ubuntu 24.04).

### Highlights:
- **Zwei-Stufen-Inking-Modell**: Kausale Echtzeit-Glättung via 1€-Filter & akausale Ecken-Erhaltung (Corner Preservation)
- **Geometrie- und Formerkennung**: Geraden, Rechtecke, Kreise, Rauten, Polygone & Koordinatensysteme (Draw-and-Hold)
- **Viewport**: Stufenloser Zoom (25% - 500%) und Panning
- **Multi-Dokumenten-Manager**: Unbegrenzte Notizbücher mit atomarem Autosave
- **In-App Updater**: Automatische Versionsprüfung und Aktualisierung via Gitea Releases
- **Vektor-PDF-Export**: Verluste freie Ausgabe für Druck und Weitergabe

### Installation:
- **Zorin OS / Ubuntu / Debian (.deb)**:
  \`\`\`bash
  sudo dpkg -i ink-notes_${VERSION}_amd64.deb
  \`\`\`
- **Portables AppImage (Alle Linux-Distributionen)**:
  \`\`\`bash
  chmod +x ink-notes_${VERSION}_x86_64.AppImage
  ./ink-notes_${VERSION}_x86_64.AppImage
  \`\`\`
EOF
)

# 1. Release via Gitea REST API erstellen (oder ID abfragen, falls bereits existent)
CREATE_PAYLOAD=$(python3 -c "
import json, sys
data = {
    'tag_name': '${TAG_NAME}',
    'target_commitish': 'main',
    'name': '${RELEASE_TITLE}',
    'body': '''${RELEASE_BODY}''',
    'draft': False,
    'prerelease': False
}
print(json.dumps(data))
")

API_BASE="${GITEA_SERVER}/api/v1/repos/${REPO_OWNER}/${REPO_NAME}"

echo "Erstelle Release über Gitea API..."
HTTP_RESPONSE=$(curl -s -w "\nHTTP_STATUS:%{http_code}" \
    -X POST "${API_BASE}/releases" \
    -H "Accept: application/json" \
    -H "Content-Type: application/json" \
    -H "Authorization: token ${TOKEN}" \
    -d "${CREATE_PAYLOAD}")

HTTP_STATUS=$(echo "${HTTP_RESPONSE}" | grep "HTTP_STATUS:" | cut -d':' -f2)
RESPONSE_BODY=$(echo "${HTTP_RESPONSE}" | sed '/HTTP_STATUS:/d')

RELEASE_ID=""
if [ "${HTTP_STATUS}" -eq 201 ]; then
    RELEASE_ID=$(echo "${RESPONSE_BODY}" | python3 -c "import sys, json; print(json.load(sys.stdin)['id'])")
    echo "Release erfolgreich angelegt (ID: ${RELEASE_ID})!"
else
    echo "Release existiert womöglich bereits (Status ${HTTP_STATUS}). Suche bestehendes Release..."
    EXISTING_RELEASE=$(curl -fsSL \
        -H "Accept: application/json" \
        -H "Authorization: token ${TOKEN}" \
        "${API_BASE}/releases/tags/${TAG_NAME}" || true)
    if [ -n "${EXISTING_RELEASE}" ]; then
        RELEASE_ID=$(echo "${EXISTING_RELEASE}" | python3 -c "import sys, json; print(json.load(sys.stdin).get('id', ''))")
    fi
fi

if [ -z "${RELEASE_ID}" ]; then
    echo "FEHLER: Konnte Release nicht erstellen oder abfragen. Serverantwort:"
    echo "${RESPONSE_BODY}"
    exit 1
fi

# 2. Assets hochladen
upload_asset() {
    local file_path="$1"
    local filename="$(basename "${file_path}")"
    echo "Lade Asset hoch: ${filename}..."
    curl -fsSL \
        -X POST "${API_BASE}/releases/${RELEASE_ID}/assets?name=${filename}" \
        -H "Accept: application/json" \
        -H "Authorization: token ${TOKEN}" \
        -H "Content-Type: application/octet-stream" \
        --data-binary "@${file_path}" > /dev/null
    echo "✓ ${filename} erfolgreich hochgeladen."
}

upload_asset "${DEB_FILE}"
upload_asset "${APPIMAGE_FILE}"
upload_asset "${CHECKSUMS_FILE}"

echo "======================================================================"
echo "Release ${TAG_NAME} erfolgreich veröffentlicht!"
echo "URL: ${GITEA_SERVER}/${REPO_OWNER}/${REPO_NAME}/releases/tag/${TAG_NAME}"
echo "======================================================================"
