//! rost-ps2-kbd — PS/2 keyboard driver for the Rost microkernel.
//!
//! Registers as "ps2-kbd" in the service registry.
//!
//! # Hardware
//!
//! PS/2 keyboard sits on ISA IRQ 1 (GSI 1).  The driver registers via
//! `SYS_IRQ_REGISTER(1, 0)` — the kernel routes IOAPIC GSI 1 → IDT vector 33
//! and delivers an IPC message `{data[0]=0xFFFF_0001}` on each interrupt.
//! No ISR port read is needed (PS/2 has no read-to-clear status register).
//!
//! On IRQ notification the driver drains all bytes from port 0x60 via
//! `SYS_IOPORT_IN` until the Output Buffer Full bit (bit 0 of port 0x64)
//! is clear.  This handles burst key events between interrupt deliveries.
//!
//! # IPC Protocol
//!
//! | Opcode              | Value  | Meaning                                |
//! |---------------------|--------|----------------------------------------|
//! | OP_KBD_REGISTER     | 0x80   | data[1]=PID — set foreground recipient |
//! | OP_KBD_UNREGISTER   | 0x81   | clear foreground recipient              |
//! | OP_KBD_GET_SCANCODE | 0x82   | non-blocking; RESP_KBD_SCANCODE or RESP_EMPTY |
//!
//! RESP_KBD_SCANCODE (0xB0): data[1]=scancode byte
//! RESP_EMPTY        (0xB1): no key available

#![no_std]
#![no_main]

mod syscall;

use core::ptr::addr_of_mut;
use syscall::{exit, getpid, ioport_in, irq_register, print, print_dec, recv_msg, register, send_msg, Msg};

// ── PS/2 I/O ports ────────────────────────────────────────────────────────────

const PS2_DATA:   u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const PS2_OBF:    u8  = 0x01; // Output Buffer Full — key data available

// ── IPC opcodes ───────────────────────────────────────────────────────────────

const OP_KBD_REGISTER:     u64 = 0x80;
const OP_KBD_UNREGISTER:   u64 = 0x81;
const OP_KBD_GET_SCANCODE: u64 = 0x82;

const RESP_KBD_SCANCODE:   u64 = 0xB0;
const RESP_EMPTY:          u64 = 0xB1;

// ── US-layout scan-code set 1 → ASCII (simple subset, no shift/ctrl) ─────────

static SCANCODE_TO_ASCII: [u8; 128] = {
    let mut t = [0u8; 128];
    // Row 1: digits
    t[0x02] = b'1'; t[0x03] = b'2'; t[0x04] = b'3'; t[0x05] = b'4';
    t[0x06] = b'5'; t[0x07] = b'6'; t[0x08] = b'7'; t[0x09] = b'8';
    t[0x0A] = b'9'; t[0x0B] = b'0'; t[0x0C] = b'-'; t[0x0D] = b'=';
    // Row 2: QWERTY
    t[0x10] = b'q'; t[0x11] = b'w'; t[0x12] = b'e'; t[0x13] = b'r';
    t[0x14] = b't'; t[0x15] = b'y'; t[0x16] = b'u'; t[0x17] = b'i';
    t[0x18] = b'o'; t[0x19] = b'p'; t[0x1A] = b'['; t[0x1B] = b']';
    // Row 3: ASDF
    t[0x1E] = b'a'; t[0x1F] = b's'; t[0x20] = b'd'; t[0x21] = b'f';
    t[0x22] = b'g'; t[0x23] = b'h'; t[0x24] = b'j'; t[0x25] = b'k';
    t[0x26] = b'l'; t[0x27] = b';'; t[0x28] = b'\''; t[0x2B] = b'\\';
    // Row 4: ZXCV
    t[0x2C] = b'z'; t[0x2D] = b'x'; t[0x2E] = b'c'; t[0x2F] = b'v';
    t[0x30] = b'b'; t[0x31] = b'n'; t[0x32] = b'm'; t[0x33] = b',';
    t[0x34] = b'.'; t[0x35] = b'/';
    // Control keys
    t[0x0E] = 0x08; // Backspace
    t[0x0F] = b'\t'; // Tab
    t[0x1C] = b'\n'; // Enter
    t[0x39] = b' '; // Space
    t
};

// ── PS/2 controller helpers ───────────────────────────────────────────────────

/// Drain any stale bytes from the PS/2 output buffer (bounded, init-time only).
fn ps2_flush() {
    for _ in 0..16 {
        let status = ioport_in(PS2_STATUS, 1) as u8;
        if status & PS2_OBF == 0 { break; }
        let _ = ioport_in(PS2_DATA, 1);
    }
}

