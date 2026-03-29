//! Mutable RAM-disk overlay for the VFS server.
//!
//! Provides a fixed-size, heap-free layer that sits *on top of* the read-only
//! static tree in `fs.rs`.  Mutable entries shadow same-path static entries in
//! `READ` and `STAT` lookups; new entries appear as additional directory
//! children in `READDIR`.
//!
//! # Limits (BSS cost)
//! - `MAX_NODES = 32`  mutable entries (files + dirs)
//! - `MAX_DATA  = 4096` bytes per file
//! - Total BSS: 32 × (64 + 4096 + 4) ≈ 131 KB

/// Maximum number of mutable entries (files + directories).
pub const MAX_NODES: usize = 32;

/// Maximum data bytes per mutable file.
pub const MAX_DATA: usize = 4096;

/// Maximum path length in bytes (including leading `/`).
pub const MAX_PATH: usize = 64;

/// Flag bit: this entry is a directory.
pub const FLAG_DIR: u8 = 0x01;

/// Flag bit: this entry is executable.
pub const FLAG_EXE: u8 = 0x02;

// ── Node ──────────────────────────────────────────────────────────────────────

#[derive(Copy, Clone)]
pub struct MutNode {
    path:     [u8; MAX_PATH],
    plen:     u8,
    pub flags: u8,
    /// Valid bytes in `data`; 0 for directories.
    pub dlen:  u16,
    pub used:  bool,
    pub data:  [u8; MAX_DATA],
}

impl MutNode {
    const fn empty() -> Self {
        MutNode {
            path:  [0; MAX_PATH],
            plen:  0,
            flags: 0,
            dlen:  0,
            used:  false,
            data:  [0; MAX_DATA],
        }
    }

    /// Absolute path bytes (no null padding).
    pub fn path_bytes(&self) -> &[u8] {
        &self.path[..self.plen as usize]
    }

    pub fn is_dir(&self) -> bool { self.flags & FLAG_DIR != 0 }
    pub fn is_exe(&self) -> bool { self.flags & FLAG_EXE != 0 }
}

// ── Storage ───────────────────────────────────────────────────────────────────

static mut NODES: [MutNode; MAX_NODES] = {
    const E: MutNode = MutNode::empty();
    [E; MAX_NODES]
};

// ── Public API ────────────────────────────────────────────────────────────────

/// Look up a mutable node by canonical absolute path.
/// Returns the slot index, or `None` if not found.
pub fn find(path: &[u8]) -> Option<usize> {
    let p = canon(trim_null(path));
    unsafe {
        for i in 0..MAX_NODES {
            if !NODES[i].used { continue; }
            if canon(NODES[i].path_bytes()) == p {
                return Some(i);
            }
        }
    }
    None
}

/// Allocate (or reuse) a mutable node at `path` with `flags`.
///
/// If a node with the same canonical path already exists, returns its index
/// unchanged (existing flags and data are preserved).
/// Returns `None` if the path is too long or the table is full.
pub fn alloc(path: &[u8], flags: u8) -> Option<usize> {
    // Reuse existing slot.
    if let Some(i) = find(path) { return Some(i); }

    let p = canon(trim_null(path));
    let plen = p.len();
    if plen == 0 || plen > MAX_PATH { return None; }

    unsafe {
        for i in 0..MAX_NODES {
            if !NODES[i].used {
                NODES[i] = MutNode::empty();
                NODES[i].path[..plen].copy_from_slice(p);
                NODES[i].plen  = plen as u8;
                NODES[i].flags = flags;
                NODES[i].used  = true;
                return Some(i);
            }
        }
    }
    None // table full
}

/// Remove the mutable node at `path`.
/// Returns `true` if found and removed, `false` if not in the mutable layer.
pub fn remove(path: &[u8]) -> bool {
    if let Some(i) = find(path) {
        unsafe { NODES[i].used = false; }
        true
    } else {
        false
    }
}

/// Write `src` bytes at `offset` within node `idx`.
///
/// Extends `dlen` as needed.  Returns `false` if the write would exceed
/// `MAX_DATA`.
pub fn write_chunk(idx: usize, offset: usize, src: &[u8]) -> bool {
    let end = match offset.checked_add(src.len()) {
        Some(e) if e <= MAX_DATA => e,
        _ => return false,
    };
    unsafe {
        NODES[idx].data[offset..end].copy_from_slice(src);
        if end > NODES[idx].dlen as usize {
            NODES[idx].dlen = end as u16;
        }
    }
    true
}

/// Truncate node `idx` to zero length (leaves flags and path intact).
pub fn truncate(idx: usize) {
    if idx < MAX_NODES {
        unsafe { NODES[idx].dlen = 0; }
    }
}

/// Return a shared reference to node `idx`.
pub fn get(idx: usize) -> Option<&'static MutNode> {
    if idx >= MAX_NODES { return None; }
    unsafe {
        if NODES[idx].used {
            Some(&*core::ptr::addr_of!(NODES[idx]))
        } else {
            None
        }
    }
}

/// Collect all mutable nodes that are *direct* children of `parent_path`.
///
/// Results are written into `names_out` and `idxs_out`.  Returns the number
/// of entries written (≤ `names_out.len()`).
///
/// Each entry in `names_out` is the child's name component (last path segment),
/// null-terminated and padded with zeroes to 48 bytes.
pub fn children_of(
    parent: &[u8],
    names_out: &mut [[u8; 48]],
    idxs_out:  &mut [usize],
) -> usize {
    let parent = canon(trim_null(parent));
    let cap = names_out.len().min(idxs_out.len());
    let mut count = 0;
    unsafe {
        for i in 0..MAX_NODES {
            if count >= cap { break; }
            if !NODES[i].used { continue; }
            let np = canon(NODES[i].path_bytes());
            if let Some(name) = direct_child_name(np, parent) {
                names_out[count] = [0u8; 48];
                let nlen = name.len().min(48);
                names_out[count][..nlen].copy_from_slice(&name[..nlen]);
                idxs_out[count] = i;
                count += 1;
            }
        }
    }
    count
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Strip trailing null bytes from a slice.
pub fn trim_null(s: &[u8]) -> &[u8] {
    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    &s[..end]
}

/// Canonical path: strip trailing `/` (except bare `"/"`).
fn canon(p: &[u8]) -> &[u8] {
    if p.len() > 1 && p.last() == Some(&b'/') { &p[..p.len() - 1] } else { p }
}

/// If `path` is a *direct* child of `parent`, return the child's name component.
///
/// ```text
/// direct_child_name(b"/bin/ls", b"/bin") = Some(b"ls")
/// direct_child_name(b"/bin",    b"/")    = Some(b"bin")
/// direct_child_name(b"/a/b/c",  b"/a")  = None   (not direct)
/// ```
fn direct_child_name<'a>(path: &'a [u8], parent: &[u8]) -> Option<&'a [u8]> {
    if parent == b"/" {
        // Root: path must be "/name" with no further slashes.
        if path.len() > 1 && path.starts_with(b"/") {
            let name = &path[1..];
            if !name.is_empty() && !name.contains(&b'/') {
                return Some(name);
            }
        }
        return None;
    }

    if !path.starts_with(parent) { return None; }
    let rest = &path[parent.len()..];
    if !rest.starts_with(b"/") { return None; }
    let name = &rest[1..];
    if !name.is_empty() && !name.contains(&b'/') {
        Some(name)
    } else {
        None
    }
}
