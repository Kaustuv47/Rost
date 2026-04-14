//! POSIX `unistd.h` subset — process and I/O primitives.
//!
//! # File descriptor model
//!
//! | fd | Direction | Backend |
//! |----|-----------|---------|
//! | 0  | read      | uart-drv IPC (via `SYS_RECV` with 10-tick timeout) |
//! | 1  | write     | `SYS_UART_WRITE` (12) — direct COM1 output |
//! | 2  | write     | `SYS_UART_WRITE` (12) — same as stdout |
//! | ≥3 | read/write| VFS server OP_READ_FD / OP_WRITE_FD |
//!
//! # Note on `fork`
//!
//! `fork` is not implemented: the kernel has no copy-on-write page tables.
//! Use `exec_elf` to launch a new process from an ELF buffer.

use crate::errno::{set_errno, EINVAL, ENOMEM};
use crate::fcntl::to_vfd;
use crate::syscall::{exit as sys_exit, getpid as sys_getpid, uart_write, recv, clock};
use crate::vfs::{vfs_read_chunk, vfs_write_chunk, vfs_seek, CHUNK_SIZE};
use crate::types::{c_int, ssize_t, off_t, pid_t, SEEK_SET};

// ── getpid / exit ─────────────────────────────────────────────────────────────

/// Return the calling process's PID.
#[no_mangle]
pub extern "C" fn getpid() -> pid_t {
    sys_getpid()
}

/// Terminate the calling process with `status`.
#[no_mangle]
pub extern "C" fn exit(status: c_int) -> ! {
    sys_exit(status as u64)
}

/// Alias for `exit` — `_exit` does not flush stdio, which we don't buffer anyway.
#[no_mangle]
pub extern "C" fn _exit(status: c_int) -> ! {
    sys_exit(status as u64)
}

// ── read ─────────────────────────────────────────────────────────────────────

/// Read up to `count` bytes into `buf`.
///
/// * fd 0 (stdin): drains uart-drv keystrokes via SYS_RECV (10-tick timeout);
///   returns once at least one byte is available.
/// * fd ≥ 3: forwards to VFS OP_READ_FD (reads one chunk at a time).
///
/// Returns the number of bytes read, 0 for EOF, or -1 with errno set.
#[no_mangle]
pub unsafe extern "C" fn read(fd: c_int, buf: *mut u8, count: usize) -> ssize_t {
    if buf.is_null() || count == 0 { return 0; }
    let dst = core::slice::from_raw_parts_mut(buf, count);

    if fd == 0 {
        // stdin: poll uart-drv with a short timeout, spin until a byte arrives.
        loop {
            let v = recv(10); // 100 ms
            if v != u64::MAX { dst[0] = v as u8; return 1; }
            crate::syscall::yield_();
        }
    }

    let vfd = to_vfd(fd);
    if vfd < 0 { return -1; }

    let mut total: ssize_t = 0;
    while (total as usize) < count {
        let rem = count - total as usize;
        let chunk = rem.min(CHUNK_SIZE);
        let n = vfs_read_chunk(vfd as u8, &mut dst[total as usize..total as usize + chunk]);
        match n {
            -1 => { if total == 0 { return -1; } break; }
            0  => break, // EOF
            n  => total += n,
        }
    }
    total
}

// ── write ─────────────────────────────────────────────────────────────────────

/// Write `count` bytes from `buf` to `fd`.
///
/// * fd 1/2 (stdout/stderr): writes byte-by-byte via `SYS_UART_WRITE`.
/// * fd ≥ 3: forwards to VFS OP_WRITE_FD in CHUNK_SIZE pieces.
///
/// Returns the number of bytes written or -1 with errno set.
#[no_mangle]
pub unsafe extern "C" fn write(fd: c_int, buf: *const u8, count: usize) -> ssize_t {
    if buf.is_null() { return 0; }
    let src = core::slice::from_raw_parts(buf, count);

    if fd == 1 || fd == 2 {
        for &b in src { uart_write(b); }
        return count as ssize_t;
    }

    let vfd = to_vfd(fd);
    if vfd < 0 { return -1; }

    let mut total: ssize_t = 0;
    while (total as usize) < count {
        let rem  = count - total as usize;
        let chunk = rem.min(CHUNK_SIZE);
        let n = vfs_write_chunk(vfd as u8, &src[total as usize..total as usize + chunk]);
        if n < 0 { if total == 0 { return -1; } break; }
        total += n;
    }
    total
}

// ── lseek ─────────────────────────────────────────────────────────────────────

/// Reposition the file offset of the open file descriptor `fd`.
///
/// Only `SEEK_SET` is supported; `SEEK_CUR` and `SEEK_END` return `EINVAL`
/// (the VFS stores absolute offsets and does not expose a "current offset"
/// query over IPC in the current protocol).
#[no_mangle]
pub extern "C" fn lseek(fd: c_int, offset: off_t, whence: c_int) -> off_t {
    if whence != SEEK_SET { set_errno(EINVAL); return -1; }
    if offset < 0 || offset > u32::MAX as i64 { set_errno(EINVAL); return -1; }
    let vfd = to_vfd(fd);
    if vfd < 0 { return -1; }
    let ret = vfs_seek(vfd as u8, offset as u32);
    if ret == 0 { offset } else { -1 }
}

// ── close ─────────────────────────────────────────────────────────────────────

// Exported by `fcntl.rs` as `close`; re-export from here for completeness.
pub use crate::fcntl::close;

// ── sleep ─────────────────────────────────────────────────────────────────────

