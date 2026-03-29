//! Rost VFS Server — virtual filesystem IPC server.
//!
//! Runs as a ring-3 ELF binary, PID 4 by convention.
//! Three storage layers (priority high → low):
//!   1. `mutable.rs` — fixed-size writable overlay (32 nodes × 4 KB ≈ 131 KB BSS)
//!   2. `fat32.rs`   — FAT32 reader via block-drv IPC (optional; dormant if no block-drv)
//!   3. `fs.rs`      — compiled-in read-only directory tree (static RAM-disk)
//!
//! Two client APIs:
//!   - **Stateless** (0x20–0x29): caller supplies full path + offset on every call
//!   - **Stateful fd** (0x30–0x35): caller opens an fd; VFS tracks position
//!
//! # Supported operations
//! Stateless path API:
//!   OP_READDIR (0x20), OP_READ (0x22), OP_STAT (0x23), OP_MOUNT (0x24)
//!   OP_WRITE_OPEN (0x25), OP_WRITE_DATA (0x26), OP_WRITE_CLOSE (0x27)
//!   OP_MKDIR (0x28), OP_UNLINK (0x29)
//!
//! Stateful fd API:
//!   OP_OPEN (0x30), OP_CLOSE (0x31), OP_READ_FD (0x32),
//!   OP_WRITE_FD (0x33), OP_SEEK (0x34), OP_FSTAT (0x35)
#![no_std]
#![no_main]

mod blk;
mod fat32;
mod fd;
mod fs;
mod mutable;
mod proto;
mod syscall;

use proto::*;
use syscall::{Msg, recv_msg, send_msg};

// ── Mount table ───────────────────────────────────────────────────────────────

struct MountPoint {
    path:   &'static [u8],
    fstype: &'static [u8],
    _source: &'static [u8],
}

static MOUNTS: &[MountPoint] = &[
    MountPoint { path: b"/", fstype: b"ramfs", _source: b"ramdisk:0" },
];

// ── Legacy write session (OP_WRITE_OPEN / DATA / CLOSE) ───────────────────────

/// One-at-a-time stateful write session used by the shell `touch`/`write` cmds.
struct WriteSession {
    idx:    usize,
    offset: usize,
}

impl WriteSession {
    const fn none() -> Self { WriteSession { idx: usize::MAX, offset: 0 } }
    fn is_open(&self)  -> bool { self.idx != usize::MAX }
    fn close(&mut self)        { self.idx = usize::MAX; self.offset = 0; }
}

static mut WS: WriteSession = WriteSession::none();

// ── Entry point ───────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _start() -> ! {
    syscall::register(b"rost-vfs\0\0\0\0\0\0\0\0");
    syscall::notify(1, 0x5246_5342_5f52_4459); // "RFSB_RDY"
    dispatch_loop()
}

fn dispatch_loop() -> ! {
    loop {
        let mut req = Msg::zeroed();
        if !recv_msg(u64::MAX, &mut req) { continue; }

        let from = req.sender;

        match req.data[0] {
            // ── Stateless path API ──────────────────────────────────────────
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
            OP_WRITE_OPEN => {
                let flags = req.data[1] as u8;
                let path  = unpack_path(&req.data[2..8]);
                handle_write_open(from, flags, &path);
            }
            OP_WRITE_DATA => {
                let chunk_len = req.data[1] as usize;
                let mut bytes = [0u8; CHUNK_SIZE];
                unpack_bytes_from(&req.data[2..8], &mut bytes);
                handle_write_data(from, chunk_len, &bytes);
            }
            OP_WRITE_CLOSE => handle_write_close(from),
            OP_MKDIR => {
                let flags = req.data[1] as u8;
                let path  = unpack_path(&req.data[2..8]);
                handle_mkdir(from, flags, &path);
            }
            OP_UNLINK => {
                let path = unpack_path(&req.data[2..8]);
                handle_unlink(from, &path);
            }
            // ── Stateful fd API ─────────────────────────────────────────────
            OP_OPEN => {
                let oflags = req.data[1] as u8;
                let path   = unpack_path(&req.data[2..8]);
                handle_open(from, oflags, &path);
            }
            OP_CLOSE => {
                let fd_num = req.data[1] as u8;
                handle_close(from, fd_num);
            }
            OP_READ_FD => {
                let fd_num = req.data[1] as u8;
                handle_read_fd(from, fd_num);
            }
            OP_WRITE_FD => {
                let fd_num    = req.data[1] as u8;
                let chunk_len = req.data[2] as usize;
                let mut bytes = [0u8; CHUNK_SIZE];
                unpack_bytes_from(&req.data[3..8], &mut bytes);
                handle_write_fd(from, fd_num, chunk_len, &bytes);
            }
            OP_SEEK => {
                let fd_num     = req.data[1] as u8;
                let new_offset = req.data[2] as u32;
                handle_seek(from, fd_num, new_offset);
            }
            OP_FSTAT => {
                let fd_num = req.data[1] as u8;
                handle_fstat(from, fd_num);
            }
            _ => {
                send_error(from, ENOSYS);
            }
        }
    }
}

