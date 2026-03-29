//! UART driver server (rost-uart-drv).
//!
//! Owns COM1 on behalf of ring-3 processes.  The kernel exposes two
//! privileged syscalls so this driver can access the UART port:
//!
//!   SYS_UART_WRITE (12) — write one byte to COM1
//!   SYS_UART_READ  (13) — non-blocking read; returns byte or u64::MAX
//!
//! # IPC Protocol
//!
//! **Shell → uart-drv (write a byte):**
//! ```text
//!   SYS_SEND(uart_drv_pid, OP_WRITE=0x01, byte_value)
//! ```
//!
//! **uart-drv → shell (push a keystroke):**
//! ```text
//!   SYS_SEND(shell_pid, 0x00, byte_value)
//! ```
//!
//! The shell reads keystrokes via SYS_RECV from its own message queue.
//!
//! # Boot sequence
//! 1. kernel spawns uart-drv as PID 2 via ELF loader
//! 2. uart-drv calls SYS_REGISTER("uart-drv\0")
//! 3. kernel spawns shell; shell calls SYS_LOOKUP("uart-drv\0") → PID 2
//! 4. shell calls SYS_LOOKUP("rost-shell\0") to get its own PID for uart-drv's reverse path
//! 5. uart-drv polls SYS_RECV_MSG for write requests and SYS_UART_READ for keystrokes

#![no_std]
#![no_main]

mod syscall;

use syscall::Msg;

/// IPC opcode from shell: write one byte to UART TX.
const OP_WRITE:   u64 = 0x01;
/// IPC opcode from kernel ISR: one byte received from UART hardware.
const OP_UART_RX: u64 = 0x02;

/// Service name for registration (null-terminated, padded to 16 bytes).
const MY_NAME: &[u8] = b"uart-drv\0\0\0\0\0\0\0\0";

/// Service name of the shell client we push keystrokes to.
const SHELL_NAME: &[u8] = b"rost-shell\0\0\0\0\0\0";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Register ourselves so the shell and kernel can find us by name.
    syscall::register(MY_NAME);

    // Wait for the shell to register itself.
    //
    // IMPORTANT: must BLOCK here, not yield.  Blocking transitions us to
    // Blocked for 10 ticks, giving the shell CPU time to call SYS_REGISTER.
    let shell_pid = loop {
        let pid = syscall::lookup(SHELL_NAME);
        if pid != u64::MAX {
            break pid;
        }
        let mut dummy = Msg::zeroed();
        syscall::recv_msg(10, &mut dummy); // block ~100 ms → let shell register
    };

    let mut msg = Msg::zeroed();

    // Interrupt-driven main loop.
    //
    // We block indefinitely on recv_msg.  Two sources wake us:
    //   - Shell sends OP_WRITE(byte): forward to UART TX.
    //   - Kernel COM1 ISR sends OP_UART_RX(byte): forward to shell.
    //
    // There is no polling loop and no artificial 1-tick sleep.  Keystroke
    // latency is now bounded by ISR overhead (~µs) rather than polling
    // period (~10 ms).
    loop {
        if !syscall::recv_msg(u64::MAX, &mut msg) {
            continue; // spurious wakeup (shouldn't happen with u64::MAX)
        }

        match msg.data[0] {
            OP_WRITE => {
                // Shell wants to print a byte.
                syscall::uart_write(msg.data[1] as u8);
            }
            OP_UART_RX => {
                // Kernel ISR delivered a received byte — forward to shell.
                // SYS_SEND puts byte in data[0] so SYS_RECV in shell returns it.
                syscall::send(shell_pid, msg.data[1], 0);
            }
            _ => {} // ignore unknown opcodes
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    syscall::exit(1);
}
