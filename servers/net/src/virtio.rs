//! Virtio-net legacy driver (PCI vendor=0x1AF4 device=0x1000).
//!
//! Queue layout (QUEUE_SIZE=256, three 4096-byte "pages" per queue = 12288 bytes):
//!   [0,    4095]  Descriptor table: 256 × 16 bytes
//!   [4096, 4611]  Available ring:   flags(u16) + idx(u16) + ring[256](u16) = 516 bytes
//!   [8192, 10243] Used ring:        flags(u16) + idx(u16) + ring[256]({id:u32,len:u32}) = 2060 bytes
//!
//! VirtioNetHdr (10 bytes) prefixes every packet on both TX and RX.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use crate::pci::{find_pci_device, get_io_bar, pci_read8, pci_read_command, pci_write_command};
use crate::syscall::{ioport_in, ioport_out, phys_addr, print};

// ── Constants ─────────────────────────────────────────────────────────────────

const VIRTIO_NET_VENDOR:  u16 = 0x1AF4;
const VIRTIO_NET_DEVICE:  u16 = 0x1000;

const QUEUE_SIZE: usize = 256;

// Virtio I/O register offsets from BAR0 I/O base
const VIRTIO_REG_DEV_FEATURES:  u16 = 0x00; // u32 R
const VIRTIO_REG_DRV_FEATURES:  u16 = 0x04; // u32 W
const VIRTIO_REG_QUEUE_PFN:     u16 = 0x08; // u32 R/W
const VIRTIO_REG_QUEUE_SIZE:    u16 = 0x0C; // u16 R
const VIRTIO_REG_QUEUE_SEL:     u16 = 0x0E; // u16 W
const VIRTIO_REG_QUEUE_NOTIFY:  u16 = 0x10; // u16 W
const VIRTIO_REG_STATUS:        u16 = 0x12; // u8  R/W
const VIRTIO_REG_ISR:           u16 = 0x13; // u8  R (clears on read)
const VIRTIO_REG_NET_MAC:       u16 = 0x14; // 6 bytes R (device-specific config)

// Virtio device status bits
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER:      u8 = 2;
const STATUS_DRIVER_OK:   u8 = 4;

// Virtio descriptor flags
const VRING_DESC_F_NEXT:  u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2; // device-writable (used for RX descriptors)

// Queue indices
const QUEUE_RX: u16 = 0;
const QUEUE_TX: u16 = 1;

// Number of RX buffers to pre-post
const RX_BUF_COUNT: usize = 8;

// Maximum Ethernet frame size + virtio net header (10 bytes)
const PKT_BUF_SIZE: usize = 1526; // 1514 (max eth) + 10 (virtio hdr) + 2 (align)

// VirtioNetHdr size
const VIRTIO_NET_HDR_SIZE: usize = 10;

// ── Static memory for virtqueue rings and packet buffers ─────────────────────
// All must be page-aligned (4096 bytes) for the PFN calculation.

#[repr(C, align(4096))]
struct QueueMem([u8; 12288]);

#[repr(C, align(4096))]
struct PktBuf([u8; PKT_BUF_SIZE]);

#[repr(C, align(4096))]
struct NetHdrBuf([u8; VIRTIO_NET_HDR_SIZE]);

static mut RX_QUEUE_MEM: QueueMem = QueueMem([0u8; 12288]);
static mut TX_QUEUE_MEM: QueueMem = QueueMem([0u8; 12288]);

// RX packet buffers — 8 slots, each 1526 bytes, page-aligned
static mut RX_BUFS: [PktBuf; RX_BUF_COUNT] = [
    PktBuf([0u8; PKT_BUF_SIZE]),
    PktBuf([0u8; PKT_BUF_SIZE]),
    PktBuf([0u8; PKT_BUF_SIZE]),
    PktBuf([0u8; PKT_BUF_SIZE]),
    PktBuf([0u8; PKT_BUF_SIZE]),
    PktBuf([0u8; PKT_BUF_SIZE]),
    PktBuf([0u8; PKT_BUF_SIZE]),
    PktBuf([0u8; PKT_BUF_SIZE]),
];

