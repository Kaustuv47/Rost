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
const MY_NAME: &[u8] = b"uart-drv\0\0\0\0\0\0\0\0";

/// Service name of the shell client we push keystrokes to.
const SHELL_NAME: &[u8] = b"rost-shell\0\0\0\0\0\0";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Register ourselves so the shell can find us by name.
    syscall::register(MY_NAME);

    // Wait for the shell to register itself.
    //
    // IMPORTANT: must BLOCK here, not yield.  SYS_YIELD keeps this process in
    // the Ready state at priority 64.  Because the shell runs at priority 128
    // (lower urgency — higher number), a yield loop would starve the shell:
    // the scheduler always picks the highest-priority ready process (lowest
    // number), so uart-drv would spin forever and the shell would never get
    // CPU time to call SYS_REGISTER.
    //
    // Blocking with recv_msg(timeout) transitions us to Blocked for 10 ticks
    // (~100 ms).  While Blocked we are invisible to the scheduler and the
    // shell runs freely.  On wake-up we retry the lookup.
    let shell_pid = loop {
        let pid = syscall::lookup(SHELL_NAME);
        if pid != u64::MAX {
            break pid;
        }
        let mut dummy = Msg::zeroed();
        syscall::recv_msg(10, &mut dummy); // block 100 ms → let shell register
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
        // Send byte as word0 (data[0]) so SYS_RECV in the shell returns it
        // directly — SYS_RECV returns data[0] of the message.
        let byte = syscall::uart_read();
        if byte != u64::MAX {
            syscall::send(shell_pid, byte, 0);
        }

        // Block for 1 tick (~10 ms) to pace UART polling.
        //
        // yield_cpu() with the new immediate-switch SYS_YIELD causes uart-drv
        // to re-read the UART register before QEMU has cleared the last byte,
        // producing duplicate keystroke delivery.  Blocking for 1 tick ensures
        // the hardware buffer is fully consumed before the next read.
        syscall::recv_msg(1, &mut msg);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    syscall::exit(1);
}
