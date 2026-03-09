#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$SCRIPT_DIR/.."

cargo build --manifest-path "$ROOT/Cargo.toml" --target x86_64-unknown-uefi "$@"

mkdir -p "$ROOT/build/efi/boot"
cp "$ROOT/target/x86_64-unknown-uefi/debug/Rost.efi" "$ROOT/build/efi/boot/bootx64.efi"
echo "Deployed → build/efi/boot/bootx64.efi"