// TX buffer and header buffer
static mut TX_BUF: PktBuf    = PktBuf([0u8; PKT_BUF_SIZE]);
static mut TX_HDR: NetHdrBuf = NetHdrBuf([0u8; VIRTIO_NET_HDR_SIZE]);

// ── Virtqueue layout accessors ────────────────────────────────────────────────
//
// Given a pointer to the 12288-byte QueueMem, the sub-structures are at:
//   desc[i]   at base + i*16                  (desc table, 256 × 16 = 4096 bytes)
//   avail     at base + 4096                  (avail ring)
//   used      at base + 8192                  (used ring, 4096-aligned)

/// Descriptor entry (16 bytes, host byte order in RAM)
#[repr(C)]
struct VirtqDesc {
    addr:  u64,
    len:   u32,
    flags: u16,
    next:  u16,
}

/// Available ring header
#[repr(C)]
struct VirtqAvailHdr {
    flags: u16,
    idx:   u16,
}

/// Used ring element
#[repr(C)]
struct VirtqUsedElem {
    id:  u32,
    len: u32,
}

/// Used ring header
#[repr(C)]
struct VirtqUsedHdr {
    flags: u16,
    idx:   u16,
}

// ── VirtioNet driver struct ───────────────────────────────────────────────────

pub struct VirtioNet {
    pub io_base:      u16,
    pub mac:          [u8; 6],
    /// PCI bus/device/function — stored so `irq_info()` can read PCI config 0x3C.
    pci_bus:          u8,
    pci_dev:          u8,
    pci_func:         u8,
    /// Next descriptor index to use for TX (wraps QUEUE_SIZE)
    tx_desc_idx:      u16,
    /// Next avail ring slot to use for TX
    tx_avail_idx:     u16,
    /// Last used ring index seen for TX
    tx_used_last:     u16,
    /// Next avail ring slot to use for RX re-fills
    rx_avail_idx:     u16,
    /// Last used ring index seen for RX
    rx_used_last:     u16,
}

// ── I/O helpers ───────────────────────────────────────────────────────────────

fn io_read8(base: u16, off: u16) -> u8 {
    ioport_in(base + off, 1) as u8
}

fn io_read16(base: u16, off: u16) -> u16 {
    ioport_in(base + off, 2) as u16
}

fn io_write8(base: u16, off: u16, val: u8) {
    ioport_out(base + off, val as u32, 1);
}

fn io_write16(base: u16, off: u16, val: u16) {
    ioport_out(base + off, val as u32, 2);
}

fn io_write32(base: u16, off: u16, val: u32) {
    ioport_out(base + off, val, 4);
}

// ── Queue setup helpers ───────────────────────────────────────────────────────

/// Get pointer to descriptor `i` in a queue memory block.
unsafe fn desc_ptr(queue_mem: *mut u8, i: usize) -> *mut VirtqDesc {
    queue_mem.add(i * 16) as *mut VirtqDesc
}

/// Get pointer to available ring header in a queue memory block.
unsafe fn avail_hdr_ptr(queue_mem: *mut u8) -> *mut VirtqAvailHdr {
    queue_mem.add(4096) as *mut VirtqAvailHdr
}

/// Get pointer to available ring slot `i`.
unsafe fn avail_ring_ptr(queue_mem: *mut u8, i: usize) -> *mut u16 {
    // avail ring: flags(u16) + idx(u16) + ring[256](u16)
    queue_mem.add(4096 + 4 + i * 2) as *mut u16
}

/// Get pointer to used ring header in a queue memory block.
unsafe fn used_hdr_ptr(queue_mem: *mut u8) -> *mut VirtqUsedHdr {
    queue_mem.add(8192) as *mut VirtqUsedHdr
}

/// Get pointer to used ring element `i`.
unsafe fn used_elem_ptr(queue_mem: *mut u8, i: usize) -> *mut VirtqUsedElem {
    // used ring: flags(u16) + idx(u16) + ring[256]({id:u32,len:u32})
    queue_mem.add(8192 + 4 + i * 8) as *mut VirtqUsedElem
}

