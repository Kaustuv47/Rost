/// Output a single byte to the console via BIOS INT 0x10
fn put_byte(byte: u8) {
    unsafe {
        core::arch::asm!(
        "mov al, {}",
        "mov ah, 0x0E",     // TTY output
        "int 0x10",
        in(reg_byte) byte,
        options(nostack)
        );
    }
}

/// Output a single character to the console
pub fn put_char(c: char) {
    put_byte(c as u8);
}

/// Print a string to the console
pub fn print_str(s: &str) {
    for byte in s.bytes() {
        put_byte(byte);
    }
}

/// Print a hex value
pub fn print_hex(val: u64) {
    print_str("0x");
    let hex_chars = b"0123456789ABCDEF";
    for i in (0..16).rev() {
        let digit = ((val >> (i * 4)) & 0xF) as usize;
        put_byte(hex_chars[digit]);
    }
}