// ── OP_READDIR ────────────────────────────────────────────────────────────────

/// Compact set of up to 32 names (≤40 bytes each) used for layer deduplication.
struct SeenNames {
    data:  [[u8; 40]; 32],
    lens:  [u8; 32],
    count: usize,
}

impl SeenNames {
    const fn new() -> Self {
        SeenNames { data: [[0u8; 40]; 32], lens: [0; 32], count: 0 }
    }

    fn add(&mut self, name: &[u8]) {
        if self.count >= 32 { return; }
        let nl = name.len().min(40);
        self.data[self.count][..nl].copy_from_slice(&name[..nl]);
        self.lens[self.count] = nl as u8;
        self.count += 1;
    }

    /// Case-insensitive ASCII match.
    fn contains(&self, name: &[u8]) -> bool {
        let nl = name.len();
        for i in 0..self.count {
            let sl = self.lens[i] as usize;
            if sl != nl { continue; }
            if self.data[i][..sl]
                .iter()
                .zip(name.iter())
                .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
            {
                return true;
            }
        }
        false
    }
}

fn handle_readdir(from: u32, path: &[u8]) {
    let target = if fs::trim_null(path).is_empty() { b"/" as &[u8] } else { path };
    let trimmed = mutable::trim_null(target);

    // Determine whether target is a valid directory across all layers.
    let has_static_dir = match fs::lookup(target) {
        Some(n) if n.is_dir() => true,
        Some(_) => {
            // Static file — valid only if a mutable dir shadows it.
            match mutable::find(trimmed) {
                Some(i) if mutable::get(i).map_or(false, |m| m.is_dir()) => false,
                _ => { send_error(from, ENOTDIR); return; }
            }
        }
        None => {
            // Not in static tree — must be a mutable or FAT32 dir.
            let mut is_dir = false;
            if let Some(i) = mutable::find(trimmed) {
                match mutable::get(i) {
                    Some(m) if  m.is_dir() => { is_dir = true; }
                    Some(_) => { send_error(from, ENOTDIR); return; }
                    None    => {}
                }
            }
            if !is_dir {
                if fat32::available() {
                    match fat32::lookup(trimmed) {
                        Some(fi) if fi.is_dir => { is_dir = true; }
                        Some(_) => { send_error(from, ENOTDIR); return; }
                        None    => {}
                    }
                }
            }
            if !is_dir { send_error(from, ENOENT); return; }
            false
        }
    };

    // ── Collect mutable children (emitted last, highest priority) ─────────────
    let mut mut_names = [[0u8; 48]; mutable::MAX_NODES];
    let mut mut_idxs  = [0usize;   mutable::MAX_NODES];
    let mut_count = mutable::children_of(trimmed, &mut mut_names[..], &mut mut_idxs[..]);

    // Build a SeenNames from mutable children names for FAT32 dedup.
    let mut seen_by_mut = SeenNames::new();
    for i in 0..mut_count {
        let nl = { let mut l = 0; while l < 48 && mut_names[i][l] != 0 { l += 1; } l };
        seen_by_mut.add(&mut_names[i][..nl]);
    }

    // ── Emit FAT32 children (skip if shadowed by mutable) ─────────────────────
    let mut fat_seen = SeenNames::new(); // names actually emitted from FAT32
    if fat32::available() {
        fat32::readdir(trimmed, |name, is_dir, size| {
            if seen_by_mut.contains(name) { return; }
            fat_seen.add(name);
            let mut r = Msg::zeroed();
            r.data[0] = RESP_ENTRY;
            r.data[1] = if is_dir { 1u64 } else { 0u64 };
            r.data[2] = size as u64;
            pack_bytes_into(&mut r.data[3..8], name);
            send_msg(from as u64, &r);
        });
    }

    // ── Emit static children (skip if shadowed by mutable OR FAT32) ───────────
    if has_static_dir {
        if let Some(node) = fs::lookup(target) {
            if let fs::NodeType::Dir { children } = &node.kind {
                for entry in *children {
                    // Mutable shadow check via full path
                    let mut cpbuf = [0u8; 64];
                    let cplen = build_child_path(&mut cpbuf, target, entry.name);
                    if mutable::find(&cpbuf[..cplen]).is_some() { continue; }

                    // FAT32 shadow check by name only (case-insensitive)
                    let ename = fs::trim_null(entry.name);
                    if fat_seen.contains(ename) { continue; }

                    let mut r = Msg::zeroed();
                    r.data[0] = RESP_ENTRY;
                    r.data[1] = entry.node.flags();
                    r.data[2] = entry.node.size();
                    pack_bytes_into(&mut r.data[3..8], entry.name);
                    send_msg(from as u64, &r);
                }
            }
        }
    }

    // ── Emit mutable children ─────────────────────────────────────────────────
    for c in 0..mut_count {
        if let Some(mn) = mutable::get(mut_idxs[c]) {
            let mut r = Msg::zeroed();
            r.data[0] = RESP_ENTRY;
            r.data[1] = mn.flags as u64;
            r.data[2] = mn.dlen  as u64;
            pack_bytes_into(&mut r.data[3..8], &mut_names[c][..NAME_BYTES]);
            send_msg(from as u64, &r);
        }
    }

    send_done(from);
}