/// Suspend execution for `secs` seconds (100 Hz resolution → 10 ms accuracy).
/// Returns 0 on normal completion (interruption by signal is not modelled here).
#[no_mangle]
pub extern "C" fn sleep(secs: u32) -> u32 {
    let ticks = secs as u64 * 100; // 100 ticks per second
    let _ = recv(ticks);           // blocks until timeout (no message expected)
    0
}

/// Suspend execution for `usecs` microseconds (rounded up to 10 ms ticks).
#[no_mangle]
pub extern "C" fn usleep(usecs: u64) -> c_int {
    let ticks = (usecs + 9_999) / 10_000; // ceil to 10 ms ticks
    if ticks > 0 { let _ = recv(ticks); }
    0
}

// ── clock / time ─────────────────────────────────────────────────────────────

/// Return nanoseconds since boot (100 Hz resolution → 10 ms granularity).
/// Equivalent to `clock_gettime(CLOCK_MONOTONIC)` at the ns level.
#[no_mangle]
pub extern "C" fn clock_monotonic_ns() -> u64 {
    clock()
}

/// Return seconds since boot (truncated).
#[no_mangle]
pub extern "C" fn uptime_secs() -> u64 {
    clock() / 1_000_000_000
}

// ── sbrk (compatibility stub) ─────────────────────────────────────────────────

/// Minimal `sbrk` implementation.
///
/// Allocates `increment` bytes by mapping new pages via `SYS_MAP`.
/// Only positive increments are supported; decrement returns the current break.
/// Callers should prefer `malloc`/`free`; this exists for C code that calls
/// `sbrk` directly.
#[no_mangle]
pub unsafe extern "C" fn sbrk(increment: isize) -> *mut u8 {
    // Delegate to malloc for simplicity — sbrk is rarely called directly.
    if increment <= 0 {
        // Return current heap top as the "break".
        let bump = crate::malloc::heap_bump_export();
        return bump as *mut u8;
    }
    let ptr = crate::malloc::malloc(increment as usize);
    if ptr.is_null() {
        set_errno(ENOMEM);
        usize::MAX as *mut u8 // (void*)-1
    } else {
        ptr
    }
}


// ── exec_elf ──────────────────────────────────────────────────────────────────

/// Spawn a new ring-3 process from the ELF binary in `buf`.
///
/// This is the Rost equivalent of `execve`: the current process **continues
/// running** (Rost has no `fork`/`exec` replace semantics).  The new process
/// starts at the ELF entry point with an independent address space.
///
/// Returns the new PID on success or 0 on failure (errno not set; check the
/// kernel process table).
pub fn exec_elf(buf: &[u8], priority: u8) -> pid_t {
    if buf.len() < 4 || &buf[..4] != b"\x7fELF" { return 0; }
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      26u64,
            in("rdi")      buf.as_ptr() as u64,
            in("rsi")      buf.len()    as u64,
            in("rdx")      priority     as u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    if ret >= u64::MAX - 7 { 0 } else { ret as u32 }
}

// ── exec ──────────────────────────────────────────────────────────────────────

/// Execute a program by path, replacing the current process.
///
/// Reads the ELF binary at `path` from the VFS, spawns it as a new ring-3
/// process, then exits the calling process.  This provides POSIX `exec`
/// semantics (the caller does not return on success).
///
/// `path` must be a null-terminated ASCII string.
/// `priority` is passed to `SYS_SPAWN_ELF`; 0 = default (128).
///
/// On failure (path not found, ELF read error, spawn failure) returns -1
/// and the caller continues running.  On success the function does not return.
///
/// # Limitations
/// - No argument vector (argv) passing — spawned process gets no arguments.
/// - Maximum ELF size is `EXEC_MAX` bytes (512 KB); larger binaries are rejected.
/// - The calling process must have access to the VFS server.
#[no_mangle]
pub unsafe extern "C" fn exec(path: *const u8, priority: u8) -> c_int {
    use crate::fcntl::{open, close, O_RDONLY};

    if path.is_null() { set_errno(EINVAL); return -1; }

    // ── 1. Open the file via VFS ──────────────────────────────────────────────
    let fd = open(path as *const i8, O_RDONLY, 0);
    if fd < 0 { return -1; }   // errno set by open()

    // ── 2. Read ELF into a malloc'd buffer ───────────────────────────────────
    //
    // We read in 40-byte VFS chunks until EOF; `read()` handles the chunking.
    // 512 KB is more than enough for all current Rost server binaries.
    const EXEC_MAX: usize = 512 * 1024;

    let buf = crate::malloc::malloc(EXEC_MAX);
    if buf.is_null() {
        close(fd);
        set_errno(ENOMEM);
        return -1;
    }

    let mut total: usize = 0;
    loop {
        if total >= EXEC_MAX { break; } // truncate silently
        let n = read(fd, buf.add(total), EXEC_MAX - total);
        if n <= 0 { break; }
        total += n as usize;
    }
    close(fd);

    if total < 4 {
        crate::malloc::free(buf);
        set_errno(EINVAL);
        return -1;
    }

    // ── 3. Validate ELF magic ─────────────────────────────────────────────────
    let magic = core::slice::from_raw_parts(buf, 4);
    if magic != b"\x7fELF" {
        crate::malloc::free(buf);
        set_errno(EINVAL);
        return -1;
    }

    // ── 4. Spawn the new process ──────────────────────────────────────────────
    let elf_slice = core::slice::from_raw_parts(buf, total);
    let pid = exec_elf(elf_slice, priority);

    crate::malloc::free(buf);

    if pid == 0 {
        set_errno(ENOMEM);
        return -1;
    }

    // ── 5. Replace calling process ────────────────────────────────────────────
    // SYS_EXIT terminates the caller; the new process continues independently.
    sys_exit(0)
}
