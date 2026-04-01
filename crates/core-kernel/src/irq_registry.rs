//! Hardware IRQ → userspace process registry.
//!
//! Maps Global System Interrupt (GSI) numbers to the ring-3 server PID that
//! should receive an IPC notification when the interrupt fires, plus the
//! optional ISR port to read (and clear) in the kernel ISR before notifying.
//!
//! Supports GSI 0–15 (ISA + PCI slave range).  The useful range for ring-3
//! PCI devices on QEMU Q35 is GSI 8–15 (slave 8259 / IOAPIC pins 8–15).
//!
//! # Virtio ISR register
//!
//! Virtio-legacy devices assert their interrupt line until the driver reads
//! the ISR Status register (BAR0 + 0x13).  The kernel ISR must perform this
//! read before notifying the ring-3 driver; otherwise the IOAPIC sees a
//! continuously-asserted interrupt and re-fires immediately after EOI.

use core::sync::atomic::{AtomicU16, AtomicU32, Ordering};

const MAX_GSI: usize = 16;

// --- const helpers so each array can be initialised without Copy/Clone ---

const PID_INIT:  AtomicU32 = AtomicU32::new(u32::MAX);
const PORT_INIT: AtomicU16 = AtomicU16::new(0);

/// PID that receives an IPC message when this GSI fires.  `u32::MAX` = none.
static IRQ_PID: [AtomicU32; MAX_GSI] = [
    PID_INIT, PID_INIT, PID_INIT, PID_INIT,
    PID_INIT, PID_INIT, PID_INIT, PID_INIT,
    PID_INIT, PID_INIT, PID_INIT, PID_INIT,
    PID_INIT, PID_INIT, PID_INIT, PID_INIT,
];

/// I/O port of the device ISR status register.  `0` = none (do not read).
///
/// For virtio-legacy: `BAR0 + 0x13` (VIRTIO_REG_ISR, read-to-clear).
static IRQ_ISR_PORT: [AtomicU16; MAX_GSI] = [
    PORT_INIT, PORT_INIT, PORT_INIT, PORT_INIT,
    PORT_INIT, PORT_INIT, PORT_INIT, PORT_INIT,
    PORT_INIT, PORT_INIT, PORT_INIT, PORT_INIT,
    PORT_INIT, PORT_INIT, PORT_INIT, PORT_INIT,
];

/// Register `pid` as the recipient for interrupts on `gsi`.
///
/// `isr_port`: I/O port the kernel ISR will `inb()` to de-assert the device
/// interrupt line (pass `0` if the device does not require a port clear).
///
/// Returns `true` on success, `false` if `gsi` is out of range.
pub fn register(gsi: u8, pid: u32, isr_port: u16) -> bool {
    let idx = gsi as usize;
    if idx >= MAX_GSI {
        return false;
    }
    // Write the ISR port first so it is visible before the PID.
    IRQ_ISR_PORT[idx].store(isr_port, Ordering::Relaxed);
    IRQ_PID[idx].store(pid, Ordering::Release);
    true
}

/// Return `(pid, isr_port)` for `gsi`, or `None` if no process is registered.
#[inline]
pub fn lookup(gsi: u8) -> Option<(u32, u16)> {
    let idx = gsi as usize;
    if idx >= MAX_GSI {
        return None;
    }
    let pid = IRQ_PID[idx].load(Ordering::Acquire);
    if pid == u32::MAX {
        return None;
    }
    let port = IRQ_ISR_PORT[idx].load(Ordering::Relaxed);
    Some((pid, port))
}