// ── OP_READ ───────────────────────────────────────────────────────────────────

fn handle_read(from: u32, offset: u64, path: &[u8]) {
    let trimmed = mutable::trim_null(path);

    // Mutable layer takes priority.
    if let Some(idx) = mutable::find(trimmed) {
        if let Some(mn) = mutable::get(idx) {
            if mn.is_dir() { send_error(from, EISDIR); return; }
            let data = &mn.data[..mn.dlen as usize];
            let off  = offset as usize;
            if off >= data.len() { send_done(from); return; }
            let end   = (off + CHUNK_SIZE).min(data.len());
            let chunk = &data[off..end];
            let mut r = Msg::zeroed();
            r.data[0] = RESP_DATA;
            r.data[1] = data.len() as u64;
            r.data[2] = chunk.len() as u64;
            pack_bytes_into(&mut r.data[3..8], chunk);
            send_msg(from as u64, &r);
            return;
        }
    }

    // FAT32 layer.
    if fat32::available() {
        let mut buf = [0u8; CHUNK_SIZE];
        if let Some((fsize, n)) = fat32::read_file(trimmed, offset as u32, &mut buf) {
            if n == 0 {
                send_done(from);
            } else {
                let mut r = Msg::zeroed();
                r.data[0] = RESP_DATA;
                r.data[1] = fsize as u64;
                r.data[2] = n as u64;
                pack_bytes_into(&mut r.data[3..8], &buf[..n as usize]);
                send_msg(from as u64, &r);
            }
            return;
        }
        // fat32::read_file returning None means "not found in FAT32"; fall through.
    }

    // Fall back to static tree.
    let node = match fs::lookup(path) {
        Some(n) => n,
        None => { send_error(from, ENOENT); return; }
    };
    let data = match &node.kind {
        fs::NodeType::File { data, .. } => *data,
        fs::NodeType::Dir  { .. }       => { send_error(from, EISDIR); return; }
    };
    let off = offset as usize;
    if off >= data.len() { send_done(from); return; }
    let end   = (off + CHUNK_SIZE).min(data.len());
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
    let trimmed = mutable::trim_null(path);
    if let Some(idx) = mutable::find(trimmed) {
        if let Some(mn) = mutable::get(idx) {
            let mut r = Msg::zeroed();
            r.data[0] = RESP_STAT;
            r.data[1] = mn.flags as u64;
            r.data[2] = mn.dlen  as u64;
            send_msg(from as u64, &r);
            return;
        }
    }
    // FAT32 layer.
    if fat32::available() {
        if let Some(fi) = fat32::lookup(trimmed) {
            let mut r = Msg::zeroed();
            r.data[0] = RESP_STAT;
            r.data[1] = if fi.is_dir { 1 } else { 0 };
            r.data[2] = fi.size as u64;
            send_msg(from as u64, &r);
            return;
        }
    }

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
        pack_bytes_into(&mut r.data[2..6], m.path);    // 32-byte mount path
        pack_bytes_into(&mut r.data[6..8], m.fstype);  // 16-byte fs type
        send_msg(from as u64, &r);
    }
    send_done(from);
}

