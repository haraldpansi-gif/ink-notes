#!/usr/bin/env bash
# ==============================================================================
# Veröffentlicht ein Release auf dem Gitea-Server (gitea.pansi.eu)
# Lädt .deb, .AppImage, SHA256SUMS.txt und SHA256SUMS.txt.minisig idempotent als Release-Assets hoch.
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${ROOT_DIR}"

GITEA_SERVER="${GITEA_SERVER:-https://gitea.pansi.eu}"
REPO_OWNER="${REPO_OWNER:-harald}"
REPO_NAME="${REPO_NAME:-ink-notes}"

# Token ausschließlich aus Umgebungsvariablen ermitteln (Fallback für Legacy-Aufruf mit Argument)
TOKEN="${INK_NOTES_GITEA_TOKEN:-${GITEA_TOKEN:-${GITHUB_TOKEN:-}}}"
if [ -z "${TOKEN}" ] && [ -n "${1:-}" ]; then
    TOKEN="$1"
fi

if [ -z "${TOKEN}" ]; then
    echo "FEHLER: Kein Gitea-API-Token angegeben."
    echo "Bitte setze die Umgebungsvariable GITEA_TOKEN oder INK_NOTES_GITEA_TOKEN."
    exit 1
fi

VERSION=$(grep -m1 '^version = ' Cargo.toml | cut -d '"' -f2)
TAG_NAME="v${VERSION}"
RELEASE_TITLE="ink-notes ${TAG_NAME}"

DEB_FILE="${ROOT_DIR}/target/ink-notes_${VERSION}_amd64.deb"
APPIMAGE_FILE="${ROOT_DIR}/target/ink-notes_${VERSION}_x86_64.AppImage"
CHECKSUMS_FILE="${ROOT_DIR}/target/SHA256SUMS.txt"
SIGNATURE_FILE="${CHECKSUMS_FILE}.minisig"
TEMPLATE_FILE="${ROOT_DIR}/packaging/release-notes.md.tmpl"

if [ ! -f "${TEMPLATE_FILE}" ]; then
    echo "FEHLER: Release-Template '${TEMPLATE_FILE}' nicht gefunden."
    exit 1
fi

# Sicherstellen, dass die Artefakte existieren
if [ ! -f "${DEB_FILE}" ] || [ ! -f "${APPIMAGE_FILE}" ] || [ ! -f "${CHECKSUMS_FILE}" ]; then
    echo "Artefakte nicht vollständig gefunden. Führe build-release.sh aus..."
    "${SCRIPT_DIR}/build-release.sh"
fi

# Ohne Signatur lehnen installierte Apps das Release als Update ab
if [ ! -f "${SIGNATURE_FILE}" ]; then
    echo "FEHLER: '${SIGNATURE_FILE}' fehlt. Release mit gesetztem MINISIGN_SECRET_KEY bauen."
    exit 1
fi

echo "======================================================================"
echo "Veröffentliche Release ${TAG_NAME} auf ${GITEA_SERVER}/${REPO_OWNER}/${REPO_NAME}"
echo "======================================================================"

# 1. Payload robust per Python aus Template erzeugen (verhindert Shell-Escaping-Probleme)
PAYLOAD_FILE="$(mktemp)"
CLEANUP_FILES=("${PAYLOAD_FILE}")
cleanup() {
    for f in "${CLEANUP_FILES[@]}"; do
        rm -f "${f}"
    done
}
trap cleanup EXIT

RELEASE_VERSION="${VERSION}" RELEASE_TAG="${TAG_NAME}" RELEASE_TITLE="${RELEASE_TITLE}" \
python3 -c '
import json, os, sys

template_path = sys.argv[1]
with open(template_path, "r", encoding="utf-8") as f:
    template_content = f.read()

version = os.environ.get("RELEASE_VERSION", "")
tag = os.environ.get("RELEASE_TAG", "")
title = os.environ.get("RELEASE_TITLE", "")

body = template_content.replace("{{VERSION}}", version).replace("{{TAG_NAME}}", tag)

payload = {
    "tag_name": tag,
    "target_commitish": "main",
    "name": title,
    "body": body,
    "draft": False,
    "prerelease": False
}
print(json.dumps(payload))
' "${TEMPLATE_FILE}" > "${PAYLOAD_FILE}"

API_BASE="${GITEA_SERVER}/api/v1/repos/${REPO_OWNER}/${REPO_NAME}"

# 2. Prüfen, ob Release bereits existiert (Idempotenz)
echo "Prüfe bestehende Releases auf ${GITEA_SERVER}..."
TAG_RESP_FILE="$(mktemp)"
CLEANUP_FILES+=("${TAG_RESP_FILE}")

HTTP_STATUS=$(curl -s -S -o "${TAG_RESP_FILE}" -w "%{http_code}" \
    -X GET "${API_BASE}/releases/tags/${TAG_NAME}" \
    -H "Accept: application/json" \
    -H "Authorization: token ${TOKEN}" || echo "000")

RELEASE_ID=""

