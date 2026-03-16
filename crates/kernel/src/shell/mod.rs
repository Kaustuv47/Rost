//! Kernel Emergency Console — ring-0 fallback shell.
//!
//! This is intentionally minimal: no history, no line editing, no ANSI escape
//! parsing.  It exists so the developer can issue `halt` or inspect kernel
//! state when the ring-3 `rost-shell` server is not running.
//!
//! The full interactive shell lives in `servers/shell/` (ring-3 ELF binary).

mod commands;

use hal::uart as serial;
use arch_x86_64::cpu;
use commands::Action;

const PROMPT: &[u8] = b"\r\n[kec]# ";
const LINE_MAX: usize = 128;

pub fn run() -> ! {
    serial::print_str("\n\x1b[1;33mKernel Emergency Console\x1b[0m [ring-0]\n");
    serial::print_str("Type \x1b[1mhelp\x1b[0m for available commands.\n");
    emit_prompt();

    let mut buf  = [0u8; LINE_MAX];
    let mut pos  = 0usize;

    loop {
        // Block until a byte arrives from COM1.
        if let Some(b) = serial::read_byte() {
            match b {
                b'\r' | b'\n' => {
                    serial::put_byte(b'\n');
                    match commands::dispatch(&buf[..pos]) {
                        Action::Halt => {
                            serial::print_str("System halting...\n");
                            cpu::disable_interrupts();
                            loop { cpu::halt(); }
                        }
                        Action::Continue => {}
                    }
                    pos = 0;
                    emit_prompt();
                }
                // Backspace / DEL
                0x08 | 0x7f => {
                    if pos > 0 {
                        pos -= 1;
                        serial::print_str("\x08 \x08"); // erase char on terminal
                    }
                }
                // Printable ASCII
                0x20..=0x7e => {
                    if pos < LINE_MAX {
                        serial::put_byte(b); // echo
                        buf[pos] = b;
                        pos += 1;
                    }
                }
                _ => {}
            }
        } else {
            cpu::halt();
            arch_x86_64::cpu::tick_scheduler();
        }
    }
}

fn emit_prompt() {
    for &b in PROMPT { serial::put_byte(b); }
}
