//! Terminal I/O for the shell server.
//!
//! All reads and writes go through the UART driver server via IPC.
//! The uart-drv server (servers/uart-drv, PID 2 by convention) owns COM1
//! and forwards keystrokes to us as IPC messages.
//!
//! # Protocol with uart-drv
//!
//! **Write a byte**
//! ```text
//!   SYS_SEND(UART_DRV_PID, word0=OP_WRITE, word1=byte_value)
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
//!
//! Once the service registry (init's name→PID map) is implemented,
//! `UART_DRV_PID` would be resolved by name lookup rather than a constant.

/// Well-known PID for the UART driver server.
/// PID 1 = init, PID 2 = uart-drv (by boot-time registration convention).
const UART_DRV_PID: u64 = 2;

/// IPC opcode: write one byte to the terminal.
const OP_WRITE: u64 = 0x01;

/// Write one byte to the terminal via uart-drv.
pub fn put_byte(b: u8) {
    crate::syscall::send(UART_DRV_PID, OP_WRITE, b as u64);
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
        // Should not reach here with infinite timeout, but guard against
        // spurious wakeups from future kernel changes.
        crate::syscall::yield_cpu();
    }
}