// ── OP_WRITE_OPEN ─────────────────────────────────────────────────────────────

fn handle_write_open(from: u32, flags: u8, path: &[u8]) {
    let trimmed = mutable::trim_null(path);

    // Disallow opening a directory path.
    if let Some(n) = fs::lookup(path) {
        if n.is_dir() { send_error(from, EISDIR); return; }
    }
    if let Some(i) = mutable::find(trimmed) {
        if let Some(mn) = mutable::get(i) {
            if mn.is_dir() { send_error(from, EISDIR); return; }
        }
    }

    let file_flags = flags & !mutable::FLAG_DIR;
    let idx = match mutable::alloc(trimmed, file_flags) {
        Some(i) => i,
        None    => { send_error(from, ENOSPC); return; }
    };
    mutable::truncate(idx);

    unsafe {
        let ws = &mut *core::ptr::addr_of_mut!(WS);
        ws.idx    = idx;
        ws.offset = 0;
    }
    send_ok(from);
}

// ── OP_WRITE_DATA ─────────────────────────────────────────────────────────────

fn handle_write_data(from: u32, chunk_len: usize, bytes: &[u8; CHUNK_SIZE]) {
    let (idx, offset) = unsafe {
        let ws = &*core::ptr::addr_of!(WS);
        if !ws.is_open() { send_error(from, EBADF); return; }
        (ws.idx, ws.offset)
    };
    let valid = chunk_len.min(CHUNK_SIZE);
    if !mutable::write_chunk(idx, offset, &bytes[..valid]) {
        send_error(from, ENOSPC);
        return;
    }
    unsafe { (&mut *core::ptr::addr_of_mut!(WS)).offset += valid; }
    send_ok(from);
}

// ── OP_WRITE_CLOSE ────────────────────────────────────────────────────────────

fn handle_write_close(from: u32) {
    let open = unsafe { (&*core::ptr::addr_of!(WS)).is_open() };
    if !open { send_error(from, EBADF); return; }
    unsafe { (&mut *core::ptr::addr_of_mut!(WS)).close(); }
    send_ok(from);
}

// ── OP_MKDIR ──────────────────────────────────────────────────────────────────

fn handle_mkdir(from: u32, _flags: u8, path: &[u8]) {
    let trimmed = mutable::trim_null(path);
    if let Some(n) = fs::lookup(path) {
        if !n.is_dir() { send_error(from, ENOTDIR); return; }
    }
    match mutable::alloc(trimmed, mutable::FLAG_DIR) {
        Some(_) => send_ok(from),
        None    => send_error(from, ENOSPC),
    }
}

// ── OP_UNLINK ─────────────────────────────────────────────────────────────────

fn handle_unlink(from: u32, path: &[u8]) {
    let trimmed = mutable::trim_null(path);
    // Close the legacy write session if it points at this node.
    unsafe {
        let ws = &mut *core::ptr::addr_of_mut!(WS);
        if ws.is_open() {
            if let Some(idx) = mutable::find(trimmed) {
                if ws.idx == idx { ws.close(); }
            }
        }
    }
    // Close any open fd pointing at this path.
    let mut fbuf = [0u8; 64];
    let flen = trimmed.len().min(64);
    fbuf[..flen].copy_from_slice(&trimmed[..flen]);
    fd::free_all_path(&fbuf[..flen]);

    if mutable::remove(trimmed) {
        send_ok(from);
    } else {
        send_error(from, ENOENT);
    }
}

// ── OP_OPEN ───────────────────────────────────────────────────────────────────