if [ "${HTTP_STATUS}" = "200" ]; then
    RELEASE_ID=$(python3 -c '
import sys, json
try:
    data = json.load(open(sys.argv[1]))
    print(data.get("id", ""))
except Exception:
    pass
' "${TAG_RESP_FILE}")
    echo "Release ${TAG_NAME} existiert bereits (ID: ${RELEASE_ID}). Aktualisiere Release-Notizen..."
    PATCH_STATUS=$(curl -s -S -o /dev/null -w "%{http_code}" \
        -X PATCH "${API_BASE}/releases/${RELEASE_ID}" \
        -H "Accept: application/json" \
        -H "Content-Type: application/json" \
        -H "Authorization: token ${TOKEN}" \
        -d @"${PAYLOAD_FILE}" || echo "000")
    if [ "${PATCH_STATUS}" != "200" ]; then
        echo "Hinweis: Aktualisierung der Metadaten ergab HTTP ${PATCH_STATUS}."
    fi
elif [ "${HTTP_STATUS}" = "404" ]; then
    echo "Release existiert noch nicht. Erstelle neues Release ${TAG_NAME}..."
    CREATE_RESP_FILE="$(mktemp)"
    CLEANUP_FILES+=("${CREATE_RESP_FILE}")
    CREATE_STATUS=$(curl -s -S -o "${CREATE_RESP_FILE}" -w "%{http_code}" \
        -X POST "${API_BASE}/releases" \
        -H "Accept: application/json" \
        -H "Content-Type: application/json" \
        -H "Authorization: token ${TOKEN}" \
        -d @"${PAYLOAD_FILE}" || echo "000")

    if [ "${CREATE_STATUS}" = "201" ]; then
        RELEASE_ID=$(python3 -c '
import sys, json
try:
    data = json.load(open(sys.argv[1]))
    print(data.get("id", ""))
except Exception:
    pass
' "${CREATE_RESP_FILE}")
        echo "Release erfolgreich angelegt (ID: ${RELEASE_ID})!"
    else
        echo "FEHLER: Release konnte nicht angelegt werden (HTTP ${CREATE_STATUS})."
        cat "${CREATE_RESP_FILE}"
        exit 1
    fi
else
    echo "FEHLER: Unerwartete Serverantwort beim Abfragen des Release-Tags (HTTP ${HTTP_STATUS})."
    cat "${TAG_RESP_FILE}"
    exit 1
fi

if [ -z "${RELEASE_ID}" ]; then
    echo "FEHLER: Keine gültige Release-ID ermittelt."
    exit 1
fi

# 3. Vorhandene Assets auflisten (um Dubletten vor Upload sauber zu löschen)
ASSETS_LIST_FILE="$(mktemp)"
CLEANUP_FILES+=("${ASSETS_LIST_FILE}")

LIST_STATUS=$(curl -s -S -o "${ASSETS_LIST_FILE}" -w "%{http_code}" \
    -X GET "${API_BASE}/releases/${RELEASE_ID}/assets" \
    -H "Accept: application/json" \
    -H "Authorization: token ${TOKEN}" || echo "000")

if [ "${LIST_STATUS}" != "200" ]; then
    echo "Hinweis: Konnte bestehende Assets nicht abrufen (HTTP ${LIST_STATUS})."
fi

upload_asset() {
    local file_path="$1"
    local filename
    filename="$(basename "${file_path}")"

    # Asset-Namen in URL URL-kodieren
    local encoded_name
    encoded_name=$(python3 -c 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1]))' "${filename}")

    # Prüfen, ob bereits Assets mit demselben Namen existieren (können mehrere sein)
    local existing_ids=()
    if [ -f "${ASSETS_LIST_FILE}" ] && [ -s "${ASSETS_LIST_FILE}" ]; then
        mapfile -t existing_ids < <(python3 -c '
import sys, json
try:
    assets = json.load(open(sys.argv[1]))
    target = sys.argv[2]
    for a in assets:
        if a.get("name") == target and a.get("id"):
            print(a.get("id"))
except Exception:
    pass
' "${ASSETS_LIST_FILE}" "${filename}")
    fi

    for eid in "${existing_ids[@]}"; do
        if [ -n "${eid}" ]; then
            echo "Lösche bestehendes Asset '${filename}' (ID: ${eid}) vor Re-Upload..."
            local del_status
            del_status=$(curl -s -S -o /dev/null -w "%{http_code}" \
                -X DELETE "${API_BASE}/releases/${RELEASE_ID}/assets/${eid}" \
                -H "Authorization: token ${TOKEN}" || echo "000")
            if [ "${del_status}" != "204" ] && [ "${del_status}" != "200" ]; then
                echo "Warnung: Löschen von bestehendem Asset '${filename}' ergab HTTP ${del_status}."
            fi
        fi
    done

    echo "Lade Asset hoch: ${filename}..."
    local upload_res_file="$(mktemp)"
    CLEANUP_FILES+=("${upload_res_file}")

    local upload_status
    upload_status=$(curl -s -S -o "${upload_res_file}" -w "%{http_code}" \
        -X POST "${API_BASE}/releases/${RELEASE_ID}/assets?name=${encoded_name}" \
        -H "Accept: application/json" \
        -H "Authorization: token ${TOKEN}" \
        -H "Content-Type: application/octet-stream" \
        --data-binary "@${file_path}" || echo "000")

    if [ "${upload_status}" = "201" ]; then
        echo "✓ ${filename} erfolgreich hochgeladen."
    else
        echo "FEHLER: Asset-Upload für '${filename}' fehlgeschlagen (HTTP ${upload_status})."
        cat "${upload_res_file}"
        exit 1
    fi
}

upload_asset "${DEB_FILE}"
upload_asset "${APPIMAGE_FILE}"
upload_asset "${CHECKSUMS_FILE}"
upload_asset "${SIGNATURE_FILE}"

echo "======================================================================"
echo "Release ${TAG_NAME} erfolgreich veröffentlicht!"
echo "URL: ${GITEA_SERVER}/${REPO_OWNER}/${REPO_NAME}/releases/tag/${TAG_NAME}"
echo "======================================================================"
