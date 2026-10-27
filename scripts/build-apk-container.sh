#!/usr/bin/env bash
# ==============================================================================
# Builds the APK inside the container from packaging/android/Containerfile (Issue #45),
# with podman (preferred) or docker. Needs nothing else on the host.
# Result: target/ink-notes_<version>_arm64.apk
#
# Signing key as in scripts/package-android.sh: ANDROID_KEYSTORE (path on the host),
# ANDROID_KEYSTORE_PASS, ANDROID_KEY_ALIAS; otherwise a debug key (kept below
# target/android-container/home, so that test builds can update each other).
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${ROOT_DIR}"

if command -v podman >/dev/null 2>&1; then
    ENGINE=podman
    USER_ARGS=(--userns=keep-id)
elif command -v docker >/dev/null 2>&1; then
    ENGINE=docker
    USER_ARGS=(--user "$(id -u):$(id -g)")
else
    echo "FEHLER: weder podman noch docker gefunden."
    exit 1
fi

IMAGE="ink-notes-android"
echo "=== Baue Container-Image ${IMAGE} ==="
"${ENGINE}" build -t "${IMAGE}" -f packaging/android/Containerfile .

mkdir -p target/android-container/home
ARGS=(
    --rm "${USER_ARGS[@]}"
    -v "${ROOT_DIR}:/src"
    -e HOME=/src/target/android-container/home
    -e CARGO_TARGET_DIR=/src/target/android-container
    # Downloaded crates stay between runs
    -e CARGO_HOME=/src/target/android-container/cargo
)
if [ -n "${ANDROID_KEYSTORE:-}" ]; then
    ARGS+=(
        -v "$(realpath "${ANDROID_KEYSTORE}"):/keystore:ro"
        -e ANDROID_KEYSTORE=/keystore
        -e ANDROID_KEYSTORE_PASS -e ANDROID_KEY_ALIAS
    )
fi
"${ENGINE}" run "${ARGS[@]}" "${IMAGE}" scripts/package-android.sh