/// Non-blocking read: returns Some(scancode) if OBF is set, else None.
/// Called after an IRQ notification to drain all pending bytes.
fn ps2_poll() -> Option<u8> {
    let status = ioport_in(PS2_STATUS, 1) as u8;
    if status & PS2_OBF != 0 {
        Some(ioport_in(PS2_DATA, 1) as u8)
    } else {
        None
    }
}

// ── Scan-code ring buffer (16 entries) ───────────────────────────────────────

const BUF_SIZE: usize = 16;
static mut SC_BUF:  [u8; BUF_SIZE] = [0u8; BUF_SIZE];
static mut SC_HEAD: usize = 0;
static mut SC_TAIL: usize = 0;

unsafe fn sc_push(sc: u8) {
    let next = (SC_TAIL + 1) % BUF_SIZE;
    if next != SC_HEAD {
        SC_BUF[SC_TAIL] = sc;
        SC_TAIL = next;
    }
}

unsafe fn sc_pop() -> Option<u8> {
    if SC_HEAD == SC_TAIL { return None; }
    let sc = SC_BUF[SC_HEAD];
    SC_HEAD = (SC_HEAD + 1) % BUF_SIZE;
    Some(sc)
}

// ── Scan-code processor ───────────────────────────────────────────────────────

fn process_scancode(sc: u8, extended: &mut bool, foreground_pid: u64) {
    if sc == 0xE0 {
        *extended = true;
        return;
    }
    let is_break  = sc & 0x80 != 0;
    let make_code = sc & 0x7F;

    if *extended {
        *extended = false;
        return; // extended key — ignore for now
    }
    if is_break { return; } // key release — ignore

    unsafe { sc_push(make_code); }

    if foreground_pid != 0 && (make_code as usize) < SCANCODE_TO_ASCII.len() {
        let ascii = SCANCODE_TO_ASCII[make_code as usize];
        if ascii != 0 {
            let mut fwd = Msg::zeroed();
            fwd.data[0] = 0x01; // OP_WRITE (byte forward, same as uart-drv)
            fwd.data[1] = ascii as u64;
            send_msg(foreground_pid, &fwd);
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _start() -> ! {
    print(b"[ps2-kbd] started, PID=");
    print_dec(getpid() as u64);
    print(b"\n");

    ps2_flush();

    if !register(b"ps2-kbd\0\0\0\0\0\0\0\0\0") {
        print(b"[ps2-kbd] ERROR: register failed\n");
        exit(1);
    }

    // Register for ISA IRQ 1 (PS/2 keyboard) via IOAPIC → IDT vector 33.
    // isr_port=0: PS/2 has no read-to-clear ISR register.
    if !irq_register(1, 0) {
        print(b"[ps2-kbd] ERROR: IRQ 1 registration failed\n");
        exit(1);
    }

    print(b"[ps2-kbd] registered, IRQ 1 active (interrupt-driven)\n");

    let mut foreground_pid: u64 = 0;
    let mut extended:       bool = false;

    static mut MSG_BUF: Msg = Msg { sender: 0, _pad: 0, data: [0; 8] };

    loop {
        let buf = unsafe { &mut *addr_of_mut!(MSG_BUF) };
        // Block indefinitely — wake on hardware IRQ notification or IPC message.
        if !recv_msg(u64::MAX, buf) { continue; }

        if (buf.data[0] >> 16) == 0xFFFF {
            // Hardware IRQ 1 fired — drain all bytes the controller has buffered.
            // ps2_poll() is non-blocking and called only after interrupt delivery,
            // so this is drain-on-interrupt, not polling.
            while let Some(sc) = ps2_poll() {
                process_scancode(sc, &mut extended, foreground_pid);
            }
        } else {
            let sender = buf.sender as u64;
            match buf.data[0] {
                OP_KBD_REGISTER => {
                    foreground_pid = buf.data[1];
                }

                OP_KBD_UNREGISTER => {
                    foreground_pid = 0;
                }

                OP_KBD_GET_SCANCODE => {
                    let mut reply = Msg::zeroed();
                    if let Some(sc) = unsafe { sc_pop() } {
                        reply.data[0] = RESP_KBD_SCANCODE;
                        reply.data[1] = sc as u64;
                    } else {
                        reply.data[0] = RESP_EMPTY;
                    }
                    send_msg(sender, &reply);
                }

                _ => {}
            }
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    print(b"[ps2-kbd] PANIC\n");
    exit(1);
}