fn handle_open(from: u32, oflags: u8, path: &[u8]) {
    let trimmed = mutable::trim_null(path);
    let amode   = oflags & 0x03; // RDONLY=0, WRONLY=1, RDWR=2

    let is_write = amode == fd::O_WRONLY || amode == fd::O_RDWR;

    if is_write {
        // Writes require a mutable node.  Create one if O_CREAT is set.
        if oflags & fd::O_CREAT != 0 {
            if mutable::alloc(trimmed, 0).is_none() {
                send_error(from, ENOSPC);
                return;
            }
        } else {
            // No O_CREAT — file must exist somewhere.
            if mutable::find(trimmed).is_none() {
                match fs::lookup(path) {
                    None => { send_error(from, ENOENT); return; }
                    Some(n) if n.is_dir() => { send_error(from, EISDIR); return; }
                    Some(_) => { send_error(from, EACCES); return; } // static = immutable
                }
            }
        }
        // O_TRUNC: reset existing content.
        if oflags & fd::O_TRUNC != 0 {
            if let Some(idx) = mutable::find(trimmed) {
                mutable::truncate(idx);
            }
        }
    } else {
        // Read-only: path must resolve to something.
        if mutable::find(trimmed).is_none() {
            match fs::lookup(path) {
                None => { send_error(from, ENOENT); return; }
                Some(n) if n.is_dir() => {
                    // Opening a dir for reading is allowed (e.g., iteration).
                }
                Some(_) => {}
            }
        }
    }

    // For O_APPEND the initial offset is the current end of file.
    let init_offset: u32 = if oflags & fd::O_APPEND != 0 {
        mutable::find(trimmed)
            .and_then(mutable::get)
            .map(|mn| mn.dlen as u32)
            .unwrap_or(0)
    } else {
        0
    };

    let fd_num = match fd::alloc(from, trimmed, oflags) {
        Some(n) => n,
        None    => { send_error(from, EMFILE); return; }
    };
    if init_offset > 0 {
        fd::seek(from, fd_num, init_offset);
    }

    let mut r = Msg::zeroed();
    r.data[0] = RESP_FD;
    r.data[1] = fd_num as u64;
    send_msg(from as u64, &r);
}

// ── OP_CLOSE ──────────────────────────────────────────────────────────────────

fn handle_close(from: u32, fd_num: u8) {
    if fd::free(from, fd_num) {
        send_ok(from);
    } else {
        send_error(from, EBADF);
    }
}

// ── OP_READ_FD ────────────────────────────────────────────────────────────────

fn handle_read_fd(from: u32, fd_num: u8) {
    // Copy out the fields we need before dropping the reference, so we can
    // call fd::advance() afterwards without a borrow conflict.
    let (offset, path_buf, plen) = match fd::get(from, fd_num) {
        None => { send_error(from, EBADF); return; }
        Some(e) => {
            if !e.readable() { send_error(from, EACCES); return; }
            let mut pb = [0u8; 64];
            let pl = e.plen as usize;
            pb[..pl].copy_from_slice(e.path_bytes());
            (e.offset as usize, pb, pl)
        }
    };
    let path = &path_buf[..plen];

    // Mutable layer first.
    if let Some(idx) = mutable::find(path) {
        if let Some(mn) = mutable::get(idx) {
            if mn.is_dir() { send_error(from, EISDIR); return; }
            let data = &mn.data[..mn.dlen as usize];
            if offset >= data.len() { send_done(from); return; }
            let end   = (offset + CHUNK_SIZE).min(data.len());
            let chunk = &data[offset..end];
            let delta = chunk.len();
            let mut r = Msg::zeroed();
            r.data[0] = RESP_DATA;
            r.data[1] = data.len() as u64;
            r.data[2] = delta as u64;
            pack_bytes_into(&mut r.data[3..8], chunk);
            send_msg(from as u64, &r);
            fd::advance(from, fd_num, delta);
            return;
        }
    }

    // FAT32 layer.
    if fat32::available() {
        let mut buf = [0u8; CHUNK_SIZE];
        if let Some((fsize, n)) = fat32::read_file(path, offset as u32, &mut buf) {
            if n == 0 {
                send_done(from);
            } else {
                let n = n as usize;
                let mut r = Msg::zeroed();
                r.data[0] = RESP_DATA;
                r.data[1] = fsize as u64;
                r.data[2] = n as u64;
                pack_bytes_into(&mut r.data[3..8], &buf[..n]);
                send_msg(from as u64, &r);
                fd::advance(from, fd_num, n);
            }
            return;
        }
        // None = not in FAT32; fall through to static.
    }

    // Fall back to static tree.
    match fs::lookup(path) {
        None => { send_error(from, ENOENT); }
        Some(node) => match &node.kind {
            fs::NodeType::Dir  { .. } => { send_error(from, EISDIR); }
            fs::NodeType::File { data, .. } => {
                if offset >= data.len() { send_done(from); return; }
                let end   = (offset + CHUNK_SIZE).min(data.len());
                let chunk = &data[offset..end];
                let delta = chunk.len();
                let mut r = Msg::zeroed();
                r.data[0] = RESP_DATA;
                r.data[1] = data.len() as u64;
                r.data[2] = delta as u64;
                pack_bytes_into(&mut r.data[3..8], chunk);
                send_msg(from as u64, &r);
                fd::advance(from, fd_num, delta);
            }
        }
    }
}

// ── OP_WRITE_FD ───────────────────────────────────────────────────────────────

