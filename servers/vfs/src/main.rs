//! Rost VFS Server — virtual filesystem IPC server.
//!
//! Runs as a ring-3 ELF binary, PID 3 by convention.
//! Backed by a static in-memory directory tree (fs.rs).
//!
//! # Supported operations
//! - OP_READDIR / OP_LIST  — list directory entries
//! - OP_READ               — read file data (chunked, stateless)
//! - OP_STAT               — query path metadata
//! - OP_MOUNT              — query mount table
#![no_std]
#![no_main]

mod fs;
mod proto;
mod syscall;

use proto::*;
use syscall::{Msg, recv_msg, send_msg};

/// Mount table — one entry per filesystem root.
struct MountPoint {
    path:   &'static [u8],
    fstype: &'static [u8],
    source: &'static [u8],
}

static MOUNTS: &[MountPoint] = &[
    MountPoint { path: b"/",    fstype: b"ramfs",  source: b"ramdisk:0" },
    // Future entries once block-drv + FAT32 parser are implemented:
    // MountPoint { path: b"/dev",  fstype: b"devfs",  source: b"virtual" },
    // MountPoint { path: b"/proc", fstype: b"procfs", source: b"virtual" },
];

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Register so SYS_LOOKUP("rost-vfs") resolves to our PID.
    // Required by: init (heartbeat tracking) and shell (VFS IPC).
    syscall::register(b"rost-vfs\0\0\0\0\0\0\0\0");

    // Signal init (PID 1) that the VFS is ready.
    syscall::notify(1, 0x5246_5342_5f52_4459); // "RFSB_RDY"
    dispatch_loop()
}

fn dispatch_loop() -> ! {
    loop {
        let mut req = Msg::zeroed();
        if !recv_msg(u64::MAX, &mut req) { continue; }

        let from = req.sender;

        match req.data[0] {
            OP_READDIR => {
                let path = unpack_path(&req.data[2..8]);
                handle_readdir(from, &path);
            }
            OP_READ => {
                let offset = req.data[1];
                let path   = unpack_path(&req.data[2..8]);
                handle_read(from, offset, &path);
            }
            OP_STAT => {
                let path = unpack_path(&req.data[2..8]);
                handle_stat(from, &path);
            }
            OP_MOUNT => handle_mount(from),
            _ => {
                let mut r = Msg::zeroed();
                r.data[0] = RESP_ERROR;
                r.data[1] = ENOSYS;
                send_msg(from as u64, &r);
            }
        }
    }
}

// ── OP_READDIR ────────────────────────────────────────────────────────────────

fn handle_readdir(from: u32, path: &[u8]) {
    // Empty path or "/" → list root.
    let target_path = if fs::trim_null(path).is_empty() { b"/" as &[u8] } else { path };

    let node = match fs::lookup(target_path) {
        Some(n) => n,
        None => {
            send_error(from, ENOENT);
            return;
        }
    };

    match &node.kind {
        fs::NodeType::File { .. } => {
            send_error(from, ENOTDIR);
        }
        fs::NodeType::Dir { children } => {
            for entry in *children {
                let mut r = Msg::zeroed();
                r.data[0] = RESP_ENTRY;
                r.data[1] = entry.node.flags();
                r.data[2] = entry.node.size();
                pack_bytes_into(&mut r.data[3..8], entry.name);
                send_msg(from as u64, &r);
            }
            send_done(from);
        }
    }
}

// ── OP_READ ───────────────────────────────────────────────────────────────────

fn handle_read(from: u32, offset: u64, path: &[u8]) {
    let node = match fs::lookup(path) {
        Some(n) => n,
        None => { send_error(from, ENOENT); return; }
    };

    let data = match &node.kind {
        fs::NodeType::File { data, .. } => *data,
        fs::NodeType::Dir  { .. }       => { send_error(from, EISDIR); return; }
    };

    let off = offset as usize;
    if off >= data.len() {
        send_done(from);
        return;
    }

    let end   = core::cmp::min(off + CHUNK_SIZE, data.len());
    let chunk = &data[off..end];

    let mut r = Msg::zeroed();
    r.data[0] = RESP_DATA;
    r.data[1] = data.len() as u64;
    r.data[2] = chunk.len() as u64;
    pack_bytes_into(&mut r.data[3..8], chunk);
    send_msg(from as u64, &r);
}

// ── OP_STAT ───────────────────────────────────────────────────────────────────

fn handle_stat(from: u32, path: &[u8]) {
    match fs::lookup(path) {
        None => send_error(from, ENOENT),
        Some(node) => {
            let mut r = Msg::zeroed();
            r.data[0] = RESP_STAT;
            r.data[1] = node.flags();
            r.data[2] = node.size();
            send_msg(from as u64, &r);
        }
    }
}

// ── OP_MOUNT ──────────────────────────────────────────────────────────────────

fn handle_mount(from: u32) {
    for m in MOUNTS {
        let mut r = Msg::zeroed();
        r.data[0] = RESP_MOUNT;
        r.data[1] = 0;
        pack_bytes_into(&mut r.data[2..6], m.path);   // 32-byte mount path
        pack_bytes_into(&mut r.data[6..8], m.fstype); // 16-byte fs type
        send_msg(from as u64, &r);
    }
    send_done(from);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn send_done(to: u32) {
    let mut r = Msg::zeroed();
    r.data[0] = RESP_DONE;
    send_msg(to as u64, &r);
}

fn send_error(to: u32, errno: u64) {
    let mut r = Msg::zeroed();
    r.data[0] = RESP_ERROR;
    r.data[1] = errno;
    send_msg(to as u64, &r);
}

/// Pack `src` bytes little-endian into `words` (each word holds 8 bytes).
fn pack_bytes_into(words: &mut [u64], src: &[u8]) {
    for (i, &b) in src.iter().enumerate() {
        let wi = i / 8;
        let bi = i % 8;
        if wi < words.len() {
            words[wi] |= (b as u64) << (bi * 8);
        }
    }
}

/// Reconstruct a 48-byte path buffer from 6 little-endian-packed u64 words.
fn unpack_path(words: &[u64]) -> [u8; 48] {
    let mut buf = [0u8; 48];
    for (wi, &w) in words.iter().enumerate().take(6) {
        for bi in 0..8 {
            buf[wi * 8 + bi] = (w >> (bi * 8)) as u8;
        }
    }
    buf
}

// ── Panic ─────────────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall::notify(1, 0x5246_5342_5f45_5252); // "RFSB_ERR"
    syscall::exit(1);
}
