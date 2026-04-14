//! POSIX signal handling — mapped to Rost IPC notifications.
//!
//! # Model
//!
//! Rost processes communicate asynchronously via `SYS_NOTIFY` which ORs bits
//! into a per-process notification word.  POSIX signals are mapped onto this
//! mechanism:
//!
//! | Signal    | Number | Notification bit |
//! |-----------|--------|-----------------|
//! | SIGHUP    |  1     | bit 0           |
//! | SIGINT    |  2     | bit 1           |
//! | SIGQUIT   |  3     | bit 2           |
//! | SIGTERM   | 15     | bit 3           |
//! | SIGUSR1   | 10     | bit 4           |
//! | SIGUSR2   | 12     | bit 5           |
//! | SIGCHLD   | 17     | bit 6           |
//! | SIGALRM   | 14     | bit 7           |
//!
//! `kill(pid, sig)` issues `SYS_NOTIFY(pid, 1 << bit(sig))`.
//!
//! # Delivery
//!
//! Signal delivery in Rost is **cooperative**: handlers are invoked at
//! explicit check-points by calling `signal_dispatch()`.  This function
//! polls the pending notification word via `SYS_RECV(0)` and invokes any
//! registered handlers for bits that are set.
//!
//! `SIG_DFL` for most signals halts the process.  `SIG_IGN` silently discards
//! the signal.  A custom handler is invoked with the signal number and then
//! returns normally (no `sigreturn` trampoline needed).

use core::sync::atomic::{AtomicUsize, Ordering};
use crate::syscall::{notify, recv};
use crate::types::{c_int, pid_t};

// ── Signal numbers ────────────────────────────────────────────────────────────

pub const SIGHUP:  c_int = 1;
pub const SIGINT:  c_int = 2;
pub const SIGQUIT: c_int = 3;
pub const SIGILL:  c_int = 4;
pub const SIGABRT: c_int = 6;
pub const SIGBUS:  c_int = 7;
pub const SIGFPE:  c_int = 8;
pub const SIGKILL: c_int = 9;  // cannot be caught; kill delivers it but handler is SIG_DFL → halt
pub const SIGUSR1: c_int = 10;
pub const SIGSEGV: c_int = 11;
pub const SIGUSR2: c_int = 12;
pub const SIGPIPE: c_int = 13;
pub const SIGALRM: c_int = 14;
pub const SIGTERM: c_int = 15;
pub const SIGCHLD: c_int = 17;

/// Maximum signal number tracked by this layer.
const NSIG: usize = 32;

// ── Special handler values ────────────────────────────────────────────────────

/// Default action (stored as 0 in the handler table).
pub const SIG_DFL: usize = 0;
/// Ignore the signal (stored as 1 in the handler table).
pub const SIG_IGN: usize = 1;

pub type SigHandler = unsafe extern "C" fn(c_int);

// ── Handler table ─────────────────────────────────────────────────────────────

/// Per-signal handler stored as a raw function pointer cast to `usize`.
///  0 = SIG_DFL, 1 = SIG_IGN, otherwise a function pointer.
static HANDLERS: [AtomicUsize; NSIG] = {
    const Z: AtomicUsize = AtomicUsize::new(SIG_DFL);
    [Z; NSIG]
};

// ── Signal → notification bit mapping ────────────────────────────────────────

fn sig_to_bit(sig: c_int) -> Option<u64> {
    let bit: u32 = match sig {
        SIGHUP  => 0,
        SIGINT  => 1,
        SIGQUIT => 2,
        SIGTERM => 3,
        SIGUSR1 => 4,
        SIGUSR2 => 5,
        SIGCHLD => 6,
        SIGALRM => 7,
        SIGKILL => 8,
        _       => return None,
    };
    Some(1u64 << bit)
}

fn bit_to_sig(bit: u32) -> c_int {
    match bit {
        0 => SIGHUP,
        1 => SIGINT,
        2 => SIGQUIT,
        3 => SIGTERM,
        4 => SIGUSR1,
        5 => SIGUSR2,
        6 => SIGCHLD,
        7 => SIGALRM,
        8 => SIGKILL,
        _ => -1,
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Register a signal handler.
///
/// * `signum` — the signal number (`SIGTERM`, `SIGINT`, …)
/// * `handler` — `SIG_DFL` (0), `SIG_IGN` (1), or a function pointer
///
/// Returns the previous handler value, or `SIG_ERR` (-1 as usize) if
/// `signum` is out of range.
///
/// # Safety
/// The handler will be called from `signal_dispatch()` which runs in normal
/// process context.  The handler must not call `signal_dispatch()` recursively.
#[no_mangle]
pub unsafe extern "C" fn signal(signum: c_int, handler: usize) -> usize {
    if signum <= 0 || signum as usize >= NSIG { return usize::MAX; } // SIG_ERR
    HANDLERS[signum as usize].swap(handler, Ordering::Relaxed)
}

/// Send signal `sig` to process `pid`.
///
/// Translates the signal number to a notification bit and issues
/// `SYS_NOTIFY(pid, bit)`.  Returns 0 on success or -1 if the signal is
/// unknown or `SYS_NOTIFY` fails.
#[no_mangle]
pub extern "C" fn kill(pid: pid_t, sig: c_int) -> c_int {
    let bit = match sig_to_bit(sig) {
        Some(b) => b,
        None    => return -1,
    };
    if notify(pid, bit) { 0 } else { -1 }
}

/// Send signal `sig` to the calling process.
#[no_mangle]
pub extern "C" fn raise(sig: c_int) -> c_int {
    let pid = crate::syscall::getpid();
    kill(pid, sig)
}

/// Poll pending signal notifications and invoke registered handlers.
///
/// This function should be called from the main loop or before/after any
/// potentially long-running operation.  It drains the pending notification
/// word once and dispatches all set bits whose corresponding handler is not
/// `SIG_IGN`.
///
/// `SIG_DFL` for most signals calls `_exit(128 + sig)` (standard behaviour).
/// SIGKILL always halts regardless of the registered handler.
pub fn signal_dispatch() {
    // Poll the notification word non-blocking.
    // SYS_RECV(0) returns u64::MAX if no message/notification pending.
    let word = recv(0);
    if word == u64::MAX || word == 0 { return; }

    for bit in 0u32..9 {
        if word & (1u64 << bit) == 0 { continue; }
        let sig = bit_to_sig(bit);
        if sig <= 0 { continue; }

        // SIGKILL cannot be caught.
        if sig == SIGKILL {
            crate::syscall::exit(128 + SIGKILL as u64);
        }

        let h = HANDLERS[sig as usize].load(Ordering::Relaxed);
        match h {
            SIG_IGN => {}       // silently discard
            SIG_DFL => {
                // Default: terminate with exit code 128+sig.
                crate::syscall::exit(128 + sig as u64);
            }
            fn_ptr => {
                // Call user handler.
                let handler: SigHandler = unsafe { core::mem::transmute(fn_ptr) };
                unsafe { handler(sig); }
            }
        }
    }
}
