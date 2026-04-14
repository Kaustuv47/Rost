//! rost-block-drv — virtio-blk block device driver for the Rost microkernel.
//!
//! Registers as "block-drv" in the service registry.
//! Provides read-only sector access to the VFS via the standard block protocol.
//!
//! # Virtio-blk (legacy PCI, vendor=0x1AF4, device=0x1001)
//!
//! I/O register layout from BAR0 (same as virtio-net, block-specific config at +0x14):
//!   +0x12  STATUS    (u8  R/W)
//!   +0x13  ISR       (u8  R — clears on read, de-asserts interrupt)
//!   +0x14  CAPACITY  (u64 R — total sectors, little-endian)
//!
//! # VFS Block Protocol
//!
//! Request  (VFS → block-drv, SYS_CALL):
//!   data[0] = 0x50 (OP_BLK_READ)
//!   data[1] = LBA  (sector number)
//!   data[2] = byte_offset within sector (0, 40, 80, …, 480)
//!
//! Reply (block-drv → VFS):
//!   data[0] = 0x90 (RESP_BLK_DATA)
//!   data[1] = 512  (sector size)
//!   data[2] = chunk_len (1–40)
//!   data[3..8] = raw bytes (little-endian packed, up to 40 bytes)
//!
//!   On error:
//!   data[0] = 0x8F (RESP_ERROR)
//!   data[1] = errno

#![no_std]
#![no_main]

mod syscall;

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};
use syscall::{exit, getpid, irq_register, phys_addr, print, print_dec, recv_msg, register, Msg};

// ── Virtio-blk PCI identifiers ────────────────────────────────────────────────

const VIRTIO_BLK_VENDOR: u16 = 0x1AF4;
const VIRTIO_BLK_DEVICE: u16 = 0x1001;

// ── PCI config-space helpers ──────────────────────────────────────────────────

const PCI_ADDR_PORT: u16 = 0xCF8;
const PCI_DATA_PORT: u16 = 0xCFC;

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

fn pci_write_command(bus: u8, dev: u8, func: u8, cmd: u16) {
    let dw  = pci_read32(bus, dev, func, 0x04);
    let new = (dw & 0xFFFF_0000) | (cmd as u32);
    syscall::ioport_out(PCI_ADDR_PORT, pci_addr(bus, dev, func, 0x04), 4);
    syscall::ioport_out(PCI_DATA_PORT, new, 4);
}

/// Scan bus 0-255 for (vendor, device).  Returns Some((bus, dev, func)).
fn find_pci_device(vendor: u16, device: u16) -> Option<(u8, u8, u8)> {
    for bus in 0u8..=255 {
        for dev in 0u8..32 {
            let id = pci_read32(bus, dev, 0, 0x00);
            if id == 0xFFFF_FFFF { continue; }
            if (id & 0xFFFF) as u16 == vendor && (id >> 16) as u16 == device {
                return Some((bus, dev, 0));
            }
            let hdr = pci_read8(bus, dev, 0, 0x0E);
            if hdr & 0x80 != 0 {
                for func in 1u8..8 {
                    let fid = pci_read32(bus, dev, func, 0x00);
                    if fid == 0xFFFF_FFFF { continue; }
                    if (fid & 0xFFFF) as u16 == vendor && (fid >> 16) as u16 == device {
                        return Some((bus, dev, func));
                    }
                }
            }
        }
    }
    None
}

fn get_io_bar(bus: u8, dev: u8, func: u8, bar_idx: u8) -> Option<u16> {
    if bar_idx > 5 { return None; }
    let offset = 0x10 + bar_idx * 4;
    let bar = pci_read32(bus, dev, func, offset);
    if bar & 0x1 == 1 { Some((bar & 0xFFFC) as u16) } else { None }
}

// ── Virtio I/O register offsets from BAR0 ────────────────────────────────────

