//! PCI bus enumeration via I/O ports 0xCF8 (address) and 0xCFC (data).
//!
//! PCI config address format:
//!   bit 31    = enable bit (always 1)
//!   bits 23:16 = bus number
//!   bits 15:11 = device number
//!   bits 10:8  = function number
//!   bits 7:2   = register offset (dword-aligned)
//!   bits 1:0   = 0 (ignored on write)

use crate::syscall::{ioport_in, ioport_out};

const PCI_ADDR_PORT: u16 = 0xCF8;
const PCI_DATA_PORT: u16 = 0xCFC;

/// Build a PCI config address register value.
#[inline]
fn pci_addr(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus  as u32) << 16)
        | ((dev  as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC)
}

/// Read a 32-bit dword from PCI configuration space.
pub fn pci_read32(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    ioport_out(PCI_ADDR_PORT, pci_addr(bus, dev, func, offset), 4);
    ioport_in(PCI_DATA_PORT, 4)
}

/// Read a 16-bit word from PCI configuration space.
pub fn pci_read16(bus: u8, dev: u8, func: u8, offset: u8) -> u16 {
    let dword = pci_read32(bus, dev, func, offset & !2);
    let shift = (offset & 2) * 8;
    (dword >> shift) as u16
}

/// Read an 8-bit byte from PCI configuration space.
pub fn pci_read8(bus: u8, dev: u8, func: u8, offset: u8) -> u8 {
    let dword = pci_read32(bus, dev, func, offset & !3);
    let shift = (offset & 3) * 8;
    (dword >> shift) as u8
}

/// Scan the PCI bus for a device matching vendor/device IDs.
/// Returns `Some((bus, dev, func))` on first match.
pub fn find_pci_device(vendor: u16, device: u16) -> Option<(u8, u8, u8)> {
    for bus in 0u8..=255 {
        for dev in 0u8..32 {
            // Check function 0 first to see if the device exists
            let id = pci_read32(bus, dev, 0, 0x00);
            if id == 0xFFFF_FFFF {
                // No device here; skip remaining functions
                continue;
            }
            let v = (id & 0xFFFF) as u16;
            let d = (id >> 16) as u16;
            if v == vendor && d == device {
                return Some((bus, dev, 0));
            }

            // Check if multi-function device (header type bit 7)
            let hdr = pci_read8(bus, dev, 0, 0x0E);
            if hdr & 0x80 != 0 {
                for func in 1u8..8 {
                    let fid = pci_read32(bus, dev, func, 0x00);
                    if fid == 0xFFFF_FFFF { continue; }
                    let fv = (fid & 0xFFFF) as u16;
                    let fd = (fid >> 16) as u16;
                    if fv == vendor && fd == device {
                        return Some((bus, dev, func));
                    }
                }
            }
        }
    }
    None
}

/// Read a BAR and, if it is an I/O space BAR (bit 0 = 1), return the
/// 16-bit I/O base address (bits 15:2, with bits 1:0 masked off).
///
/// `bar_idx` is 0–5 corresponding to BAR0–BAR5 at offsets 0x10–0x24.
pub fn get_io_bar(bus: u8, dev: u8, func: u8, bar_idx: u8) -> Option<u16> {
    if bar_idx > 5 { return None; }
    let offset = 0x10 + (bar_idx as u8) * 4;
    let bar = pci_read32(bus, dev, func, offset);
    // Bit 0 = 1 → I/O space BAR
    if bar & 0x1 == 1 {
        Some((bar & 0xFFFC) as u16)
    } else {
        None
    }
}

/// Read the 16-bit command register (offset 0x04) for a PCI device.
pub fn pci_read_command(bus: u8, dev: u8, func: u8) -> u16 {
    pci_read16(bus, dev, func, 0x04)
}

/// Write the 16-bit command register (offset 0x04) for a PCI device.
pub fn pci_write_command(bus: u8, dev: u8, func: u8, cmd: u16) {
    // Read full dword first, then merge our word back in
    let dword = pci_read32(bus, dev, func, 0x04);
    let new_dword = (dword & 0xFFFF_0000) | (cmd as u32);
    ioport_out(PCI_ADDR_PORT, pci_addr(bus, dev, func, 0x04), 4);
    ioport_out(PCI_DATA_PORT, new_dword, 4);
}
