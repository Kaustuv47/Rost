# Chapter 19 — Device Drivers

## 19.1 Driver Architecture

Rost device drivers are ring-3 ELF processes that communicate with hardware
through two mechanisms:

1. **I/O port access** via `SYS_IOPORT_IN` (30) and `SYS_IOPORT_OUT` (29) —
   the kernel validates the port range and executes the I/O instruction on the
   process's behalf.
2. **Interrupt delivery** via `SYS_IRQ_REGISTER` (32) — the kernel routes an
   IOAPIC GSI (8–15) to the driver's IPC queue as a notification.

This model keeps hardware access ring-3 only.  No driver code runs in the kernel.
A buggy or crashed driver is terminated by the kernel's fault handler; init then
restarts it (Chapter 16).

## 19.2 UART Driver (`servers/uart-drv`, PID 3)

The UART driver is the most fundamental server — it owns COM1 (serial port) and
is the primary keyboard input path.

### 19.2.1 Hardware

COM1 uses I/O ports in the range `0x3F8–0x3FF`:

| Port | Register | Direction |
|------|----------|-----------|
| 0x3F8 | Data / Divisor Low | R/W |
| 0x3F9 | IER / Divisor High | R/W |
| 0x3FA | IIR (read) / FCR (write) | R/W |
| 0x3FB | LCR (line control) | R/W |
| 0x3FC | MCR (modem control) | R/W |
| 0x3FD | LSR (line status) | R |

UART TX is interrupt-driven via IRQ 4 (COM1 is routed to GSI 4, but
`SYS_IRQ_REGISTER` only handles GSIs 8–15; COM1 IRQ therefore falls back to
polling in the current implementation).

### 19.2.2 Keystroke Forwarding

The UART driver keeps a "foreground PID" — the process that receives keystroke
IPC messages.  Initially this is the shell (PID 5).  When the shell calls
`SYS_REGISTER("rost-shell")`, the UART driver performs a lookup and caches the
shell's PID.

Each received byte is forwarded as:

```
SYS_SEND(shell_pid, byte, 0)
```

The `SYS_SEND` opcode (3) delivers a two-word message to the target's IPC queue.
The shell reads it with `SYS_RECV` (4).

### 19.2.3 TX Ring Buffer

Outgoing bytes are queued in a 256-byte ring buffer.  The TX ISR drains the ring
one byte at a time.  If the ring is full, new bytes are dropped (the serial port
is considered a best-effort output path).

The ring buffer implementation:

```rust
struct TxRing {
    buf:  [u8; 256],
    head: usize,   // next byte to send
    tail: usize,   // next empty slot
}

impl TxRing {
    fn push(&mut self, b: u8) -> bool {
        let next_tail = (self.tail + 1) % 256;
        if next_tail == self.head { return false; } // full
        self.buf[self.tail] = b;
        self.tail = next_tail;
        true
    }
    fn pop(&mut self) -> Option<u8> {
        if self.head == self.tail { return None; } // empty
        let b = self.buf[self.head];
        self.head = (self.head + 1) % 256;
        Some(b)
    }
}
```

## 19.3 PCI Bus Server (`servers/pci-bus`, PID 7)

The PCI bus server enumerates the PCI configuration space and provides discovery
services to other drivers.

### 19.3.1 PCI Config Space Access

PCI configuration registers are accessed through the legacy I/O port mechanism:

```
Port 0xCF8 — CONFIG_ADDRESS (32-bit write):
  Bit 31: Enable bit (must be 1)
  Bits [23:16]: Bus number
  Bits [15:11]: Device number
  Bits [10:8]:  Function number
  Bits [7:2]:   Register offset (DWORD aligned)
  Bits [1:0]:   Always 0

Port 0xCFC — CONFIG_DATA (32-bit R/W):
  Reads/writes the selected register
```

```rust
fn pci_addr(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus  as u32) << 16)
        | ((dev  as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC)
}

fn pci_read32(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    syscall::ioport_out(0xCF8, pci_addr(bus, dev, func, offset), 4);
    syscall::ioport_in(0xCFC, 4)
}
```

### 19.3.2 Enumeration