const VIRTIO_REG_DEV_FEATURES: u16 = 0x00;
const VIRTIO_REG_DRV_FEATURES: u16 = 0x04;
const VIRTIO_REG_QUEUE_PFN:    u16 = 0x08;
const VIRTIO_REG_QUEUE_SIZE:   u16 = 0x0C;
const VIRTIO_REG_QUEUE_SEL:    u16 = 0x0E;
const VIRTIO_REG_QUEUE_NOTIFY: u16 = 0x10;
const VIRTIO_REG_STATUS:       u16 = 0x12;
const VIRTIO_REG_ISR:          u16 = 0x13;
// Block-specific config (at BAR0 + 0x14):
const VIRTIO_REG_BLK_CAPACITY: u16 = 0x14; // u64 — total sectors

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER:      u8 = 2;
const STATUS_DRIVER_OK:   u8 = 4;

// ── Virtqueue layout (QUEUE_SIZE = 256, single request queue) ─────────────────
// One "page" = 4096 bytes.
//   [0,    4095]  Descriptor table: 256 × 16 bytes
//   [4096, 4611]  Available ring:   flags(u16) + idx(u16) + ring[256](u16) = 516 bytes
//   [8192, ...]   Used ring:        flags(u16) + idx(u16) + ring[256]({id:u32,len:u32})

const QUEUE_SIZE: usize = 256;

#[repr(C, align(4096))]
struct QueueMem([u8; 12288]);

static mut QUEUE_MEM: QueueMem = QueueMem([0u8; 12288]);

// virtio-blk request header (18 bytes) — device-readable.
#[repr(C)]
struct VirtBlkReq {
    req_type: u32, // 0 = read
    reserved: u32,
    sector:   u64,
}

// virtio-blk status byte — device-writable; placed at end of request chain.
#[repr(C, align(1))]
struct VirtBlkStatus(u8);

// Static buffers for the in-flight request (one at a time).
static mut BLK_REQ:    VirtBlkReq    = VirtBlkReq { req_type: 0, reserved: 0, sector: 0 };
static mut BLK_STATUS: VirtBlkStatus = VirtBlkStatus(0xFF);
// Sector data buffer — device-writable (512 bytes).
#[repr(C, align(512))]
struct SectorBuf([u8; 512]);
static mut SECTOR_BUF: SectorBuf = SectorBuf([0u8; 512]);

// ── Queue accessors ───────────────────────────────────────────────────────────

#[repr(C)]
struct VirtqDesc { addr: u64, len: u32, flags: u16, next: u16 }

#[repr(C)]
struct VirtqAvailHdr { flags: u16, idx: u16 }

#[repr(C)]
struct VirtqUsedElem { id: u32, len: u32 }

#[repr(C)]
struct VirtqUsedHdr { flags: u16, idx: u16 }

unsafe fn desc_ptr(i: usize) -> *mut VirtqDesc {
    let base = addr_of_mut!(QUEUE_MEM) as *mut u8;
    base.add(i * 16) as *mut VirtqDesc
}

unsafe fn avail_hdr() -> *mut VirtqAvailHdr {
    let base = addr_of_mut!(QUEUE_MEM) as *mut u8;
    base.add(4096) as *mut VirtqAvailHdr
}

unsafe fn avail_ring(i: usize) -> *mut u16 {
    let base = addr_of_mut!(QUEUE_MEM) as *mut u8;
    base.add(4096 + 4 + i * 2) as *mut u16
}

unsafe fn used_hdr() -> *mut VirtqUsedHdr {
    let base = addr_of_mut!(QUEUE_MEM) as *mut u8;
    base.add(8192) as *mut VirtqUsedHdr
}

unsafe fn used_elem(i: usize) -> *mut VirtqUsedElem {
    let base = addr_of_mut!(QUEUE_MEM) as *mut u8;
    base.add(8192 + 4 + i * 8) as *mut VirtqUsedElem
}

// ── I/O helpers ───────────────────────────────────────────────────────────────

fn io_read8(base: u16, off: u16) -> u8 {
    syscall::ioport_in(base + off, 1) as u8
}

fn io_write8(base: u16, off: u16, val: u8) {
    syscall::ioport_out(base + off, val as u32, 1);
}

