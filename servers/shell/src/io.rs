//! Terminal I/O for the shell server.
//!
//! All reads and writes go through the UART driver server via IPC.
//! The uart-drv server (servers/uart-drv) owns COM1 and forwards
//! keystrokes to us as IPC messages.
//!
//! # Protocol with uart-drv
//!
//! **Write a byte**
//! ```text
//!   SYS_SEND(uart_drv_pid, OP_WRITE=0x01, byte_value)
//! ```
//!
//! **Read a byte** (non-blocking)
//! ```text
//!   SYS_RECV(timeout=0)
//!   → u64::MAX  : no byte available
//!   → byte value: LSB of returned word
//! ```
//!
//! **Read a byte** (blocking)
//! ```text
//!   SYS_RECV(timeout=u64::MAX)
//!   → byte value when uart-drv pushes a keystroke to our mailbox
//! ```

/// IPC opcode: write one byte to the terminal.
const OP_WRITE: u64 = 0x01;

/// Service name used to locate uart-drv at runtime.
const UART_DRV_NAME: &[u8] = b"uart-drv\0";

/// Lazily resolved PID of the uart-drv server.
///
/// Initialised on the first call to `uart_drv_pid()`, then cached.
static mut UART_DRV_PID: u64 = u64::MAX;

/// Returns the current PID of the uart-drv server, resolving it if needed.
fn uart_drv_pid() -> u64 {
    // Safety: single-threaded ring-3 process; no concurrent modification.
    unsafe {
        if UART_DRV_PID == u64::MAX {
            loop {
                let pid = crate::syscall::lookup(UART_DRV_NAME);
                if pid != u64::MAX {
                    UART_DRV_PID = pid;
                    break;
                }
                crate::syscall::yield_cpu();
            }
        }
        UART_DRV_PID
    }
}

/// Write one byte to the terminal via uart-drv.
pub fn put_byte(b: u8) {
    crate::syscall::send(uart_drv_pid(), OP_WRITE, b as u64);
}

/// Write a string slice to the terminal.
pub fn print_str(s: &str) {
    for b in s.bytes() {
        put_byte(b);
    }
}

/// Non-blocking read.
///
/// The uart-drv server pushes each received keystroke as an IPC message
/// to our mailbox.  We poll with `timeout=0` to check without blocking.
///
/// Returns `None` if no keystroke is pending.
pub fn read_byte() -> Option<u8> {
    let v = crate::syscall::recv(0); // timeout=0 → non-blocking
    if v == u64::MAX { None } else { Some(v as u8) }
}

/// Blocking read — yields the CPU until a byte arrives.
///
/// Uses `timeout=u64::MAX` to block indefinitely.  While we are blocked
/// the kernel marks us `Blocked` and does not schedule us, so we burn
/// no CPU time waiting for input.
pub fn read_byte_blocking() -> u8 {
    loop {
        let v = crate::syscall::recv(u64::MAX);
        if v != u64::MAX {
            return v as u8;
        }
        crate::syscall::yield_cpu();
    }
}
