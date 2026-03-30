#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$SCRIPT_DIR/.."

QEMU_ARGS=(
  qemu-system-x86_64
  -machine q35
  -accel   hvf
  -cpu     host
  -m       512M
  -drive   "if=pflash,format=raw,readonly=on,file=/opt/local/share/qemu/edk2-x86_64-code.fd"
  -drive   "format=raw,file=fat:rw:$ROOT/build/"
  -device  ib700,id=watchdog0
  -watchdog-action reset
  -net     none
  -nographic
)

# Run QEMU inside a pty so it always sees a real TTY on stdin/stdout.
#
# Without a pty, QEMU's -nographic (mon:stdio) may leave the terminal in
# cooked mode causing local echo and line-buffered input, which produces the
# "double echo" symptom.  Wrapping in script(1) guarantees a pty is present,
# so QEMU's own tcsetattr call correctly sets raw mode.
#
# -q    : suppress "Script started / done" banners
# /dev/null : discard the typescript (we don't need a recording)
exec script -q /dev/null "${QEMU_ARGS[@]}"
