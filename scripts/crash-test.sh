#!/usr/bin/env bash
# ==============================================================================
# scripts/crash-test.sh
# Stresstest für ink-notes Speicher- und Autosave-Robustheit.
# Simuliert harte Programmabstürze (SIGKILL / kill -9) während Schreibvorgängen
# und verifiziert die Integrität der Notizbücher und Seiten.
# ==============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

ITERATIONS="${1:-200}"
TEST_DIR="${REPO_ROOT}/target/crash_test_workspace"
CRASH_SIM_BIN="${REPO_ROOT}/target/release/crash_sim"

echo "=================================================================="
echo " ink-notes Crash- & Datenverlust-Stresstest"
echo " Iterationen: ${ITERATIONS}"
echo " Testverzeichnis: ${TEST_DIR}"
echo "=================================================================="

# 1. Sicherstellen, dass das Test-Binary gebaut ist
if [ ! -f "${CRASH_SIM_BIN}" ]; then
    echo "Baue crash_sim Release-Binary..."
    cargo build --release --bin crash_sim --manifest-path "${REPO_ROOT}/Cargo.toml"
fi

# 2. Sauberes Testverzeichnis anlegen und Notizbuch initialisieren
rm -rf "${TEST_DIR}"
mkdir -p "${TEST_DIR}"
"${CRASH_SIM_BIN}" init "${TEST_DIR}"

SUCCESS_COUNT=0
INTERRUPTED_WRITES=0

trap 'rm -rf "${TEST_DIR}"' EXIT

echo "Starte Testlauf mit ${ITERATIONS} SIGKILL-Zyklen..."

for i in $(seq 1 "${ITERATIONS}"); do
    # Worker im Hintergrund starten
    "${CRASH_SIM_BIN}" worker "${TEST_DIR}" > /dev/null 2>&1 &
    WORKER_PID=$!

    # Zufällige Schlafdauer zwischen 2ms und 35ms, um Schreiboperationen mitten im Flug zu treffen
    # RANDOM % 33 + 2 ergibt 2..34 Millisekunden
    MS_DELAY=$(( (RANDOM % 33) + 2 ))
    DELAY="0.$(printf "%03d" "$MS_DELAY")"
    sleep "${DELAY}"

    # Harter Prozessabbruch ohne Clean-up (SIGKILL)
    if kill -9 "${WORKER_PID}" 2>/dev/null; then
        INTERRUPTED_WRITES=$((INTERRUPTED_WRITES + 1))
    fi
    wait "${WORKER_PID}" 2>/dev/null || true

    # Integritäts- und Konsistenzprüfung
    if ! "${CRASH_SIM_BIN}" verify "${TEST_DIR}" > /dev/null 2>&1; then
        echo ""
        echo "❌ FEHLER in Iteration ${i}!"
        echo "Notizbuch im Zustand nach Absturz nicht konsistent ladbar!"
        "${CRASH_SIM_BIN}" verify "${TEST_DIR}" || true
        trap - EXIT
        exit 1
    fi

    SUCCESS_COUNT=$((SUCCESS_COUNT + 1))
    if [ $((i % 25)) -eq 0 ] || [ "${i}" -eq "${ITERATIONS}" ]; then
        echo "  [$(date +%T)] Iteration ${i}/${ITERATIONS} bestanden (erfolgreich unterbrochene Schreibvorgänge: ${INTERRUPTED_WRITES})."
    fi
done

echo ""
echo "=================================================================="
echo "✅ ALLE ${ITERATIONS} CRASH-TESTS ERFOLGREICH BESTANDEN!"
echo "   - Überstandene SIGKILLs: ${INTERRUPTED_WRITES}"
echo "   - Konsistente Ladevorgänge: ${SUCCESS_COUNT}/${ITERATIONS}"
echo "   - Keine korrupten JSON-Dateien ohne Quarantäne"
echo "   - Keine unvollständigen Atom-Schreibungen"
echo "=================================================================="