fn io_write16(base: u16, off: u16, val: u16) {
    syscall::ioport_out(base + off, val as u32, 2);
}

fn io_write32(base: u16, off: u16, val: u32) {
    syscall::ioport_out(base + off, val, 4);
}

fn io_read16(base: u16, off: u16) -> u16 {
    syscall::ioport_in(base + off, 2) as u16
}

// ── Block device state ────────────────────────────────────────────────────────

struct BlkDev {
    io_base:      u16,
    capacity:     u64,  // total sectors
    avail_idx:    u16,  // next avail ring slot
    used_last:    u16,  // last used ring idx consumed
    pci_bus:      u8,
    pci_dev:      u8,
    pci_func:     u8,
}

impl BlkDev {
    /// Submit a read request for `sector` into SECTOR_BUF.
    /// Returns `true` if submission succeeded.
    unsafe fn submit_read(&mut self, sector: u64) -> bool {
        // ── Fill BLK_REQ ──────────────────────────────────────────────────────
        let req_ptr = addr_of_mut!(BLK_REQ);
        write_volatile(&mut (*req_ptr).req_type, 0); // VIRTIO_BLK_T_IN = 0
        write_volatile(&mut (*req_ptr).reserved, 0);
        write_volatile(&mut (*req_ptr).sector,   sector);

        let status_ptr = addr_of_mut!(BLK_STATUS);
        write_volatile(&mut (*status_ptr).0, 0xFF); // reset to "pending"

        // ── Compute physical addresses ────────────────────────────────────────
        let req_phys    = phys_addr(req_ptr    as u64).unwrap_or(req_ptr    as u64);
        let buf_phys    = phys_addr(addr_of_mut!(SECTOR_BUF) as u64)
                            .unwrap_or(addr_of_mut!(SECTOR_BUF) as u64);
        let status_phys = phys_addr(status_ptr as u64).unwrap_or(status_ptr as u64);

        // ── Build 3-descriptor chain ──────────────────────────────────────────
        // Desc 0: header (device-readable)
        let d0 = desc_ptr(0);
        write_volatile(&mut (*d0).addr,  req_phys);
        write_volatile(&mut (*d0).len,   18u32);  // sizeof VirtBlkReq
        write_volatile(&mut (*d0).flags, 1u16);   // VRING_DESC_F_NEXT
        write_volatile(&mut (*d0).next,  1u16);

        // Desc 1: data buffer (device-writable)
        let d1 = desc_ptr(1);
        write_volatile(&mut (*d1).addr,  buf_phys);
        write_volatile(&mut (*d1).len,   512u32);
        write_volatile(&mut (*d1).flags, 1 | 2);  // NEXT | WRITE
        write_volatile(&mut (*d1).next,  2u16);

        // Desc 2: status byte (device-writable)
        let d2 = desc_ptr(2);
        write_volatile(&mut (*d2).addr,  status_phys);
        write_volatile(&mut (*d2).len,   1u32);
        write_volatile(&mut (*d2).flags, 2u16);   // WRITE only
        write_volatile(&mut (*d2).next,  0u16);

        // ── Post to avail ring ────────────────────────────────────────────────
        let slot = (self.avail_idx as usize) % QUEUE_SIZE;
        write_volatile(avail_ring(slot), 0u16); // head = desc 0

        self.avail_idx = self.avail_idx.wrapping_add(1);
        fence(Ordering::SeqCst);

        write_volatile(&mut (*avail_hdr()).idx, self.avail_idx);
        fence(Ordering::SeqCst);

        // Kick queue 0
        io_write16(self.io_base, VIRTIO_REG_QUEUE_NOTIFY, 0);
        true
    }

