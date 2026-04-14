//! rost-pci-bus — PCI bus enumeration server for the Rost microkernel.
//!
//! Registers as "pci-bus" in the service registry.
//!
//! # IPC Protocol
//!
//! | Opcode          | Value  | Request                            | Reply             |
//! |-----------------|--------|------------------------------------|-------------------|
//! | OP_PCI_FIND     | 0x60   | data[1]=vendor, data[2]=device     | RESP_PCI_BDF or RESP_ERROR |
//! | OP_PCI_READ     | 0x61   | data[1]=bdf, data[2]=offset, data[3]=width | RESP_PCI_DATA |
//! | OP_PCI_WRITE    | 0x62   | data[1]=bdf, data[2]=offset, data[3]=width, data[4]=val | RESP_OK |
//! | OP_PCI_ENUMERATE| 0x63   | —                                  | RESP_PCI_ENUM     |
//!
//! BDF encoding: bus[23:16] | dev[12:8] | func[2:0]
//!
//! RESP_PCI_BDF  (0x91): data[1]=bdf
//! RESP_PCI_DATA (0x92): data[1]=value
//! RESP_PCI_ENUM (0x93): data[1]=count, data[2..] = BDF list (up to 6 entries)
//! RESP_OK       (0x85): success
//! RESP_ERROR    (0x8F): data[1]=errno (1=not found, 2=invalid)

#![no_std]
#![no_main]

mod syscall;

use core::ptr::addr_of_mut;
use syscall::{exit, getpid, print, print_dec, recv_msg, register, send_msg, Msg};

// ── PCI I/O port constants ────────────────────────────────────────────────────

const PCI_ADDR_PORT: u16 = 0xCF8;
const PCI_DATA_PORT: u16 = 0xCFC;

// ── IPC opcodes / responses ───────────────────────────────────────────────────

const OP_PCI_FIND:      u64 = 0x60;
const OP_PCI_READ:      u64 = 0x61;
const OP_PCI_WRITE:     u64 = 0x62;
const OP_PCI_ENUMERATE: u64 = 0x63;

const RESP_PCI_BDF:     u64 = 0x91;
const RESP_PCI_DATA:    u64 = 0x92;
const RESP_PCI_ENUM:    u64 = 0x93;
const RESP_OK:          u64 = 0x85;
const RESP_ERROR:       u64 = 0x8F;

// ── PCI config-space helpers ──────────────────────────────────────────────────

fn pci_addr(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus  as u32) << 16)
        | ((dev  as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC)
}

fn pci_read32(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    syscall::ioport_out(PCI_ADDR_PORT, pci_addr(bus, dev, func, offset), 4);
    syscall::ioport_in(PCI_DATA_PORT, 4)
}

fn pci_read16(bus: u8, dev: u8, func: u8, offset: u8) -> u16 {
    let dw = pci_read32(bus, dev, func, offset & !2);
    (dw >> ((offset & 2) * 8)) as u16
}

fn pci_read8(bus: u8, dev: u8, func: u8, offset: u8) -> u8 {
    let dw = pci_read32(bus, dev, func, offset & !3);
    (dw >> ((offset & 3) * 8)) as u8
}

fn pci_write32(bus: u8, dev: u8, func: u8, offset: u8, val: u32) {
    syscall::ioport_out(PCI_ADDR_PORT, pci_addr(bus, dev, func, offset), 4);
    syscall::ioport_out(PCI_DATA_PORT, val, 4);
}

/// Encode (bus, dev, func) into a compact BDF word.
fn bdf(bus: u8, dev: u8, func: u8) -> u64 {
    ((bus as u64) << 16) | ((dev as u64) << 8) | (func as u64)
}

fn bdf_decode(v: u64) -> (u8, u8, u8) {
    ((v >> 16) as u8, ((v >> 8) & 0x1F) as u8, (v & 0x7) as u8)
}

// ── Bus scan ──────────────────────────────────────────────────────────────────

/// Scan the PCI bus for a device matching (vendor, device).
/// Returns Some(bdf_word) on first match, else None.
fn find_device(vendor: u16, device: u16) -> Option<u64> {
    for bus in 0u8..=255 {
        for dev in 0u8..32 {
            let id = pci_read32(bus, dev, 0, 0x00);
            if id == 0xFFFF_FFFF { continue; }
            let v = (id & 0xFFFF) as u16;
            let d = (id >> 16) as u16;
            if v == vendor && d == device { return Some(bdf(bus, dev, 0)); }

            let hdr = pci_read8(bus, dev, 0, 0x0E);
            if hdr & 0x80 != 0 {
                for func in 1u8..8 {
                    let fid = pci_read32(bus, dev, func, 0x00);
                    if fid == 0xFFFF_FFFF { continue; }
                    let fv = (fid & 0xFFFF) as u16;
                    let fd = (fid >> 16) as u16;
                    if fv == vendor && fd == device { return Some(bdf(bus, dev, func)); }
                }
            }
        }
    }
    None
}