fn handle_write_fd(from: u32, fd_num: u8, chunk_len: usize, bytes: &[u8; CHUNK_SIZE]) {
    let (offset, path_buf, plen, is_append) = match fd::get(from, fd_num) {
        None => { send_error(from, EBADF); return; }
        Some(e) => {
            if !e.writable() { send_error(from, EACCES); return; }
            let mut pb = [0u8; 64];
            let pl = e.plen as usize;
            pb[..pl].copy_from_slice(e.path_bytes());
            (e.offset as usize, pb, pl, e.oflags & fd::O_APPEND != 0)
        }
    };
    let path = &path_buf[..plen];

    let idx = match mutable::find(path) {
        Some(i) => i,
        None    => { send_error(from, EACCES); return; } // static files are immutable
    };

    let write_at = if is_append {
        // Always append to end regardless of stored offset.
        mutable::get(idx).map_or(0, |mn| mn.dlen as usize)
    } else {
        offset
    };

    let valid = chunk_len.min(CHUNK_SIZE);
    if !mutable::write_chunk(idx, write_at, &bytes[..valid]) {
        send_error(from, ENOSPC);
        return;
    }
    fd::advance(from, fd_num, valid);
    send_ok(from);
}

// ── OP_SEEK ───────────────────────────────────────────────────────────────────

fn handle_seek(from: u32, fd_num: u8, new_offset: u32) {
    if fd::get(from, fd_num).is_none() {
        send_error(from, EBADF);
        return;
    }
    fd::seek(from, fd_num, new_offset);
    send_ok(from);
}

// ── OP_FSTAT ──────────────────────────────────────────────────────────────────

fn handle_fstat(from: u32, fd_num: u8) {
    let (path_buf, plen) = match fd::get(from, fd_num) {
        None => { send_error(from, EBADF); return; }
        Some(e) => {
            let mut pb = [0u8; 64];
            let pl = e.plen as usize;
            pb[..pl].copy_from_slice(e.path_bytes());
            (pb, pl)
        }
    };
    let path = &path_buf[..plen];

    if let Some(idx) = mutable::find(path) {
        if let Some(mn) = mutable::get(idx) {
            let mut r = Msg::zeroed();
            r.data[0] = RESP_STAT;
            r.data[1] = mn.flags as u64;
            r.data[2] = mn.dlen  as u64;
            send_msg(from as u64, &r);
            return;
        }
    }
    if fat32::available() {
        if let Some(fi) = fat32::lookup(path) {
            let mut r = Msg::zeroed();
            r.data[0] = RESP_STAT;
            r.data[1] = if fi.is_dir { 1 } else { 0 };
            r.data[2] = fi.size as u64;
            send_msg(from as u64, &r);
            return;
        }
    }

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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn send_ok(to: u32) {
    let mut r = Msg::zeroed();
    r.data[0] = RESP_OK;
    send_msg(to as u64, &r);
}

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

/// Build the full path of a child given its parent path and name.
///
/// ```text
/// build_child_path(buf, b"/etc",  b"hosts")  → b"/etc/hosts"
/// build_child_path(buf, b"/",     b"bin")    → b"/bin"
/// ```
fn build_child_path(buf: &mut [u8; 64], parent: &[u8], name: &[u8]) -> usize {
    let parent = mutable::trim_null(parent);
    let name   = fs::trim_null(name);
    if parent == b"/" {
        buf[0] = b'/';
        let nlen = name.len().min(63);
        buf[1..1 + nlen].copy_from_slice(&name[..nlen]);
        1 + nlen
    } else {
        let plen = parent.len().min(63);
        buf[..plen].copy_from_slice(&parent[..plen]);
        if plen < 63 {
            buf[plen] = b'/';
            let nlen = name.len().min(63 - plen - 1);
            buf[plen + 1..plen + 1 + nlen].copy_from_slice(&name[..nlen]);
            plen + 1 + nlen
        } else {
            plen
        }
    }
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

/// Unpack `out.len()` bytes from little-endian-packed words into `out`.
fn unpack_bytes_from(words: &[u64], out: &mut [u8]) {
    for (i, slot) in out.iter_mut().enumerate() {
        let wi = i / 8;
        let bi = i % 8;
        if wi < words.len() {
            *slot = (words[wi] >> (bi * 8)) as u8;
        }
    }
}

// ── Panic ─────────────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall::notify(1, 0x5246_5342_5f45_5252); // "RFSB_ERR"
    syscall::exit(1);
}
