//! Per-process file descriptor table for the VFS server.
//!
//! Provides a fixed-size, heap-free table of open file descriptors.
//! Each process may have up to `FDS_PER_PROC` simultaneously open fds.
//! Total BSS: `MAX_FDS` × ~80 bytes ≈ 20 KB.
//!
//! # Lifecycle
//! ```text
//! OP_OPEN  → fd::alloc()  → fd number returned to client
//! OP_CLOSE → fd::free()
//! OP_READ_FD / OP_WRITE_FD → fd::get() then fd::advance()
//! OP_SEEK  → fd::seek()
//! ```

/// Maximum simultaneously open file descriptors across all processes.
pub const MAX_FDS: usize = 256; // 8 per process × 32 processes

/// Maximum open fds per single process.
pub const FDS_PER_PROC: u8 = 8;

// ── Open flags (POSIX-compatible bit assignments) ─────────────────────────────
/// Open for reading only.
pub const O_RDONLY: u8 = 0x00;
/// Open for writing only.
pub const O_WRONLY: u8 = 0x01;
/// Open for reading and writing.
pub const O_RDWR:   u8 = 0x02;
/// Create file if it does not exist (requires mutable overlay).
pub const O_CREAT:  u8 = 0x04;
/// Truncate file to zero length on open.
pub const O_TRUNC:  u8 = 0x08;
/// All writes go to the end of the file.
pub const O_APPEND: u8 = 0x10;

// ── FdEntry ───────────────────────────────────────────────────────────────────

#[derive(Copy, Clone)]
pub struct FdEntry {
    pub pid:    u32,
    pub fd:     u8,       // fd number (0–FDS_PER_PROC-1) within the process
    pub oflags: u8,       // open flags (O_RDONLY/O_WRONLY/O_RDWR | O_CREAT | O_TRUNC | O_APPEND)
    pub offset: u32,      // current byte position
    path:       [u8; 64],
    pub plen:   u8,
    pub used:   bool,
}

impl FdEntry {
    const fn empty() -> Self {
        FdEntry {
            pid:    0,
            fd:     0,
            oflags: 0,
            offset: 0,
            path:   [0u8; 64],
            plen:   0,
            used:   false,
        }
    }

    /// Absolute path bytes (no null padding).
    pub fn path_bytes(&self) -> &[u8] {
        &self.path[..self.plen as usize]
    }

    /// Returns `true` if this fd was opened for reading.
    pub fn readable(&self) -> bool {
        let amode = self.oflags & 0x03;
        amode == O_RDONLY || amode == O_RDWR
    }

    /// Returns `true` if this fd was opened for writing.
    pub fn writable(&self) -> bool {
        let amode = self.oflags & 0x03;
        amode == O_WRONLY || amode == O_RDWR
    }
}

// ── Storage ───────────────────────────────────────────────────────────────────

static mut TABLE: [FdEntry; MAX_FDS] = {
    const E: FdEntry = FdEntry::empty();
    [E; MAX_FDS]
};

// ── Public API ────────────────────────────────────────────────────────────────

/// Allocate a new fd for `pid` at canonical `path` with `oflags`.
///
/// Returns the fd number (0–`FDS_PER_PROC`-1) on success, or `None` if the
/// per-process fd limit is reached, the global table is full, or the path is
/// longer than 64 bytes.
pub fn alloc(pid: u32, path: &[u8], oflags: u8) -> Option<u8> {
    let plen = path.len();
    if plen == 0 || plen > 64 { return None; }

    unsafe {
        let tbl = &mut *core::ptr::addr_of_mut!(TABLE);

        // Collect which fd numbers are already in use for this pid.
        let mut used_fds = [false; FDS_PER_PROC as usize];
        for slot in tbl.iter() {
            if slot.used && slot.pid == pid {
                let f = slot.fd as usize;
                if f < FDS_PER_PROC as usize {
                    used_fds[f] = true;
                }
            }
        }

        // Pick the lowest available fd number.
        let mut fd_num = FDS_PER_PROC; // sentinel = "none available"
        for f in 0..FDS_PER_PROC {
            if !used_fds[f as usize] {
                fd_num = f;
                break;
            }
        }
        if fd_num == FDS_PER_PROC { return None; } // per-process limit hit

        // Find a free slot in the global table.
        for slot in tbl.iter_mut() {
            if !slot.used {
                *slot        = FdEntry::empty();
                slot.pid     = pid;
                slot.fd      = fd_num;
                slot.oflags  = oflags;
                slot.offset  = 0;
                slot.path[..plen].copy_from_slice(path);
                slot.plen    = plen as u8;
                slot.used    = true;
                return Some(fd_num);
            }
        }
    }
    None // global table full
}

/// Look up the fd entry for `(pid, fd_num)`.
/// Returns a shared reference to the entry, or `None` if not found.
pub fn get(pid: u32, fd: u8) -> Option<&'static FdEntry> {
    unsafe {
        let tbl = &*core::ptr::addr_of!(TABLE);
        for slot in tbl.iter() {
            if slot.used && slot.pid == pid && slot.fd == fd {
                return Some(slot);
            }
        }
    }
    None
}

/// Advance the file-position offset for `(pid, fd)` by `delta` bytes.
pub fn advance(pid: u32, fd: u8, delta: usize) {
    unsafe {
        let tbl = &mut *core::ptr::addr_of_mut!(TABLE);
        for slot in tbl.iter_mut() {
            if slot.used && slot.pid == pid && slot.fd == fd {
                slot.offset = slot.offset.saturating_add(delta as u32);
                return;
            }
        }
    }
}

/// Set the file-position offset for `(pid, fd)` to `new_offset` (absolute).
pub fn seek(pid: u32, fd: u8, new_offset: u32) {
    unsafe {
        let tbl = &mut *core::ptr::addr_of_mut!(TABLE);
        for slot in tbl.iter_mut() {
            if slot.used && slot.pid == pid && slot.fd == fd {
                slot.offset = new_offset;
                return;
            }
        }
    }
}

/// Close (free) the fd entry for `(pid, fd_num)`.
/// Returns `true` if found and freed, `false` if no such entry exists.
pub fn free(pid: u32, fd: u8) -> bool {
    unsafe {
        let tbl = &mut *core::ptr::addr_of_mut!(TABLE);
        for slot in tbl.iter_mut() {
            if slot.used && slot.pid == pid && slot.fd == fd {
                slot.used = false;
                return true;
            }
        }
    }
    false
}

/// Close all file descriptors belonging to `pid`.
///
/// Called implicitly when a process exits or is restarted by init.
pub fn free_all(pid: u32) {
    unsafe {
        let tbl = &mut *core::ptr::addr_of_mut!(TABLE);
        for slot in tbl.iter_mut() {
            if slot.pid == pid {
                slot.used = false;
            }
        }
    }
}

/// Close all file descriptors whose path matches `path` (any process).
///
/// Called by `handle_unlink` so that fds pointing at a removed file become
/// invalid before the mutable node is freed.
pub fn free_all_path(path: &[u8]) {
    unsafe {
        let tbl = &mut *core::ptr::addr_of_mut!(TABLE);
        for slot in tbl.iter_mut() {
            if slot.used && slot.path_bytes() == path {
                slot.used = false;
            }
        }
    }
}
