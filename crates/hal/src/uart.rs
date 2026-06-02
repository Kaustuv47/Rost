use core::sync::atomic::{AtomicU8, Ordering};

const COM1: u16 = 0x3F8;

// COM1 register offsets
const OFF_THR: u16 = 0; // Transmit Holding Register (write)
const OFF_IER: u16 = 1; // Interrupt Enable Register
const OFF_IIR: u16 = 2; // Interrupt Identification Register (read-only)
const OFF_LSR: u16 = 5; // Line Status Register

// LSR bits
const LSR_DR:   u8 = 0x01; // Data Ready (RX byte available)
const LSR_THRE: u8 = 0x20; // Transmit Holding Register Empty

// IER bits
const IER_ERBFI: u8 = 0x01; // Enable Received-Data interrupt
const IER_ETBEI: u8 = 0x02; // Enable Transmitter Holding Register Empty interrupt

// IIR identification codes (bits[3:1] when bit[0]=0 = interrupt pending)
pub const IIR_THRE: u8 = 0x02; // TX holding register empty
pub const IIR_RDA:  u8 = 0x04; // RX data available
pub const IIR_CTI:  u8 = 0x0C; // RX character timeout (FIFO mode)
pub const IIR_NONE: u8 = 0x01; // No interrupt pending (bit 0 set)

unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nostack));
}

unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    core::arch::asm!("in al, dx", in("dx") port, out("al") v, options(nostack));
    v
}

// ── Interrupt-driven TX ring buffer ──────────────────────────────────────────
//
// SPSC (single-producer single-consumer): `put_byte` is the sole producer and
// `tx_isr` is the sole consumer.  On a single-core system this is always safe
// because `tx_isr` can only execute between instructions, never concurrently.
//
// u8 indices wrap naturally at 256, which equals TX_BUF_SIZE.  A full ring is
// detected as `next_head == tail` leaving one slot unused (255 usable bytes).

const TX_BUF_SIZE: usize = 256;
static mut TX_BUF: [u8; TX_BUF_SIZE] = [0u8; TX_BUF_SIZE];
/// Write index — advanced by `put_byte`.
static TX_HEAD: AtomicU8 = AtomicU8::new(0);
/// Read  index — advanced by `tx_isr`.
static TX_TAIL: AtomicU8 = AtomicU8::new(0);

/// Initialize COM1 at 115200 baud, 8N1, with RX interrupt enabled.
/// ETBEI (TX interrupt) is NOT enabled here; `put_byte` arms it on demand.
pub fn init() {
    unsafe {
        outb(COM1 + OFF_IER, IER_ERBFI); // Enable RX-ready interrupt only
        outb(COM1 + 3, 0x80);            // Enable DLAB
        outb(COM1 + 0, 0x01);            // Baud divisor low  (115200)
        outb(COM1 + 1, 0x00);            // Baud divisor high
        outb(COM1 + 3, 0x03);            // 8 bits, no parity, 1 stop bit
        outb(COM1 + 2, 0xC7);            // Enable + clear FIFO
        outb(COM1 + 4, 0x0B);            // RTS/DSR set
    }
}

/// Write one byte directly to COM1, spinning until THRE is set.
///
/// Bypasses the TX ring buffer entirely.  Use for crash-path and
/// diagnostic output where interrupts may be disabled or the ring
/// is already full.  Does NOT arm ETBEI — no side effects on the
/// interrupt-driven path.
pub fn put_byte_direct(byte: u8) {
    unsafe {
        while inb(COM1 + OFF_LSR) & LSR_THRE == 0 {}
        outb(COM1 + OFF_THR, byte);
    }
}

/// Same as `put_byte_direct` but for a string.
pub fn print_str_direct(s: &str) {
    for b in s.bytes() { put_byte_direct(b); }
}

/// Same as `print_hex` but direct-poll (no ring buffer).
pub fn print_hex_direct(val: u64) {
    print_str_direct("0x");
    let hex_chars = b"0123456789ABCDEF";
    for i in (0..16).rev() {
        let digit = ((val >> (i * 4)) & 0xF) as usize;
        put_byte_direct(hex_chars[digit]);
    }
}

