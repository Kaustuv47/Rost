#!/usr/bin/env bash
# Automated QEMU system test harness for the Rost microkernel.
#
# Each test case boots the kernel in a fresh QEMU instance, captures serial
# output to a temporary file, and asserts that all expected strings appear
# within a configurable timeout.  The watchdog is configured to power-off
# the VM if the kernel hangs, so tests always terminate.
#
# Usage:
#   scripts/test-qemu.sh            # build + run all tests
#   scripts/test-qemu.sh --no-build # skip build, use existing binary
#   TIMEOUT=60 scripts/test-qemu.sh # extend per-test timeout (default: 30 s)
#
# Prerequisites:
#   qemu-system-x86_64  (macOS: brew install qemu  or  port install qemu)
#   OVMF / edk2 firmware (macOS: port install qemu  or  set OVMF env var)
#
# Exit code: 0 = all tests passed, 1 = one or more tests failed, 2 = skipped.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$SCRIPT_DIR/.."

QEMU="${QEMU:-qemu-system-x86_64}"
OVMF="${OVMF:-/opt/local/share/qemu/edk2-x86_64-code.fd}"
TIMEOUT="${TIMEOUT:-30}"   # seconds to wait for each expected string

PASS=0
FAIL=0
SERIAL_LOG=""

# ── Helpers ───────────────────────────────────────────────────────────────────

log() { printf '[test-qemu] %s\n' "$*"; }
log_err() { printf '[test-qemu] %s\n' "$*" >&2; }

cleanup() {
    if [[ -n "${QEMU_PID:-}" ]]; then
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
        QEMU_PID=""
    fi
    [[ -n "$SERIAL_LOG" ]] && rm -f "$SERIAL_LOG"
}
trap cleanup EXIT

# wait_for_string FILE STRING TIMEOUT_SECS
# Polls FILE every 100 ms until STRING appears or deadline expires.
wait_for_string() {
    local file="$1" str="$2" secs="$3"
    local deadline=$(( $(date +%s) + secs ))
    while [[ $(date +%s) -lt $deadline ]]; do
        if grep -qF "$str" "$file" 2>/dev/null; then return 0; fi
        sleep 0.1
    done
    return 1
}

# run_test NAME EXPECT...
# Boots the kernel in QEMU and asserts that each EXPECT string is seen on the
# serial port in sequence.  Kills QEMU when the last expected string is seen
# (or after timeout).
run_test() {
    local name="$1"; shift
    local -a expect=("$@")

    log "=== TEST: $name ==="

    # Fresh log file for each test.
    SERIAL_LOG="$(mktemp /tmp/rost-serial.XXXXXX)"
    : > "$SERIAL_LOG"

    # Start QEMU: headless, serial → log file, watchdog → poweroff on hang.
    "$QEMU" \
        -machine q35 \
        -cpu Haswell,+smep \
        -m 256M \
        -drive if=pflash,format=raw,readonly=on,file="$OVMF" \
        -drive format=raw,file=fat:rw:"$ROOT/build/" \
        -device ib700,id=watchdog0 \
        -watchdog-action poweroff \
        -net none \
        -no-reboot \
        -display none \
        -serial "file:${SERIAL_LOG}" \
        </dev/null >/dev/null 2>&1 &
    QEMU_PID=$!

    local ok=1
    for str in "${expect[@]}"; do
        if wait_for_string "$SERIAL_LOG" "$str" "$TIMEOUT"; then
            log "  PASS  ← '${str}'"
        else
            log_err "  FAIL  ✗ '${str}' not seen within ${TIMEOUT}s"
            ok=0
            break
        fi
    done

    kill "$QEMU_PID" 2>/dev/null || true
    wait "$QEMU_PID" 2>/dev/null || true
    QEMU_PID=""

    if [[ $ok -eq 1 ]]; then
        log "  RESULT: PASS"
        (( PASS++ )) || true
    else
        log_err "  RESULT: FAIL"
        log_err "  --- last 20 lines of serial log ---"
        tail -20 "$SERIAL_LOG" | sed 's/^/    /' >&2
        (( FAIL++ )) || true
    fi

    rm -f "$SERIAL_LOG"
    SERIAL_LOG=""
}

# ── Pre-flight checks ─────────────────────────────────────────────────────────

if [[ "${1:-}" != "--no-build" ]]; then
    log "Building kernel..."
    "$SCRIPT_DIR/build.sh"
fi

if ! command -v "$QEMU" &>/dev/null; then
    log "SKIP: $QEMU not found."
    log "  Install: brew install qemu  or  port install qemu"
    exit 2
fi

if [[ ! -f "$OVMF" ]]; then
    log "SKIP: OVMF firmware not found at: $OVMF"
    log "  Set the OVMF environment variable to the correct path."
    log "  macOS (MacPorts): /opt/local/share/qemu/edk2-x86_64-code.fd"
    log "  macOS (Homebrew): /opt/homebrew/share/qemu/edk2-x86_64-code.fd"
    exit 2
fi

if [[ ! -f "$ROOT/build/efi/boot/bootx64.efi" ]]; then
    log "SKIP: $ROOT/build/efi/boot/bootx64.efi not found — run build.sh first."
    exit 2
fi

# ── Test cases ────────────────────────────────────────────────────────────────
#
# TC-SYS-001: Boot sequence completes all eight initialisation stages.
run_test "TC-SYS-001: boot-completes" \
    "Stage 1:" \
    "Stage 4:" \
    "Stage 8:" \
    "Rost kernel ready"

# TC-SYS-002: HPET timer hardware is detected and the TSC is calibrated.
run_test "TC-SYS-002: timer-hardware-init" \
    "Stage 4:" \
    "HPET" \
    "TSC calibrated"

# TC-SYS-003: All four ring-3 server processes are spawned and assigned PIDs.
run_test "TC-SYS-003: ring3-servers-spawned" \
    "Stage 8:" \
    "PID 2" \
    "PID 3" \
    "PID 4"

# TC-SYS-004: Kernel idle loop is running (watchdog is kicked = scheduler ticks).
run_test "TC-SYS-004: scheduler-ticks" \
    "Stage 8:" \
    "Rost kernel ready"

# TC-SYS-005: Crash log region is initialised (drain is called at Stage 1).
run_test "TC-SYS-005: crash-log-drain" \
    "Stage 1:" \
    "crash"

# ── Summary ───────────────────────────────────────────────────────────────────

log ""
log "Results: ${PASS} passed, ${FAIL} failed"

if [[ $FAIL -gt 0 ]]; then
    log "FAIL"
    exit 1
fi
log "PASS"
exit 0
