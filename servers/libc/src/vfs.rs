#![allow(dead_code)] // Protocol constants are API surface even if not all consumed internally.
//! Internal VFS IPC client.
//!
//! Wraps the VFS server's stateful fd API (OP_OPEN / OP_CLOSE / OP_READ_FD /
//! OP_WRITE_FD / OP_SEEK) for use by `fcntl` and `unistd`.
//!
//! The VFS PID is resolved lazily via `SYS_LOOKUP("rost-vfs")` and cached.
//! If the VFS is not registered yet, operations return `EIO`.

use core::sync::atomic::{AtomicU32, Ordering};
use crate::syscall::{call, lookup, Msg};
use crate::errno::{set_errno, EIO, ENOENT, EBADF, ENOSPC, EACCES, EISDIR, EMFILE};

// ── VFS opcodes & responses ───────────────────────────────────────────────────

pub const OP_READ:       u64 = 0x22; // stateless read (path + offset)
pub const OP_STAT:       u64 = 0x23; // stat path
pub const OP_OPEN:       u64 = 0x30;
pub const OP_CLOSE:      u64 = 0x31;
pub const OP_READ_FD:    u64 = 0x32;
pub const OP_WRITE_FD:   u64 = 0x33;
pub const OP_SEEK:       u64 = 0x34;
pub const OP_FSTAT:      u64 = 0x35;

pub const RESP_DONE:     u64 = 0x81;
pub const RESP_DATA:     u64 = 0x82;
pub const RESP_STAT:     u64 = 0x83;
pub const RESP_OK:       u64 = 0x85;
pub const RESP_FD:       u64 = 0x86;
pub const RESP_ERROR:    u64 = 0x8F;

/// VFS errno codes (from servers/vfs/src/proto.rs)
pub const VFS_ENOENT:  u64 = 1;
pub const VFS_ENOTDIR: u64 = 2;
pub const VFS_EISDIR:  u64 = 3;
pub const VFS_ENOSYS:  u64 = 4;
pub const VFS_ENOSPC:  u64 = 5;
pub const VFS_EBADF:   u64 = 6;
pub const VFS_EMFILE:  u64 = 7;
pub const VFS_EACCES:  u64 = 8;

/// IPC chunk size (bytes per OP_READ_FD / OP_WRITE_FD message).
pub const CHUNK_SIZE: usize = 40;

/// Timeout in scheduler ticks for VFS IPC calls.
const VFS_TIMEOUT: u64 = 200;

// ── Cached VFS PID ────────────────────────────────────────────────────────────

static VFS_PID: AtomicU32 = AtomicU32::new(0);

fn vfs_pid() -> Option<u64> {
    let cached = VFS_PID.load(Ordering::Relaxed);
    if cached != 0 { return Some(cached as u64); }
    let pid = lookup(b"rost-vfs\0");
    if pid == u64::MAX { return None; }
    VFS_PID.store(pid as u32, Ordering::Relaxed);
    Some(pid)
}

fn map_vfs_errno(code: u64) {
    let e = match code {
        VFS_ENOENT  => ENOENT,
        VFS_EISDIR  => EISDIR,
        VFS_ENOSPC  => ENOSPC,
        VFS_EBADF   => EBADF,
        VFS_EMFILE  => EMFILE,
        VFS_EACCES  => EACCES,
        _           => EIO,
    };
    set_errno(e);
}

// ── Path packing ──────────────────────────────────────────────────────────────

/// Pack a path byte slice into 6 × u64 words (48 bytes, null-padded).
pub fn pack_path(path: &[u8]) -> [u64; 6] {
    let mut words = [0u64; 6];
    let bytes: &mut [u8; 48] = unsafe {
        &mut *(&mut words as *mut [u64; 6] as *mut [u8; 48])
    };
    let n = path.len().min(47);
    bytes[..n].copy_from_slice(&path[..n]);
    bytes[n] = 0; // null-terminate
    words
}

/// Pack up to 40 data bytes into 5 × u64 words (little-endian).
pub fn pack_data(src: &[u8]) -> [u64; 5] {
    let mut words = [0u64; 5];
    let bytes: &mut [u8; 40] = unsafe {
        &mut *(&mut words as *mut [u64; 5] as *mut [u8; 40])
    };
    let n = src.len().min(CHUNK_SIZE);
    bytes[..n].copy_from_slice(&src[..n]);
    words
}

