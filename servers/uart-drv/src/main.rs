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

/// IPC opcode from shell: write one byte.
const OP_WRITE: u64 = 0x01;

/// Service name for registration (null-terminated, padded to 16 bytes).
const MY_NAME: &[u8] = b"uart-drv\0";

/// Service name of the shell client we push keystrokes to.
const SHELL_NAME: &[u8] = b"rost-shell\0";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Register ourselves so the shell can find us by name.
    syscall::register(MY_NAME);

    // Wait for the shell to register itself.
    let shell_pid = loop {
        let pid = syscall::lookup(SHELL_NAME);
        if pid != u64::MAX {
            break pid;
        }
        syscall::yield_cpu();
    };

    let mut msg = Msg::zeroed();

    loop {
        // Drain IPC queue: forward write requests from shell to UART.
        while syscall::recv_msg(0, &mut msg) {
            if msg.data[0] == OP_WRITE {
                syscall::uart_write(msg.data[1] as u8);
            }
        }

        // Poll UART RX: forward any received bytes to the shell.
        let byte = syscall::uart_read();
        if byte != u64::MAX {
            syscall::send(shell_pid, 0, byte);
        }

        syscall::yield_cpu();
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    syscall::exit(1);
}
