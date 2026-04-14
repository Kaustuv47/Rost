//! GOP framebuffer output forwarder for the Rost shell.
//!
//! Every byte written to the UART is also forwarded to the GOP server so that
//! the graphical display mirrors the serial console.  To avoid saturating the
//! IPC queue with single-byte messages, bytes are batched into 40-byte chunks
//! (the payload capacity of one `OP_GOP_PUTS` message) and sent fire-and-forget
//! with `send_msg`.
//!
//! # Design notes
//! - PID lookup is done lazily on first use and cached.  A lookup failure
//!   (GOP not yet registered) is silently dropped — the GOP server may start
//!   after the shell's first output line.
//! - `send_msg` is non-blocking: if the GOP queue is full the message is
//!   dropped rather than stalling the shell.
//! - `flush()` must be called before blocking on input so that partial lines
//!   are visible on the display immediately (e.g. the prompt).

use core::ptr::addr_of_mut;
use crate::syscall::{send_msg, lookup, Msg};

// ── GOP IPC opcodes ───────────────────────────────────────────────────────────

const OP_GOP_PUTS: u64 = 0x71;

// ── State ─────────────────────────────────────────────────────────────────────

/// Cached PID of the "gop" server (0 = not yet resolved).
static mut GOP_PID: u64 = 0;

/// Pending byte buffer — packed into data[1..5] (5 × u64 = 40 bytes).
static mut BUF: [u8; 40] = [0u8; 40];
static mut BUF_LEN: usize = 0;

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Resolve the GOP PID on first call; returns 0 if not yet registered.
fn gop_pid() -> u64 {
    let cached = unsafe { GOP_PID };
    if cached != 0 { return cached; }
    let pid = lookup(b"gop\0\0\0\0\0\0\0\0\0\0\0\0\0");
    if pid != u64::MAX && pid != 0 {
        unsafe { GOP_PID = pid; }
        pid
    } else {
        0
    }
}

/// Send the current buffer contents to the GOP server and reset the buffer.
unsafe fn flush_inner(pid: u64) {
    let len = BUF_LEN;
    if len == 0 { return; }

    let mut msg = Msg::zeroed();
    msg.data[0] = OP_GOP_PUTS;
    // Pack bytes into data[1..5] (little-endian; receiver unpacks as &[u8; 40])
    let src = addr_of_mut!(BUF) as *const u8;
    let dst = msg.data[1..6].as_mut_ptr() as *mut u8;
    for i in 0..40usize {
        *dst.add(i) = *src.add(i);
    }

    // fire-and-forget — drop on queue full / GOP not running
    send_msg(pid, &msg);

    // Zero the buffer for next batch
    for b in &mut *addr_of_mut!(BUF) { *b = 0; }
    BUF_LEN = 0;
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Append one byte to the GOP output buffer; auto-flushes when the buffer fills.
pub fn write_byte(b: u8) {
    let pid = gop_pid();
    if pid == 0 { return; } // GOP not available yet

    unsafe {
        let len = BUF_LEN;
        (*addr_of_mut!(BUF))[len] = b;
        BUF_LEN = len + 1;
        if BUF_LEN == 40 {
            flush_inner(pid);
        }
    }
}

/// Flush any buffered bytes to the GOP server immediately.
pub fn flush() {
    let pid = gop_pid();
    if pid == 0 { return; }
    unsafe { flush_inner(pid); }
}
