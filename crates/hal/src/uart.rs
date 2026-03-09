const COM1: u16 = 0x3F8;

unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nostack));
}

unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    core::arch::asm!("in al, dx", in("dx") port, out("al") v, options(nostack));
    v
}

/// Initialize COM1 at 38400 baud, 8N1
pub fn init() {
    unsafe {
        outb(COM1 + 1, 0x00); // Disable interrupts
        outb(COM1 + 3, 0x80); // Enable DLAB
        outb(COM1 + 0, 0x03); // Baud divisor low  (38400)
        outb(COM1 + 1, 0x00); // Baud divisor high
        outb(COM1 + 3, 0x03); // 8 bits, no parity, 1 stop bit
        outb(COM1 + 2, 0xC7); // Enable + clear FIFO
        outb(COM1 + 4, 0x0B); // RTS/DSR set
    }
}

/// Transmit one byte; auto-appends CR after LF
pub fn put_byte(byte: u8) {
    unsafe {
        while inb(COM1 + 5) & 0x20 == 0 {}
        outb(COM1, byte);
        if byte == b'\n' {
            while inb(COM1 + 5) & 0x20 == 0 {}
            outb(COM1, b'\r');
        }
    }
}

/// Returns the next byte from COM1 if one is available (non-blocking)
pub fn read_byte() -> Option<u8> {
    unsafe {
        if inb(COM1 + 5) & 0x01 != 0 { Some(inb(COM1)) } else { None }
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

pub fn print_hex(val: u64) {
    print_str("0x");
    let hex_chars = b"0123456789ABCDEF";
    for i in (0..16).rev() {
        let digit = ((val >> (i * 4)) & 0xF) as usize;
        put_byte(hex_chars[digit]);
    }
}