    /// Wait for the device to complete the current request.
    /// Polls `used_hdr().idx` until it advances, then advances `used_last`.
    unsafe fn wait_complete(&mut self) -> bool {
        // Busy-wait with yield for up to ~500 ticks (5 seconds).
        // In an interrupt-driven path the IRQ notification wakes us; we
        // arrive here after recv_msg returns with an IRQ message.  For
        // simplicity, also poll a fixed number of iterations.
        for _ in 0..500_000usize {
            fence(Ordering::SeqCst);
            let used_idx = read_volatile(&(*used_hdr()).idx);
            if used_idx != self.used_last {
                self.used_last = used_idx;
                let status = read_volatile(&(*addr_of!(BLK_STATUS)).0);
                return status == 0; // 0 = VIRTIO_BLK_S_OK
            }
            // cooperative yield
            unsafe { core::arch::asm!("syscall",
                in("rax") 0u64,
                lateout("rax") _, lateout("rcx") _, lateout("r11") _,
                options(nostack)); }
        }
        false // timeout
    }
}

// ── IPC protocol constants (from VFS blk.rs) ─────────────────────────────────

const OP_BLK_READ:   u64 = 0x50;
const RESP_BLK_DATA: u64 = 0x90;
const RESP_ERROR:    u64 = 0x8F;

// ── Sector cache (single-slot) ────────────────────────────────────────────────
//
// Most filesystem reads are sequential; a 1-sector cache eliminates re-reading
// the same sector for each 40-byte chunk (the VFS makes 13 calls per sector).

static mut CACHE_LBA:   u32  = u32::MAX;
static mut CACHE_BUF:   [u8; 512] = [0u8; 512];
static mut CACHE_VALID: bool = false;

// ── Message handler ───────────────────────────────────────────────────────────

/// Pack `n` bytes from `src[offset..offset+n]` into up to 5 × u64 words.
fn pack_bytes(src: &[u8], offset: usize, n: usize) -> [u64; 5] {
    let mut words = [0u64; 5];
    let bytes: &mut [u8; 40] = unsafe {
        &mut *(&mut words as *mut [u64; 5] as *mut [u8; 40])
    };
    let end = (offset + n).min(src.len()).min(offset + 40);
    let count = end - offset;
    bytes[..count].copy_from_slice(&src[offset..offset + count]);
    words
}

unsafe fn handle_read(dev: &mut BlkDev, msg: &mut Msg) {
    let lba         = msg.data[1] as u32;
    let byte_offset = msg.data[2] as usize;
    let sender      = msg.sender as u64;

    if byte_offset >= 512 {
        let mut r = Msg::zeroed();
        r.data[0] = RESP_ERROR;
        r.data[1] = 1;
        syscall::send_msg(sender, &r);
        return;
    }

    // Cache check
    let cache_hit = {
        let cv = &*addr_of!(CACHE_VALID);
        let cl = &*addr_of!(CACHE_LBA);
        *cv && *cl == lba
    };

    if !cache_hit {
        // Evict and fetch new sector.
        if !dev.submit_read(lba as u64) || !dev.wait_complete() {
            let mut r = Msg::zeroed();
            r.data[0] = RESP_ERROR;
            r.data[1] = 5; // EIO
            syscall::send_msg(sender, &r);
            return;
        }
        // Copy SECTOR_BUF → CACHE_BUF
        let sb = &*addr_of!(SECTOR_BUF);
        let cb = &mut *addr_of_mut!(CACHE_BUF);
        cb.copy_from_slice(&sb.0);
        *addr_of_mut!(CACHE_LBA) = lba;
        *addr_of_mut!(CACHE_VALID) = true;
    }

    // Serve chunk from cache.
    let cb = &*addr_of!(CACHE_BUF);
    let remaining = 512 - byte_offset;
    let chunk_len = remaining.min(40);
    let words = pack_bytes(cb, byte_offset, chunk_len);

    let mut r = Msg::zeroed();
    r.data[0] = RESP_BLK_DATA;
    r.data[1] = 512;
    r.data[2] = chunk_len as u64;
    r.data[3..8].copy_from_slice(&words);
    syscall::send_msg(sender, &r);
}

// ── Device init ───────────────────────────────────────────────────────────────

