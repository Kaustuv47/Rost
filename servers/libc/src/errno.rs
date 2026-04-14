//! POSIX error constants and per-process errno accessor.
//!
//! `errno` is stored as a process-global `AtomicI32` (single-threaded model;
//! no true threads in Rost).  Signal delivery is cooperative so there is no
//! race between the setter and any handler.

use core::sync::atomic::{AtomicI32, Ordering};

// ── Error constant definitions ────────────────────────────────────────────────

pub const EPERM:        i32 = 1;   // Operation not permitted
pub const ENOENT:       i32 = 2;   // No such file or directory
pub const ESRCH:        i32 = 3;   // No such process
pub const EINTR:        i32 = 4;   // Interrupted system call
pub const EIO:          i32 = 5;   // I/O error
pub const ENXIO:        i32 = 6;   // No such device or address
pub const ENOMEM:       i32 = 12;  // Out of memory
pub const EACCES:       i32 = 13;  // Permission denied
pub const EFAULT:       i32 = 14;  // Bad address
pub const EBUSY:        i32 = 16;  // Device or resource busy
pub const ENODEV:       i32 = 19;  // No such device
pub const ENOTDIR:      i32 = 20;  // Not a directory
pub const EISDIR:       i32 = 21;  // Is a directory
pub const EINVAL:       i32 = 22;  // Invalid argument
pub const EMFILE:       i32 = 24;  // Too many open files
pub const ENOSPC:       i32 = 28;  // No space left on device
pub const ESPIPE:       i32 = 29;  // Illegal seek
pub const EBADF:        i32 = 9;   // Bad file descriptor
pub const EAGAIN:       i32 = 11;  // Try again / Resource temporarily unavailable
pub const EDEADLK:      i32 = 35;  // Resource deadlock avoided
pub const ENOSYS:       i32 = 38;  // Function not implemented
pub const ETIMEDOUT:    i32 = 110; // Connection timed out
pub const ENOTSUP:      i32 = 95;  // Operation not supported

// ── errno storage ─────────────────────────────────────────────────────────────

static ERRNO_VAL: AtomicI32 = AtomicI32::new(0);

/// Return the current errno value.
#[inline]
pub fn errno() -> i32 {
    ERRNO_VAL.load(Ordering::Relaxed)
}

/// Set errno.  Called by every library function that encounters an error before
/// returning -1 / NULL.
#[inline]
pub fn set_errno(e: i32) {
    ERRNO_VAL.store(e, Ordering::Relaxed);
}

/// Clear errno (set to 0).
#[inline]
pub fn clear_errno() {
    ERRNO_VAL.store(0, Ordering::Relaxed);
}

// ── C-ABI accessor (for code that reads `errno` as a variable) ───────────────

/// C-compatible errno accessor: `*__errno_location() = errno`.
///
/// Standard libc exposes `errno` as a macro expanding to `*__errno_location()`.
/// Provide the underlying symbol so C code compiled for Rost can link against it.
#[no_mangle]
pub extern "C" fn __errno_location() -> *mut i32 {
    // SAFETY: single-threaded; the static lives for the program lifetime.
    ERRNO_VAL.as_ptr()
}