/// Enqueue `byte` for transmission via the interrupt-driven TX path.
///
/// The function **never spins**.  It pushes the byte into the ring buffer and
/// "primes the pump": if THRE is set (UART idle) the byte is written directly
/// to COM1, starting the THRE interrupt chain.  Subsequent bytes are drained
/// by `tx_isr`.
///
/// Safe from ISR context: when called with interrupts disabled, the THRE
/// interrupt is armed in IER but cannot fire until the outer ISR returns.
/// The ring buffer preserves queued bytes until that point.  Bytes are
/// silently dropped only if the 255-byte ring overflows.
pub fn put_byte(byte: u8) {
    let head = TX_HEAD.load(Ordering::Relaxed);
    let next_head = head.wrapping_add(1);

    // Drop byte rather than spinning if ring is full.
    if next_head == TX_TAIL.load(Ordering::Acquire) {
        return;
    }

    // Publish byte to ring.
    unsafe { TX_BUF[head as usize] = byte; }
    TX_HEAD.store(next_head, Ordering::Release);

    unsafe {
        if inb(COM1 + OFF_LSR) & LSR_THRE != 0 {
            // UART idle — pop the byte we just pushed and write it directly.
            // This starts the interrupt chain even in ISR context (THRE IRQ
            // is armed; fires as soon as outer ISR re-enables interrupts).
            let tail = TX_TAIL.load(Ordering::Acquire);
            let cur_head = TX_HEAD.load(Ordering::Relaxed);
            if tail != cur_head {
                let b = TX_BUF[tail as usize];
                TX_TAIL.store(tail.wrapping_add(1), Ordering::Release);
                outb(COM1 + OFF_THR, b);
            }
        }
        // Arm ETBEI so tx_isr fires when THRE next asserts.
        let ier = inb(COM1 + OFF_IER);
        if ier & IER_ETBEI == 0 {
            outb(COM1 + OFF_IER, ier | IER_ETBEI);
        }
    }
}

/// Called from the UART ISR when IIR indicates THRE.
///
/// Writes the next queued byte from the ring to COM1.  Disables ETBEI when
/// the ring empties so no further spurious THRE interrupts are generated.
pub fn tx_isr() {
    let tail = TX_TAIL.load(Ordering::Acquire);
    if tail == TX_HEAD.load(Ordering::Relaxed) {
        // Ring empty — disarm THRE interrupt until next put_byte call.
        unsafe {
            let ier = inb(COM1 + OFF_IER);
            outb(COM1 + OFF_IER, ier & !IER_ETBEI);
        }
        return;
    }
    let b = unsafe { TX_BUF[tail as usize] };
    TX_TAIL.store(tail.wrapping_add(1), Ordering::Release);
    unsafe { outb(COM1 + OFF_THR, b); }
}

/// Read the Interrupt Identification Register.
///
/// Bit 0 = 1 → no interrupt pending.  Otherwise bits[3:1] identify the
/// pending interrupt type; compare against `IIR_THRE`, `IIR_RDA`, `IIR_CTI`.
#[inline]
pub fn read_iir() -> u8 {
    unsafe { inb(COM1 + OFF_IIR) }
}

/// Returns the next byte from COM1 if one is available (non-blocking).
pub fn read_byte() -> Option<u8> {
    unsafe {
        if inb(COM1 + OFF_LSR) & LSR_DR != 0 { Some(inb(COM1)) } else { None }
    }
}

pub fn put_char(c: char) {
    put_byte(c as u8);
}

pub fn print_str(s: &str) {
    for byte in s.bytes() {
        put_byte(byte);
    }
}

pub fn print_dec(val: u64) {
    if val == 0 { put_byte(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut pos = 20usize;
    let mut n = val;
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    for &b in &buf[pos..] { put_byte(b); }
}

pub fn print_hex(val: u64) {
    print_str("0x");
    let hex_chars = b"0123456789ABCDEF";
    for i in (0..16).rev() {
        let digit = ((val >> (i * 4)) & 0xF) as usize;
        put_byte(hex_chars[digit]);
    }
}

/// Drain all bytes currently in the RX FIFO, calling `cb` for each byte.
///
/// Bounded to 16 iterations (16550A FIFO depth).  Safe to call in ISR context.
pub fn drain_rx_fifo(mut cb: impl FnMut(u8)) {
    unsafe {
        for _ in 0..16 {
            if inb(COM1 + OFF_LSR) & LSR_DR == 0 { break; }
            cb(inb(COM1));
        }
    }
}
