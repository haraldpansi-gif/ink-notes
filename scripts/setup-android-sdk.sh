#!/usr/bin/env bash
# ==============================================================================
# Installs the Android SDK parts that scripts/package-android.sh needs, in fixed versions
# (Issue #45): command-line tools, platform 34, build-tools 34.0.0 and NDK 28.2.
# Used by the CI and packaging/android/Containerfile; works for a local SDK as well.
#
#   ANDROID_HOME  target folder (default: ~/android-sdk)
#
# Needs curl, unzip and a JDK 17 or newer (sdkmanager is a Java program).
# ==============================================================================
set -euo pipefail

ANDROID_HOME="${ANDROID_HOME:-${HOME}/android-sdk}"
CMDLINE_TOOLS_ZIP="commandlinetools-linux-11076708_latest.zip"
CMDLINE_TOOLS_SHA256="2d2d50857e4eb553af5a6dc3ad507a17adf43d115264b1afc116f95c92e5e258"
PACKAGES=(
    "platforms;android-34"
    "build-tools;34.0.0"
    "ndk;28.2.13676358"
)

SDKMANAGER="${ANDROID_HOME}/cmdline-tools/latest/bin/sdkmanager"
if [ ! -x "${SDKMANAGER}" ]; then
    echo "=== Lade Android Command-line Tools ==="
    TMP="$(mktemp -d)"
    trap 'rm -rf "${TMP}"' EXIT
    curl -sSfL -o "${TMP}/${CMDLINE_TOOLS_ZIP}" \
        "https://dl.google.com/android/repository/${CMDLINE_TOOLS_ZIP}"
    echo "${CMDLINE_TOOLS_SHA256}  ${TMP}/${CMDLINE_TOOLS_ZIP}" | sha256sum -c -
    unzip -q "${TMP}/${CMDLINE_TOOLS_ZIP}" -d "${TMP}"
    mkdir -p "${ANDROID_HOME}/cmdline-tools"
    rm -rf "${ANDROID_HOME}/cmdline-tools/latest"
    mv "${TMP}/cmdline-tools" "${ANDROID_HOME}/cmdline-tools/latest"
fi

echo "=== Installiere ${PACKAGES[*]} nach ${ANDROID_HOME} ==="
# sdkmanager ends with an error when `yes` is cut off by a closed pipe; that's fine
(yes || true) | "${SDKMANAGER}" --sdk_root="${ANDROID_HOME}" --licenses >/dev/null
"${SDKMANAGER}" --sdk_root="${ANDROID_HOME}" --install "${PACKAGES[@]}" >/dev/null
echo "Android-SDK bereit: ${ANDROID_HOME}"