At startup, the PCI server scans buses 0–7, devices 0–31, functions 0–7.  For
each slot, it reads Vendor ID and Device ID.  If both are non-0xFFFF (slot
populated), it records the Bus/Device/Function (BDF) and identifiers.

```rust
const BDF_WORD: fn(bus: u8, dev: u8, func: u8) -> u32 =
    |b, d, f| ((b as u32) << 16) | ((d as u32) << 8) | f as u32;
```

### 19.3.3 IPC Protocol

| Opcode | Value | Request | Reply |
|--------|-------|---------|-------|
| `OP_PCI_FIND` | 0x60 | data[1]=vendor, data[2]=device | `RESP_PCI_BDF(0x91)`: data[1]=BDF |
| `OP_PCI_READ` | 0x61 | data[1]=BDF, data[2]=offset, data[3]=width | `RESP_PCI_DATA(0x92)` |
| `OP_PCI_WRITE` | 0x62 | data[1]=BDF, data[2]=offset, data[3]=width, data[4]=value | `RESP_OK` |
| `OP_PCI_ENUMERATE` | 0x63 | — | `RESP_PCI_ENUM(0x93)`: data[1]=count, data[2..]=BDF list |

Example: the block driver discovers its virtio-blk device by:
```rust
// In block-drv: find virtio-blk device
let bdf = pci_find(0x1AF4, 0x1001)?;  // vendor=virtio, device=blk
let bar0 = pci_read(bdf, 0x10, 4);    // read BAR0 (I/O port base)
```

## 19.4 Block Driver (`servers/block-drv`, PID 8)

The block driver implements the virtio-blk protocol over a legacy PCI device.

### 19.4.1 Virtio-blk Legacy PCI

Virtio-blk (legacy) uses BAR0 as an I/O port window:

| Offset | Register | Description |
|--------|----------|-------------|
| +0x00 | HOST_FEATURES | Device feature bits |
| +0x04 | GUEST_FEATURES | Driver-selected features |
| +0x08 | QUEUE_PFN | Physical address of virtqueue / 4096 |
| +0x0C | QUEUE_NUM_MAX | Maximum queue size |
| +0x0E | QUEUE_NUM | Selected queue size |
| +0x12 | STATUS | Device status byte |
| +0x13 | ISR | Interrupt status (read-to-clear) |
| +0x14 | CAPACITY | Total sector count (2 × u32) |

### 19.4.2 Virtqueue Protocol

A virtqueue is a shared memory structure with three parts:

```
Descriptor Table (N × 16 bytes):
  addr:  u64  — physical address of buffer
  len:   u32  — buffer length
  flags: u16  — NEXT(1), WRITE(2)
  next:  u16  — index of next descriptor in chain

Available Ring:
  flags: u16
  idx:   u16  — producer writes here
  ring:  [u16; N]  — descriptor chain head indices

Used Ring:
  flags: u16
  idx:   u16  — device writes here
  ring:  [UsedElem; N]
    id:  u32  — descriptor chain head index
    len: u32  — bytes written by device
```

A read request uses a three-descriptor chain:
1. **Descriptor 0**: `BlkReqHeader` (type=0 READ, sector=LBA)
2. **Descriptor 1**: Data buffer (512 bytes, WRITE flag — device writes here)
3. **Descriptor 2**: Status byte (WRITE flag — device writes 0=OK, 2=ERR)

```rust
#[repr(C)]
struct BlkReqHeader {
    req_type: u32,    // 0 = read
    reserved: u32,
    sector:   u64,    // LBA
}
```

### 19.4.3 Sector Cache

The block driver maintains a single-slot cache:

```rust
static mut CACHE_LBA: u32 = u32::MAX;
static mut CACHE_BUF: [u8; 512] = [0u8; 512];
```

If a `OP_BLK_READ` request has the same LBA as the cached sector, the data is
served from the cache without hardware I/O.  The VFS reads sectors sequentially
(it walks FAT chains linearly), so the cache hit rate is high.

### 19.4.4 VFS Block Protocol

```
Request (VFS → block-drv):
  data[0] = 0x50 (OP_BLK_READ)
  data[1] = LBA  (sector number, 0-based)
  data[2] = byte_offset within sector (0, 40, 80, …, 480)

Reply (block-drv → VFS):
  data[0] = 0x90 (RESP_BLK_DATA)
  data[1] = 512  (sector size, always 512)
  data[2] = chunk_len (typically 40, last chunk may be ≤ 40)
  data[3..7] = raw bytes (40 bytes packed little-endian)
```

