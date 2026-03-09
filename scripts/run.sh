#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$SCRIPT_DIR/.."

qemu-system-x86_64 \
  -machine q35 \
  -accel hvf \
  -cpu host \
  -m 512M \
  -drive if=pflash,format=raw,readonly=on,file=/opt/local/share/qemu/edk2-x86_64-code.fd \
  -drive format=raw,file=fat:rw:"$ROOT/build/" \
  -net none \
  -serial stdio