unsafe fn init_device() -> Option<BlkDev> {
    let (bus, dev, func) = find_pci_device(VIRTIO_BLK_VENDOR, VIRTIO_BLK_DEVICE)?;
    let io_base = get_io_bar(bus, dev, func, 0)?;

    print(b"[block-drv] virtio-blk PCI found, BAR0=0x");
    syscall::print_hex(io_base as u64);
    print(b"\n");

    // Enable I/O + bus mastering
    let cmd = pci_read16(bus, dev, func, 0x04);
    pci_write_command(bus, dev, func, cmd | 0x05);

    // 1. Reset
    io_write8(io_base, VIRTIO_REG_STATUS, 0);
    // 2. Acknowledge + Driver
    io_write8(io_base, VIRTIO_REG_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
    // 3. Read + accept no optional features
    let _feats = syscall::ioport_in(io_base + VIRTIO_REG_DEV_FEATURES, 4);
    io_write32(io_base, VIRTIO_REG_DRV_FEATURES, 0);

    // 4. Read capacity (sectors)
    let cap_lo = syscall::ioport_in(io_base + VIRTIO_REG_BLK_CAPACITY,     4) as u64;
    let cap_hi = syscall::ioport_in(io_base + VIRTIO_REG_BLK_CAPACITY + 4, 4) as u64;
    let capacity = cap_lo | (cap_hi << 32);

    // 5. Setup request queue (queue 0)
    io_write16(io_base, VIRTIO_REG_QUEUE_SEL, 0);
    let _qsz = io_read16(io_base, VIRTIO_REG_QUEUE_SIZE);

    let virt = addr_of_mut!(QUEUE_MEM) as u64;
    let phys = phys_addr(virt).unwrap_or(virt);
    io_write32(io_base, VIRTIO_REG_QUEUE_PFN, (phys >> 12) as u32);

    // 6. DRIVER_OK
    io_write8(io_base, VIRTIO_REG_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK);

    // 7. Register IRQ (PCI interrupt line, typically GSI 11 on QEMU q35).
    let irq_line = pci_read8(bus, dev, func, 0x3C);
    let isr_port = io_base + VIRTIO_REG_ISR;
    if irq_line >= 8 && irq_line <= 15 {
        if irq_register(irq_line, isr_port) {
            print(b"[block-drv] IRQ registered, GSI=");
            print_dec(irq_line as u64);
            print(b"\n");
        } else {
            print(b"[block-drv] IRQ register failed (continuing with polling)\n");
        }
    } else {
        print(b"[block-drv] IRQ out of range (GSI=");
        print_dec(irq_line as u64);
        print(b"), polling fallback\n");
    }

    print(b"[block-drv] ready, capacity=");
    print_dec(capacity);
    print(b" sectors\n");

    Some(BlkDev {
        io_base,
        capacity,
        avail_idx: 0,
        used_last:  0,
        pci_bus:   bus,
        pci_dev:   dev,
        pci_func:  func,
    })
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _start() -> ! {
    print(b"[block-drv] started, PID=");
    print_dec(getpid() as u64);
    print(b"\n");

    let mut dev = match unsafe { init_device() } {
        Some(d) => d,
        None => {
            print(b"[block-drv] no virtio-blk device found, exiting\n");
            exit(0);
        }
    };

    if !register(b"block-drv\0\0\0\0\0\0\0") {
        print(b"[block-drv] ERROR: register failed\n");
        exit(1);
    }

    print(b"[block-drv] registered as 'block-drv'\n");

    static mut MSG_BUF: Msg = Msg { sender: 0, _pad: 0, data: [0; 8] };
    loop {
        let buf = unsafe { &mut *addr_of_mut!(MSG_BUF) };
        if recv_msg(u64::MAX, buf) {
            match buf.data[0] {
                OP_BLK_READ => unsafe { handle_read(&mut dev, buf); },

                // IRQ notification (data[0] == 0xFFFF_0000 | gsi)
                v if v >> 16 == 0xFFFF => {
                    // ACK: read ISR port (already done by kernel before delivering IPC).
                    // Reclaim any completed descriptors; cache stays valid.
                    fence(Ordering::SeqCst);
                    // No explicit action needed — wait_complete will see used_idx advance.
                }

                _ => {} // ignore unknown
            }
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    print(b"[block-drv] PANIC\n");
    exit(1);
}
