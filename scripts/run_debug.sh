#!/usr/bin/env bash
# Debug run script: launches QEMU with a monitor socket, then injects
# "exec /bin/hello<enter>" via the QEMU monitor after the shell is ready.
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$SCRIPT_DIR/.."
MONITOR_SOCK=/tmp/qemu-monitor-$$.sock
SERIAL_LOG=/tmp/qemu-serial-$$.log

cleanup() {
    kill "$QEMU_PID" 2>/dev/null || true
    wait "$QEMU_PID" 2>/dev/null || true
    rm -f "$MONITOR_SOCK"
}
trap cleanup EXIT INT TERM

# Launch QEMU: serial → file (so we can grep it), monitor → unix socket
qemu-system-x86_64 \
  -machine q35 \
  -accel   hvf \
  -cpu     host \
  -m       512M \
  -drive   "if=pflash,format=raw,readonly=on,file=/opt/local/share/qemu/edk2-x86_64-code.fd" \
  -drive   "format=raw,file=fat:rw:$ROOT/build/" \
  -device  ib700,id=watchdog0 \
  -watchdog-action reset \
  -netdev  "user,id=net0" \
  -device  "virtio-net-pci,netdev=net0" \
  -nographic \
  -serial  "file:$SERIAL_LOG" \
  -monitor "unix:$MONITOR_SOCK,server,nowait" \
  > /dev/null 2>&1 &
QEMU_PID=$!

echo "[debug] QEMU PID=$QEMU_PID"
echo "[debug] Serial log: $SERIAL_LOG"

# Wait for shell prompt
echo "[debug] Waiting for shell prompt..."
for i in $(seq 1 60); do
    sleep 1
    if grep -q 'rost@local' "$SERIAL_LOG" 2>/dev/null; then
        echo "[debug] Shell detected at ${i}s"
        break
    fi
    if ! kill -0 "$QEMU_PID" 2>/dev/null; then
        echo "[debug] QEMU exited!"
        break
    fi
done

sleep 2

echo "[debug] Injecting 'exec /bin/hello' via QEMU monitor..."

# Python script to send sendkey commands via unix socket monitor
python3 - "$MONITOR_SOCK" << 'PYEOF'
import socket, time, sys

sock_path = sys.argv[1]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(sock_path)

# Read initial QEMU monitor banner
time.sleep(0.2)
try:
    s.setblocking(False)
    banner = s.recv(4096)
    s.setblocking(True)
except:
    pass

def sendkey(key):
    cmd = f"sendkey {key}\n"
    s.sendall(cmd.encode())
    time.sleep(0.06)
    try:
        s.setblocking(False)
        s.recv(256)
        s.setblocking(True)
    except:
        pass

# "exec /bin/hello\n"
chars = {
    'e': 'e', 'x': 'x', 'e': 'e', 'c': 'c',
}

cmd = "exec /bin/hello"
keymap = {
    ' ': 'spc',
    '/': 'slash',
    '-': 'minus',
    '.': 'dot',
}

for ch in cmd:
    key = keymap.get(ch, ch)
    sendkey(key)

sendkey('ret')
s.close()
print("[debug] Keys sent")
PYEOF

echo "[debug] Waiting 35s for output..."
sleep 35

echo ""
echo "=== SERIAL LOG (last 200 lines) ==="
tail -200 "$SERIAL_LOG" | cat