/// Unpack up to 40 bytes from 5 × u64 words into `dst`.
/// Returns the number of bytes written.
pub fn unpack_data(words: &[u64; 5], dst: &mut [u8], count: usize) -> usize {
    let src_bytes: &[u8; 40] = unsafe {
        &*(words as *const [u64; 5] as *const [u8; 40])
    };
    let n = count.min(CHUNK_SIZE).min(dst.len());
    dst[..n].copy_from_slice(&src_bytes[..n]);
    n
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Open a file via the VFS stateful fd API.
/// Returns the VFS fd number (0–255) or -1 on error (errno set).
pub fn vfs_open(path: &[u8], oflags: u8) -> i32 {
    let pid = match vfs_pid() { Some(p) => p, None => { set_errno(EIO); return -1; } };
    let pw = pack_path(path);
    let mut req = Msg::zeroed();
    req.data[0] = OP_OPEN;
    req.data[1] = oflags as u64;
    req.data[2..8].copy_from_slice(&pw);
    let mut resp = Msg::zeroed();
    if !call(pid, &req, &mut resp, VFS_TIMEOUT) { set_errno(EIO); return -1; }
    match resp.data[0] {
        RESP_FD    => resp.data[1] as i32,
        RESP_ERROR => { map_vfs_errno(resp.data[1]); -1 }
        _          => { set_errno(EIO); -1 }
    }
}

/// Close a VFS fd.  Returns 0 or -1 (errno set).
pub fn vfs_close(vfd: u8) -> i32 {
    let pid = match vfs_pid() { Some(p) => p, None => { set_errno(EIO); return -1; } };
    let mut req = Msg::zeroed();
    req.data[0] = OP_CLOSE;
    req.data[1] = vfd as u64;
    let mut resp = Msg::zeroed();
    if !call(pid, &req, &mut resp, VFS_TIMEOUT) { set_errno(EIO); return -1; }
    match resp.data[0] {
        RESP_OK    => 0,
        RESP_ERROR => { map_vfs_errno(resp.data[1]); -1 }
        _          => { set_errno(EIO); -1 }
    }
}

/// Read the next chunk via an open VFS fd.  Returns bytes copied into `buf`
/// (0 = EOF, -1 = error with errno set).
pub fn vfs_read_chunk(vfd: u8, buf: &mut [u8]) -> isize {
    let pid = match vfs_pid() { Some(p) => p, None => { set_errno(EIO); return -1; } };
    let mut req = Msg::zeroed();
    req.data[0] = OP_READ_FD;
    req.data[1] = vfd as u64;
    let mut resp = Msg::zeroed();
    if !call(pid, &req, &mut resp, VFS_TIMEOUT) { set_errno(EIO); return -1; }
    match resp.data[0] {
        RESP_DATA => {
            let chunk_len = resp.data[2] as usize;
            let words: &[u64; 5] = resp.data[3..8].try_into().unwrap();
            let n = unpack_data(words, buf, chunk_len);
            n as isize
        }
        RESP_DONE  => 0,
        RESP_ERROR => { map_vfs_errno(resp.data[1]); -1 }
        _          => { set_errno(EIO); -1 }
    }
}

/// Write one chunk via an open VFS fd.  Returns bytes written or -1 (errno).
pub fn vfs_write_chunk(vfd: u8, data: &[u8]) -> isize {
    let pid = match vfs_pid() { Some(p) => p, None => { set_errno(EIO); return -1; } };
    let n = data.len().min(CHUNK_SIZE);
    let words = pack_data(&data[..n]);
    let mut req = Msg::zeroed();
    req.data[0] = OP_WRITE_FD;
    req.data[1] = vfd as u64;
    req.data[2] = n as u64;
    req.data[3..8].copy_from_slice(&words);
    let mut resp = Msg::zeroed();
    if !call(pid, &req, &mut resp, VFS_TIMEOUT) { set_errno(EIO); return -1; }
    match resp.data[0] {
        RESP_OK    => n as isize,
        RESP_ERROR => { map_vfs_errno(resp.data[1]); -1 }
        _          => { set_errno(EIO); -1 }
    }
}

/// Seek to an absolute offset in an open VFS fd.  Returns 0 or -1 (errno).
pub fn vfs_seek(vfd: u8, offset: u32) -> i32 {
    let pid = match vfs_pid() { Some(p) => p, None => { set_errno(EIO); return -1; } };
    let mut req = Msg::zeroed();
    req.data[0] = OP_SEEK;
    req.data[1] = vfd as u64;
    req.data[2] = offset as u64;
    let mut resp = Msg::zeroed();
    if !call(pid, &req, &mut resp, VFS_TIMEOUT) { set_errno(EIO); return -1; }
    match resp.data[0] {
        RESP_OK    => 0,
        RESP_ERROR => { map_vfs_errno(resp.data[1]); -1 }
        _          => { set_errno(EIO); -1 }
    }
}

/// Stat an open VFS fd.  Fills `is_dir` and `size`.  Returns 0 or -1.
pub fn vfs_fstat(vfd: u8, is_dir: &mut bool, size: &mut u64) -> i32 {
    let pid = match vfs_pid() { Some(p) => p, None => { set_errno(EIO); return -1; } };
    let mut req = Msg::zeroed();
    req.data[0] = OP_FSTAT;
    req.data[1] = vfd as u64;
    let mut resp = Msg::zeroed();
    if !call(pid, &req, &mut resp, VFS_TIMEOUT) { set_errno(EIO); return -1; }
    match resp.data[0] {
        RESP_STAT  => {
            *is_dir = resp.data[1] & 1 != 0;
            *size   = resp.data[2];
            0
        }
        RESP_ERROR => { map_vfs_errno(resp.data[1]); -1 }
        _          => { set_errno(EIO); -1 }
    }
}

/// Stat a path (does not require an open fd).  Fills `is_dir` and `size`.
pub fn vfs_stat(path: &[u8], is_dir: &mut bool, size: &mut u64) -> i32 {
    let pid = match vfs_pid() { Some(p) => p, None => { set_errno(EIO); return -1; } };
    let pw = pack_path(path);
    let mut req = Msg::zeroed();
    req.data[0] = OP_STAT;
    req.data[2..8].copy_from_slice(&pw);
    let mut resp = Msg::zeroed();
    if !call(pid, &req, &mut resp, VFS_TIMEOUT) { set_errno(EIO); return -1; }
    match resp.data[0] {
        RESP_STAT  => {
            *is_dir = resp.data[1] & 1 != 0;
            *size   = resp.data[2];
            0
        }
        RESP_ERROR => { map_vfs_errno(resp.data[1]); -1 }
        _          => { set_errno(EIO); -1 }
    }
}
