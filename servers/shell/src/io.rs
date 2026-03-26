//! Terminal I/O for the shell server.
//!
//! **Output**: `put_byte` / `print_str` call `SYS_UART_WRITE` (syscall 12)
//! directly.  This bypasses the uart-drv IPC queue, eliminating queue-overflow
//! byte loss when the shell emits the banner or redraws the prompt.
//!
//! **Input**: keystrokes arrive as IPC messages sent by uart-drv
//! (`SYS_SEND(shell_pid, byte, 0)`).  `read_byte` polls via `SYS_RECV`
//! (timeout=0); `read_byte_blocking` blocks until a byte arrives.

/// Write one byte to the terminal.
///
/// Uses `SYS_UART_WRITE` (syscall 12) to write directly to COM1 via the
/// kernel, bypassing the uart-drv IPC queue.  This prevents queue-overflow
/// byte loss when the shell prints the banner or redraws the prompt (both
/// of which emit far more than the 16-message queue depth in one burst).
///
/// Input (keystrokes) still flows uart-drv → SYS_SEND → shell IPC queue,
/// read back via `read_byte()` / `read_byte_blocking()`.
///
/// No CR/LF translation.  Use `put_newline()` for a proper line break.
pub fn put_byte(b: u8) {
    crate::syscall::uart_write(b);
}

/// Emit a proper newline: CR (\r) then LF (\n).
///
/// The UART layer does no translation, so callers must use this instead of
/// `put_byte(b'\n')` whenever a visible line break is needed.
pub fn put_newline() {
    crate::syscall::uart_write(b'\r');
    crate::syscall::uart_write(b'\n');
}

/// Write a string slice to the terminal.
///
/// Every `\n` in the string is sent as `\r\n` so that the terminal moves to
/// the start of the next line.
pub fn print_str(s: &str) {
    for b in s.bytes() {
        if b == b'\n' {
            put_newline();
        } else {
            put_byte(b);
        }
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
