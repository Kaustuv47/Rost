#!/usr/bin/env bash
# Generate line / branch coverage for core-kernel unit tests.
#
# Requirements (install once):
#   cargo install cargo-llvm-cov
#   rustup component add llvm-tools-preview
#
# Usage:
#   scripts/coverage.sh              # summary to stdout
#   scripts/coverage.sh --html       # open interactive HTML report in browser
#   THRESHOLD=80 scripts/coverage.sh # exit 1 if line coverage < 80 %
#
# Why --target host?
#   core-kernel uses #![cfg_attr(not(test), no_std)], so in test mode it links
#   against std.  The UEFI target can produce test binaries but they cannot run
#   on the build host.  We therefore explicitly target the native host platform
#   so that cargo-llvm-cov can execute the tests and collect coverage data.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$SCRIPT_DIR/.."

# Detect native host triple so this script works on both x86_64 and ARM macOS.
HOST="$(rustc -vV 2>/dev/null | awk '/^host:/{print $2}')"
if [[ -z "$HOST" ]]; then
    echo "error: could not determine host triple from 'rustc -vV'" >&2
    exit 1
fi

cd "$ROOT"

# ── Pre-flight: verify cargo-llvm-cov is installed ───────────────────────────

if ! cargo llvm-cov --version &>/dev/null; then
    echo "error: cargo-llvm-cov not found." >&2
    echo "  Install: cargo install cargo-llvm-cov" >&2
    echo "  Component: rustup component add llvm-tools-preview" >&2
    exit 1
fi

# ── Collect coverage ─────────────────────────────────────────────────────────

LCOV_OUT="$ROOT/target/lcov.info"

echo "[coverage] Running core-kernel tests with LLVM instrumentation..."
cargo llvm-cov \
    --target "$HOST" \
    --package core-kernel \
    --lcov \
    --output-path "$LCOV_OUT"

# ── Summary ──────────────────────────────────────────────────────────────────

echo ""
echo "[coverage] Summary:"
cargo llvm-cov report \
    --target "$HOST" \
    --package core-kernel \
    --summary-only 2>&1 | tee /tmp/rost-cov-summary.txt

# ── Optional: HTML report ─────────────────────────────────────────────────────

if [[ "${1:-}" == "--html" ]]; then
    echo "[coverage] Opening HTML report..."
    cargo llvm-cov \
        --target "$HOST" \
        --package core-kernel \
        --open
fi

# ── Optional: threshold enforcement ──────────────────────────────────────────

if [[ -n "${THRESHOLD:-}" ]]; then
    # Extract the overall line coverage percentage.
    LINE_COV="$(grep -oE 'TOTAL.*[0-9]+\.[0-9]+%' /tmp/rost-cov-summary.txt \
                | grep -oE '[0-9]+\.[0-9]+' | tail -1 || echo 0)"
    if (( $(echo "$LINE_COV < $THRESHOLD" | bc -l) )); then
        echo ""
        echo "FAIL: line coverage ${LINE_COV}% is below required ${THRESHOLD}%"
        exit 1
    fi
    echo ""
    echo "OK: line coverage ${LINE_COV}% >= required ${THRESHOLD}%"
fi

echo "[coverage] LCOV data written to: $LCOV_OUT"