// ── Virtio queue setup ────────────────────────────────────────────────────────

/// Setup a virtqueue: select it, read its size, and write its PFN.
/// Returns the queue size (should be QUEUE_SIZE).
unsafe fn setup_queue(io_base: u16, queue_idx: u16, queue_mem: *mut u8) -> u16 {
    // Select the queue
    io_write16(io_base, VIRTIO_REG_QUEUE_SEL, queue_idx);
    let qsz = io_read16(io_base, VIRTIO_REG_QUEUE_SIZE);
    if qsz == 0 { return 0; }

    // Compute physical address of queue memory, then PFN (>> 12)
    let virt = queue_mem as u64;
    let phys = phys_addr(virt).unwrap_or(virt); // fallback: assume identity map
    let pfn  = (phys >> 12) as u32;

    io_write32(io_base, VIRTIO_REG_QUEUE_PFN, pfn);
    qsz
}

// ── Public init ───────────────────────────────────────────────────────────────

pub fn init() -> Option<VirtioNet> {
    // 1. Find the PCI device
    let (bus, dev, func) = find_pci_device(VIRTIO_NET_VENDOR, VIRTIO_NET_DEVICE)?;

    print(b"[net] virtio-net PCI found\n");

    // 2. Get BAR0 (I/O port base)
    let io_base = get_io_bar(bus, dev, func, 0)?;

    // Enable PCI I/O space + bus mastering in command register
    let cmd = pci_read_command(bus, dev, func);
    pci_write_command(bus, dev, func, cmd | 0x05);

    // 3. Reset the device (write 0 to status)
    io_write8(io_base, VIRTIO_REG_STATUS, 0);

    // 4. ACKNOWLEDGE + DRIVER
    io_write8(io_base, VIRTIO_REG_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

    // 5. Read & negotiate features (we request none beyond the defaults)
    let _dev_features = ioport_in(io_base + VIRTIO_REG_DEV_FEATURES, 4);
    // Accept no optional features (legacy device ignores most flags)
    io_write32(io_base, VIRTIO_REG_DRV_FEATURES, 0);

    // 6. Read MAC address from device config at offset +0x14
    let mut mac = [0u8; 6];
    for i in 0..6 {
        mac[i] = io_read8(io_base, VIRTIO_REG_NET_MAC + i as u16);
    }

    // 7. Setup queues
    unsafe {
        let rx_mem = addr_of_mut!(RX_QUEUE_MEM) as *mut u8;
        let tx_mem = addr_of_mut!(TX_QUEUE_MEM) as *mut u8;

        setup_queue(io_base, QUEUE_RX, rx_mem);
        setup_queue(io_base, QUEUE_TX, tx_mem);

        // 8. Pre-populate RX descriptors
        // Each RX buffer gets one descriptor pointing to its data area.
        for i in 0..RX_BUF_COUNT {
            let buf_virt = addr_of_mut!(RX_BUFS[i]) as u64;
            let buf_phys = phys_addr(buf_virt).unwrap_or(buf_virt);

            let d = desc_ptr(rx_mem, i);
            write_volatile(&mut (*d).addr,  buf_phys);
            write_volatile(&mut (*d).len,   PKT_BUF_SIZE as u32);
            write_volatile(&mut (*d).flags, VRING_DESC_F_WRITE);
            write_volatile(&mut (*d).next,  0);

            // Put descriptor index into avail ring
            write_volatile(avail_ring_ptr(rx_mem, i), i as u16);
        }

        // Advance avail idx to 8 (all buffers posted)
        let avail = avail_hdr_ptr(rx_mem);
        write_volatile(&mut (*avail).flags, 0u16);
        write_volatile(&mut (*avail).idx,   RX_BUF_COUNT as u16);

        // Kick RX queue so device starts using the buffers
        io_write16(io_base, VIRTIO_REG_QUEUE_NOTIFY, QUEUE_RX);
    }

    // 9. DRIVER_OK
    io_write8(io_base, VIRTIO_REG_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK);

    print(b"[net] virtio-net ready, MAC: ");
    for (i, &b) in mac.iter().enumerate() {
        let hex = crate::HEX_DIGITS;
        crate::syscall::uart_write(hex[(b >> 4) as usize]);
        crate::syscall::uart_write(hex[(b & 0xF) as usize]);
        if i < 5 { crate::syscall::uart_write(b':'); }
    }
    crate::syscall::uart_write(b'\n');

    Some(VirtioNet {
        io_base,
        mac,
        pci_bus:      bus,
        pci_dev:      dev,
        pci_func:     func,
        tx_desc_idx:  0,
        tx_avail_idx: 0,
        tx_used_last: 0,
        rx_avail_idx: RX_BUF_COUNT as u16,
        rx_used_last: 0,
    })
}

// ── Send a packet ─────────────────────────────────────────────────────────────

impl VirtioNet {
    /// Send `data` as an Ethernet frame.
    /// Prepends a zero virtio net header and uses two chained descriptors.
    pub fn send_packet(&mut self, data: &[u8]) -> bool {
        if data.len() > PKT_BUF_SIZE - VIRTIO_NET_HDR_SIZE {
            return false;
        }

        unsafe {
            let tx_mem = addr_of_mut!(TX_QUEUE_MEM) as *mut u8;

            // Zero-fill the TX virtio net header
            let hdr_virt = addr_of_mut!(TX_HDR) as *mut u8;
            for i in 0..VIRTIO_NET_HDR_SIZE {
                write_volatile(hdr_virt.add(i), 0u8);
            }

            // Copy payload into TX_BUF
            let buf_ptr = addr_of_mut!(TX_BUF) as *mut u8;
            for (i, &b) in data.iter().enumerate() {
                write_volatile(buf_ptr.add(i), b);
            }

            let hdr_phys = phys_addr(hdr_virt as u64).unwrap_or(hdr_virt as u64);
            let buf_phys = phys_addr(buf_ptr  as u64).unwrap_or(buf_ptr  as u64);

            // Descriptor 0: virtio net header (10 bytes), chained to descriptor 1
            let d0_idx = (self.tx_desc_idx as usize) % QUEUE_SIZE;
            let d1_idx = (self.tx_desc_idx as usize + 1) % QUEUE_SIZE;

            let d0 = desc_ptr(tx_mem, d0_idx);
            write_volatile(&mut (*d0).addr,  hdr_phys);
            write_volatile(&mut (*d0).len,   VIRTIO_NET_HDR_SIZE as u32);
            write_volatile(&mut (*d0).flags, VRING_DESC_F_NEXT);
            write_volatile(&mut (*d0).next,  d1_idx as u16);

            // Descriptor 1: actual packet data
            let d1 = desc_ptr(tx_mem, d1_idx);
            write_volatile(&mut (*d1).addr,  buf_phys);
            write_volatile(&mut (*d1).len,   data.len() as u32);
            write_volatile(&mut (*d1).flags, 0u16);
            write_volatile(&mut (*d1).next,  0u16);

            // Put descriptor 0 head index into TX avail ring
            let avail_slot = (self.tx_avail_idx as usize) % QUEUE_SIZE;
            write_volatile(avail_ring_ptr(tx_mem, avail_slot), d0_idx as u16);

            // Advance avail idx
            self.tx_avail_idx = self.tx_avail_idx.wrapping_add(1);

            // Memory barrier before notifying device
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

            // Update avail ring idx
            let avail = avail_hdr_ptr(tx_mem);
            write_volatile(&mut (*avail).idx, self.tx_avail_idx);

            // Kick TX queue
            io_write16(self.io_base, VIRTIO_REG_QUEUE_NOTIFY, QUEUE_TX);

            // Advance our desc index by 2
            self.tx_desc_idx = self.tx_desc_idx.wrapping_add(2) % QUEUE_SIZE as u16;

            // Poll TX used ring until the device has consumed this buffer.
            // Spin for up to ~50k iterations (~5ms at typical speeds).
            let deadline_used = self.tx_used_last.wrapping_add(1);
            let mut spins = 0u32;
            loop {
                core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
                let used_idx = read_volatile(&(*used_hdr_ptr(tx_mem)).idx);
                if used_idx == deadline_used {
                    self.tx_used_last = deadline_used;
                    break;
                }
                spins += 1;
                if spins > 50_000 {
                    // Timed out — still mark as done to avoid deadlock
                    self.tx_used_last = deadline_used;
                    return false;
                }
                core::hint::spin_loop();
            }
        }
        true
    }

    // ── Receive a packet ──────────────────────────────────────────────────────

    /// Non-blocking receive. If a packet is available in the RX used ring,
    /// copies payload (skipping the 10-byte virtio net header) into `out`.
    /// Returns `Some(len)` where `len` is the Ethernet frame length.
    pub fn recv_packet_into(&mut self, out: &mut [u8]) -> Option<usize> {
        unsafe {
            let rx_mem = addr_of_mut!(RX_QUEUE_MEM) as *mut u8;

            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

            // Check if device has placed a buffer in the used ring
            let used_idx = read_volatile(&(*used_hdr_ptr(rx_mem)).idx);
            if used_idx == self.rx_used_last {
                return None; // nothing received yet
            }

            let slot = (self.rx_used_last as usize) % QUEUE_SIZE;
            let elem = used_elem_ptr(rx_mem, slot);
            let desc_id  = read_volatile(&(*elem).id)  as usize;
            let total_len = read_volatile(&(*elem).len) as usize;

            self.rx_used_last = self.rx_used_last.wrapping_add(1);

            // Data in RX_BUFS[desc_id] starts with 10-byte VirtioNetHdr
            if total_len <= VIRTIO_NET_HDR_SIZE {
                // Corrupt/empty — re-enqueue and skip
                self.reenqueue_rx(rx_mem, desc_id);
                return None;
            }

            let eth_len = total_len - VIRTIO_NET_HDR_SIZE;
            let copy_len = eth_len.min(out.len());

            // Copy eth frame data (after virtio header)
            let buf_ptr = addr_of!(RX_BUFS[desc_id]) as *const u8;
            for i in 0..copy_len {
                out[i] = read_volatile(buf_ptr.add(VIRTIO_NET_HDR_SIZE + i));
            }

            // Re-enqueue the descriptor for future RX
            self.reenqueue_rx(rx_mem, desc_id);

            Some(copy_len)
        }
    }

    /// Return `(irq_line, isr_port)` for use with `SYS_IRQ_REGISTER`.
    ///
    /// `irq_line` is the PCI Interrupt Line from config offset 0x3C (GSI number).
    /// `isr_port` is `io_base + VIRTIO_REG_ISR` (0x13): reading this byte de-asserts
    /// the virtio interrupt line and must be done in the kernel ISR before EOI.
    pub fn irq_info(&self) -> (u8, u16) {
        let irq_line = pci_read8(self.pci_bus, self.pci_dev, self.pci_func, 0x3C);
        let isr_port = self.io_base + VIRTIO_REG_ISR;
        (irq_line, isr_port)
    }

    /// Re-enqueue an RX descriptor into the available ring after it has been consumed.
    unsafe fn reenqueue_rx(&mut self, rx_mem: *mut u8, desc_id: usize) {
        let avail_slot = (self.rx_avail_idx as usize) % QUEUE_SIZE;
        write_volatile(avail_ring_ptr(rx_mem, avail_slot), desc_id as u16);
        self.rx_avail_idx = self.rx_avail_idx.wrapping_add(1);

        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        let avail = avail_hdr_ptr(rx_mem);
        write_volatile(&mut (*avail).idx, self.rx_avail_idx);

        // Kick RX queue
        io_write16(self.io_base, VIRTIO_REG_QUEUE_NOTIFY, QUEUE_RX);
    }
}
