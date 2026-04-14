//! File open/create/flags — POSIX `fcntl.h` subset.
//!
//! `open()` is the primary entry point.  All file operations go through the
//! VFS server (rost-vfs, PID 4) via the stateful fd IPC API.
//!
//! # File descriptor numbering
//!
//! POSIX fds 0 / 1 / 2 are the standard streams and are handled directly by
//! `unistd::read` / `write` without consulting the VFS.  FDs 3–`OPEN_MAX` are
//! backed by the fd table below which maps POSIX fd → VFS fd.
//!
//! The VFS fd table is per-process and lives inside the VFS server's BSS.  The
//! libc fd table here is the local mapping that translates POSIX fd numbers to
//! the VFS fd numbers assigned by the server.

use core::sync::atomic::{AtomicI16, Ordering};
use crate::errno::{set_errno, EINVAL, EBADF, EMFILE};
use crate::vfs::vfs_open;
use crate::types::c_int;

// ── O_* flags (must match proto.rs O_* values) ───────────────────────────────

pub const O_RDONLY: c_int = 0;
pub const O_WRONLY: c_int = 1;
pub const O_RDWR:   c_int = 2;
pub const O_CREAT:  c_int = 4;
pub const O_TRUNC:  c_int = 8;
pub const O_APPEND: c_int = 16;

// ── POSIX fd → VFS fd table ───────────────────────────────────────────────────

/// Maximum POSIX file descriptors (0–OPEN_MAX-1).
/// 0/1/2 = stdin/stdout/stderr; 3..OPEN_MAX = VFS-backed.
const OPEN_MAX: usize = 32;

/// VFS fd for each POSIX fd slot.  -1 = slot not in use.
/// Slots 0, 1, 2 are the standard streams and are never assigned VFS fds here
/// (they are handled by read/write directly).
static FD_TABLE: [AtomicI16; OPEN_MAX] = {
    const NEG1: AtomicI16 = AtomicI16::new(-1);
    [NEG1; OPEN_MAX]
};

/// Allocate the next free POSIX fd slot ≥ 3.  Returns `EMFILE` if full.
fn alloc_fd(vfd: i16) -> c_int {
    for i in 3..OPEN_MAX {
        if FD_TABLE[i].compare_exchange(-1, vfd, Ordering::SeqCst, Ordering::Relaxed).is_ok() {
            return i as c_int;
        }
    }
    set_errno(EMFILE);
    -1
}

/// Return the VFS fd for POSIX fd `fd`, or -1 with `EBADF` set.
pub fn to_vfd(fd: c_int) -> i32 {
    if fd < 3 || fd as usize >= OPEN_MAX { set_errno(EBADF); return -1; }
    let vfd = FD_TABLE[fd as usize].load(Ordering::Relaxed);
    if vfd < 0 { set_errno(EBADF); return -1; }
    vfd as i32
}

/// Release a POSIX fd slot.
fn release_fd(fd: c_int) {
    if fd >= 3 && (fd as usize) < OPEN_MAX {
        FD_TABLE[fd as usize].store(-1, Ordering::Relaxed);
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Open or create a file.
///
/// * `path`  — null-terminated path string
/// * `flags` — combination of `O_RDONLY` / `O_WRONLY` / `O_RDWR` and
///             optionally `O_CREAT` / `O_TRUNC` / `O_APPEND`
/// * `_mode` — ignored (Rost has no Unix permission bits)
///
/// Returns a non-negative file descriptor on success, or -1 with `errno` set.
#[no_mangle]
pub unsafe extern "C" fn open(path: *const i8, flags: c_int, _mode: u32) -> c_int {
    if path.is_null() { set_errno(EINVAL); return -1; }
    // Build a byte slice from the C string.
    let path_bytes = {
        let mut len = 0usize;
        while *path.add(len) != 0 { len += 1; }
        core::slice::from_raw_parts(path as *const u8, len + 1) // includes \0
    };
    let oflags = (flags & 0x1F) as u8;
    let vfd = vfs_open(path_bytes, oflags);
    if vfd < 0 { return -1; } // errno already set by vfs_open
    let posix_fd = alloc_fd(vfd as i16);
    if posix_fd < 0 {
        // Ran out of local fd slots; close the VFS fd we just opened.
        let _ = crate::vfs::vfs_close(vfd as u8);
    }
    posix_fd
}

/// Close a file descriptor.  Returns 0 on success or -1 with `errno` set.
#[no_mangle]
pub extern "C" fn close(fd: c_int) -> c_int {
    let vfd = to_vfd(fd);
    if vfd < 0 { return -1; }
    let ret = crate::vfs::vfs_close(vfd as u8);
    if ret == 0 { release_fd(fd); }
    ret
}

/// Safe Rust wrapper for `open`.
#[inline]
pub fn open_path(path: &[u8], flags: c_int) -> c_int {
    unsafe { open(path.as_ptr() as *const i8, flags, 0) }
}