/// Collect all present PCI devices (up to `out.len()` entries).
/// Returns the number written.
fn enumerate(out: &mut [u64]) -> usize {
    let mut count = 0usize;
    'outer: for bus in 0u8..=255 {
        for dev in 0u8..32 {
            for func in 0u8..8 {
                if func > 0 {
                    // Only check additional functions for multi-function devices.
                    let hdr = pci_read8(bus, dev, 0, 0x0E);
                    if hdr & 0x80 == 0 { break; }
                }
                let id = pci_read32(bus, dev, func, 0x00);
                if id == 0xFFFF_FFFF { continue; }
                if count >= out.len() { break 'outer; }
                out[count] = bdf(bus, dev, func);
                count += 1;
            }
        }
    }
    count
}

// ── Message handler ───────────────────────────────────────────────────────────

fn handle(msg: &mut Msg) {
    let sender = msg.sender as u64;
    match msg.data[0] {
        OP_PCI_FIND => {
            let vendor = msg.data[1] as u16;
            let device = msg.data[2] as u16;
            let mut reply = Msg::zeroed();
            if let Some(b) = find_device(vendor, device) {
                reply.data[0] = RESP_PCI_BDF;
                reply.data[1] = b;
            } else {
                reply.data[0] = RESP_ERROR;
                reply.data[1] = 1; // not found
            }
            send_msg(sender, &reply);
        }

        OP_PCI_READ => {
            let b      = msg.data[1];
            let offset = msg.data[2] as u8;
            let width  = msg.data[3] as u8;
            let (bus, dev, func) = bdf_decode(b);
            let val = match width {
                1 => pci_read8 (bus, dev, func, offset) as u32,
                2 => pci_read16(bus, dev, func, offset) as u32,
                _ => pci_read32(bus, dev, func, offset),
            };
            let mut reply = Msg::zeroed();
            reply.data[0] = RESP_PCI_DATA;
            reply.data[1] = val as u64;
            send_msg(sender, &reply);
        }

        OP_PCI_WRITE => {
            let b      = msg.data[1];
            let offset = msg.data[2] as u8;
            let width  = msg.data[3] as u8;
            let val    = msg.data[4] as u32;
            let (bus, dev, func) = bdf_decode(b);
            match width {
                1 => {
                    let dw = pci_read32(bus, dev, func, offset & !3);
                    let shift = (offset & 3) * 8;
                    let new_dw = (dw & !(0xFF << shift)) | (((val & 0xFF) as u32) << shift);
                    pci_write32(bus, dev, func, offset & !3, new_dw);
                }
                2 => {
                    let dw = pci_read32(bus, dev, func, offset & !2);
                    let shift = (offset & 2) * 8;
                    let new_dw = (dw & !(0xFFFF << shift)) | (((val & 0xFFFF) as u32) << shift);
                    pci_write32(bus, dev, func, offset & !2, new_dw);
                }
                _ => pci_write32(bus, dev, func, offset, val),
            }
            let mut reply = Msg::zeroed();
            reply.data[0] = RESP_OK;
            send_msg(sender, &reply);
        }

        OP_PCI_ENUMERATE => {
            // Pack up to 6 BDF words into data[2..8].
            let mut bdfs = [0u64; 6];
            let count = enumerate(&mut bdfs);
            let mut reply = Msg::zeroed();
            reply.data[0] = RESP_PCI_ENUM;
            reply.data[1] = count as u64;
            let n = count.min(6);
            reply.data[2..2 + n].copy_from_slice(&bdfs[..n]);
            send_msg(sender, &reply);
        }

        _ => {
            // Unknown opcode — reply with error.
            let mut reply = Msg::zeroed();
            reply.data[0] = RESP_ERROR;
            reply.data[1] = 2; // invalid opcode
            send_msg(sender, &reply);
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _start() -> ! {
    print(b"[pci-bus] started, PID=");
    print_dec(getpid() as u64);
    print(b"\n");

    if !register(b"pci-bus\0\0\0\0\0\0\0\0\0") {
        print(b"[pci-bus] ERROR: register failed\n");
        exit(1);
    }

    print(b"[pci-bus] registered as 'pci-bus'; scanning bus...\n");

    // Quick sanity scan: count present devices on bus 0.
    let mut bus0_count = 0u32;
    for dev in 0u8..32 {
        let id = pci_read32(0, dev, 0, 0x00);
        if id != 0xFFFF_FFFF { bus0_count += 1; }
    }
    print(b"[pci-bus] bus 0 devices: ");
    print_dec(bus0_count as u64);
    print(b"\n");

    // ── Main loop ─────────────────────────────────────────────────────────────
    static mut MSG_BUF: Msg = Msg { sender: 0, _pad: 0, data: [0; 8] };
    loop {
        let buf = unsafe { &mut *addr_of_mut!(MSG_BUF) };
        if recv_msg(u64::MAX, buf) {
            handle(buf);
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    print(b"[pci-bus] PANIC\n");
    exit(1);
}
