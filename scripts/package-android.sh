#!/usr/bin/env bash
# ==============================================================================
# Builds the Android app target/ink-notes_<version>_arm64.apk (Issues #42, #45)
# without Gradle: the Rust library is built as cdylib with the NDK, javac and d8 compile the
# activity (packaging/android/java), aapt2 links the manifest and the icon, zipalign and
# apksigner finish the APK. Needs a JDK (javac, keytool).
#
# Needs the Android SDK (build-tools, platform android-34) and the NDK:
#   ANDROID_HOME      SDK folder (default: ~/android-sdk)
#   ANDROID_NDK_HOME  NDK folder (default: newest in $ANDROID_HOME/ndk)
#
# Signing key (all three, otherwise the debug key ~/.android/debug.keystore is used and
# created if missing; such an APK is only meant for testing):
#   ANDROID_KEYSTORE, ANDROID_KEYSTORE_PASS, ANDROID_KEY_ALIAS
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${ROOT_DIR}"

TARGET="aarch64-linux-android"
ABI="arm64-v8a"
MIN_SDK=26
TARGET_SDK=34
VERSION=$(grep -m1 '^version = ' Cargo.toml | cut -d '"' -f2)
IFS=. read -r MAJOR MINOR PATCH <<<"${VERSION%%-*}"
VERSION_CODE=$((MAJOR * 10000 + MINOR * 100 + PATCH))
APK_NAME="ink-notes_${VERSION}_arm64.apk"

ANDROID_HOME="${ANDROID_HOME:-${HOME}/android-sdk}"
if [ -z "${ANDROID_NDK_HOME:-}" ]; then
    ANDROID_NDK_HOME=$(ls -d "${ANDROID_HOME}"/ndk/*/ 2>/dev/null | sort -V | tail -n1)
fi
BUILD_TOOLS=$(ls -d "${ANDROID_HOME}"/build-tools/*/ 2>/dev/null | sort -V | tail -n1)
ANDROID_JAR="${ANDROID_HOME}/platforms/android-${TARGET_SDK}/android.jar"
NDK_BIN="${ANDROID_NDK_HOME%/}/toolchains/llvm/prebuilt/linux-x86_64/bin"

for needed in "${NDK_BIN}/${TARGET}${MIN_SDK}-clang" "${BUILD_TOOLS}aapt2" "${BUILD_TOOLS}d8" "${ANDROID_JAR}"; do
    if [ ! -e "${needed}" ]; then
        echo "FEHLER: ${needed} nicht gefunden (Android-SDK/NDK, siehe Kopf des Skripts)."
        exit 1
    fi
done
if ! command -v javac >/dev/null 2>&1; then
    echo "FEHLER: javac nicht gefunden (JDK 17 oder neuer)."
    exit 1
fi
if command -v rustup >/dev/null 2>&1; then
    rustup target add "${TARGET}"
fi

echo "=== [1/4] Baue libink_notes.so (${TARGET}) ==="
export CC_aarch64_linux_android="${NDK_BIN}/${TARGET}${MIN_SDK}-clang"
export AR_aarch64_linux_android="${NDK_BIN}/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="${NDK_BIN}/${TARGET}${MIN_SDK}-clang"
cargo rustc --release --lib --crate-type cdylib --target "${TARGET}"
LIB="${CARGO_TARGET_DIR:-target}/${TARGET}/release/libink_notes.so"

echo "=== [2/4] Ressourcen und Manifest ==="
WORK="target/android-apk"
rm -rf "${WORK}"
mkdir -p "${WORK}/res/mipmap-xxxhdpi" "${WORK}/compiled" "${WORK}/apk/lib/${ABI}" "${WORK}/classes"
cp assets/ink-notes.png "${WORK}/res/mipmap-xxxhdpi/ic_launcher.png"
sed -e "s/@VERSION_CODE@/${VERSION_CODE}/" -e "s/@VERSION@/${VERSION}/" \
    packaging/android/AndroidManifest.xml >"${WORK}/AndroidManifest.xml"
"${BUILD_TOOLS}aapt2" compile --dir "${WORK}/res" -o "${WORK}/compiled/res.zip"
"${BUILD_TOOLS}aapt2" link -o "${WORK}/unaligned.apk" -I "${ANDROID_JAR}" \
    --manifest "${WORK}/AndroidManifest.xml" \
    --min-sdk-version "${MIN_SDK}" --target-sdk-version "${TARGET_SDK}" \
    "${WORK}/compiled/res.zip"

echo "=== [3/4] Activity und native Bibliothek hinzufügen ==="
javac -source 8 -target 8 -nowarn -Xlint:-options -classpath "${ANDROID_JAR}" \
    -d "${WORK}/classes" $(find packaging/android/java -name '*.java')
"${BUILD_TOOLS}d8" --min-api "${MIN_SDK}" --lib "${ANDROID_JAR}" --output "${WORK}/apk" \
    $(find "${WORK}/classes" -name '*.class')
cp "${LIB}" "${WORK}/apk/lib/${ABI}/"
(cd "${WORK}/apk" && zip -q -r ../unaligned.apk classes.dex lib)
"${BUILD_TOOLS}zipalign" -f -p 4 "${WORK}/unaligned.apk" "${WORK}/aligned.apk"

echo "=== [4/4] Signieren ==="
if [ -n "${ANDROID_KEYSTORE:-}" ] && [ -n "${ANDROID_KEYSTORE_PASS:-}" ] && [ -n "${ANDROID_KEY_ALIAS:-}" ]; then
    KEYSTORE="${ANDROID_KEYSTORE}"
    KEY_ALIAS="${ANDROID_KEY_ALIAS}"
    export KS_PASS="${ANDROID_KEYSTORE_PASS}"
else
    KEYSTORE="${HOME}/.android/debug.keystore"
    KEY_ALIAS="androiddebugkey"
    export KS_PASS="android"
    if [ ! -f "${KEYSTORE}" ]; then
        mkdir -p "$(dirname "${KEYSTORE}")"
        keytool -genkeypair -keystore "${KEYSTORE}" -storepass android -keypass android \
            -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 \
            -dname "CN=Android Debug,O=Android,C=US"
    fi
    echo "Hinweis: Signiert mit dem Debug-Schlüssel ${KEYSTORE} (nur zum Testen)."
fi
"${BUILD_TOOLS}apksigner" sign --ks "${KEYSTORE}" --ks-key-alias "${KEY_ALIAS}" \
    --ks-pass env:KS_PASS --key-pass env:KS_PASS \
    --out "target/${APK_NAME}" "${WORK}/aligned.apk"
"${BUILD_TOOLS}apksigner" verify "target/${APK_NAME}"
ls -lh "target/${APK_NAME}"