The VFS reads a 512-byte sector by issuing 13 consecutive requests
(13 × 40 = 520 ≥ 512):
- Requests 0–12: byte_offset = 0, 40, 80, …, 480
- Request 12: chunk_len = 32 (512 - 480 = 32 bytes)

### 19.4.5 IRQ-Driven I/O

The block driver registers for virtio-blk's GSI using `SYS_IRQ_REGISTER`.
When the device completes a request, it asserts the IRQ, the kernel delivers a
notification to the block driver's IPC queue, and the driver reads the result
from the used ring.

```rust
syscall::irq_register(gsi);  // claim GSI

// After submitting a request:
loop {
    let notif = recv_msg(timeout, &mut msg)?;  // block until IRQ notification
    if used_ring.idx != last_used { break; }   // device has completed
}
```

## 19.5 PS/2 Keyboard Driver (`servers/ps2-kbd`, PID 10)

The PS/2 keyboard driver is a supplementary input source.  The primary input
path remains uart-drv (COM1 serial).

### 19.5.1 Hardware

PS/2 uses two I/O ports:

| Port | Register |
|------|----------|
| 0x60 | Data register (scan code byte) |
| 0x64 | Status register (bit 0 = Output Buffer Full) |

```rust
fn poll_scancode() -> Option<u8> {
    let status = syscall::ioport_in(0x64, 1) as u8;
    if status & 0x01 != 0 {
        Some(syscall::ioport_in(0x60, 1) as u8)
    } else {
        None
    }
}
```

### 19.5.2 Scan Code Set 1

The driver translates scan code set 1 (standard PC keyboard) to ASCII:

- Press codes 0x01–0x58 map to ASCII using a lookup table
- Release codes (0x80+) are ignored
- Modifier state (Shift, Ctrl, Alt) is tracked via a bit field
- Extended codes (0xE0 prefix) handle arrow keys, Insert, Delete, etc.

### 19.5.3 Foreground Forwarding

Like uart-drv, the PS/2 driver maintains a foreground PID and sends ASCII bytes
with `SYS_SEND(foreground_pid, byte, 0)`.

```
OP_KBD_REGISTER   (0x80) — data[1]=PID: set foreground process
OP_KBD_UNREGISTER (0x81) — clear foreground process
OP_KBD_GET_SCANCODE (0x82) — non-blocking scancode poll
```

### 19.5.4 Ring Buffer

A 16-byte scan-code ring buffers keystrokes between polls and delivers.  If the
ring is full (application not reading), oldest codes are dropped.

## 19.6 Driver Lifecycle

All drivers follow the same lifecycle:

```
1. _start() → SYS_REGISTER("service-name")
2. Hardware initialization (PCI scan, BAR read, virtqueue setup)
3. SYS_NOTIFY(1, READY_SIGNAL) — signal init
4. Main dispatch loop:
   recv_msg(timeout, &mut msg)
   match msg.data[0] { opcodes... }
```

If a driver crashes (hardware fault, bus error, internal bug):
- The kernel's fault handler fires
- The process is terminated
- A fault notification is sent to init (PID 1)
- Init calls `SYS_RESTART_SERVER("service-name")`
- The kernel re-spawns the driver from the embedded ELF

This hot-restart capability means a transient driver bug (e.g. one caused by
unusual hardware state) can be recovered without rebooting the system.

## 19.7 Summary

| Driver | PID | Hardware | Protocol | IRQ |
|--------|-----|----------|----------|-----|
| uart-drv | 3 | COM1 (0x3F8) | Keystroke forwarding | IRQ 4 (polling) |
| pci-bus | 7 | 0xCF8/0xCFC | PCI config scan | none |
| block-drv | 8 | virtio-blk (legacy PCI) | Block read (40 B chunks) | GSI 8–15 |
| ps2-kbd | 10 | 0x60/0x64 | Keystroke forwarding | polling |

All drivers run in ring-3, access hardware only via SYS_IOPORT_IN/OUT, and
are restartable by init after a crash.
