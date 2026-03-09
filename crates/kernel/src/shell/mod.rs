mod commands;

use hal::uart as serial;
use arch_x86_64::cpu;

/// Interactive shell — reads from serial, never returns
pub fn run() -> ! {
    let mut cmd_buf = [0u8; 256];
    let mut cmd_len: usize = 0;

    serial::print_str("rost> ");

    loop {
        if let Some(byte) = serial::read_byte() {
            match byte {
                b'\r' | b'\n' => {
                    serial::put_byte(b'\n');
                    if cmd_len > 0 {
                        commands::dispatch(&cmd_buf[..cmd_len]);
                        cmd_len = 0;
                    }
                    serial::print_str("rost> ");
                }
                0x08 | 0x7F => {
                    if cmd_len > 0 {
                        cmd_len -= 1;
                        serial::put_byte(0x08);
                        serial::put_byte(b' ');
                        serial::put_byte(0x08);
                    }
                }
                b if b >= 0x20 && cmd_len < 255 => {
                    cmd_buf[cmd_len] = b;
                    cmd_len += 1;
                    serial::put_byte(b);
                }
                _ => {}
            }
        } else {
            cpu::halt();
        }
    }
}
