//! FAT32 filesystem parser for the VFS server.
//!
//! Reads from the block-device layer (`blk::read_sector`).  If no block
//! driver is available `available()` returns `false` and every other public
//! function is a no-op / returns `None` / `false`.
//!
//! # Limitations
//! - Sector size must be 512 bytes.
//! - Long file names (LFN) are supported: up to 255 ASCII characters per
//!   entry.  Non-ASCII UTF-16 code-points are replaced with `_`.
//! - The FAT table is cached one sector at a time; cluster data is also
//!   cached one sector at a time.  Both caches live in BSS (~1 KB total).
//! - Path components are matched case-insensitively (ASCII fold only).
//! - Hidden sectors in the BPB are ignored; the VBR is assumed to be at
//!   LBA 0 of the block device (i.e. we read from the start of a partition).
//!
//! # Storage layout
//! ```text
//! BSS: Fat32State  ≈ 1 076 bytes
//! ```

use crate::blk;

// ── FAT32 directory entry attribute bits ──────────────────────────────────────
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_LFN:       u8 = 0x0F; // read-only | hidden | system | volume_id

// End-of-chain threshold (FAT32; bottom 28 bits)
const EOC: u32 = 0x0FFF_FFF8;

// ── State ─────────────────────────────────────────────────────────────────────

struct Fat32State {
    valid:     bool,
    spc:       u8,    // sectors_per_cluster (power of 2, ≥ 1)
    fat_lba:   u32,   // first LBA of FAT1
    data_lba:  u32,   // first LBA of data area (cluster 2)
    root_clus: u32,   // first cluster of root directory
    // one-sector FAT cache
    fat_clba:  u32,
    fat_cache: [u8; 512],
    // one-sector general cache (cluster data / directory)
    sec_clba:  u32,
    sec_cache: [u8; 512],
}

impl Fat32State {
    const fn uninit() -> Self {
        Fat32State {
            valid: false, spc: 1,
            fat_lba: 0, data_lba: 0, root_clus: 2,
            fat_clba: u32::MAX, fat_cache: [0u8; 512],
            sec_clba: u32::MAX, sec_cache: [0u8; 512],
        }
    }
}

static mut STATE: Fat32State = Fat32State::uninit();

// ── Public types ──────────────────────────────────────────────────────────────

pub struct FileInfo {
    pub first_cluster: u32,
    pub size:          u32,
    pub is_dir:        bool,
}

// ── Init ──────────────────────────────────────────────────────────────────────

/// Returns `true` when a FAT32 volume is reachable via the block driver.
///
/// Parses the BPB on the first call and caches the result.
pub fn available() -> bool {
    // Fast path: already parsed
    if unsafe { (*core::ptr::addr_of!(STATE)).valid } { return true; }
    if !blk::available() { return false; }

    let mut vbr = [0u8; 512];
    if !blk::read_sector(0, &mut vbr) { return false; }

    // Jump-boot signature check
    if vbr[0] != 0xEB && vbr[0] != 0xE9 { return false; }

    let bps = u16::from_le_bytes([vbr[11], vbr[12]]) as u32;
    if bps != 512 { return false; }   // only 512-byte sectors supported

    let spc      = vbr[13];
    if spc == 0  { return false; }

    let reserved = u16::from_le_bytes([vbr[14], vbr[15]]) as u32;
    let num_fats = vbr[16] as u32;
    if num_fats == 0 { return false; }

    let fat_size = u32::from_le_bytes([vbr[36], vbr[37], vbr[38], vbr[39]]);
    if fat_size == 0 { return false; }

    let root_clus = u32::from_le_bytes([vbr[44], vbr[45], vbr[46], vbr[47]]);

    // Extended boot signature: must be 0x28 or 0x29 for FAT32
    if vbr[66] != 0x28 && vbr[66] != 0x29 { return false; }

    let fat_lba  = reserved;
    let data_lba = reserved + num_fats * fat_size;

    unsafe {
        let s = &mut *core::ptr::addr_of_mut!(STATE);
        s.spc       = spc;
        s.fat_lba   = fat_lba;
        s.data_lba  = data_lba;
        s.root_clus = root_clus;
        s.valid     = true;
    }
    true
}

// ── Internal sector helpers ───────────────────────────────────────────────────

/// Read sector `lba` into `STATE.sec_cache` (cached).
fn read_sec(lba: u32) -> bool {
    let cached_lba = unsafe { (*core::ptr::addr_of!(STATE)).sec_clba };
    if cached_lba == lba { return true; }
    let buf = unsafe { &mut (*core::ptr::addr_of_mut!(STATE)).sec_cache };
    if blk::read_sector(lba, buf) {
        unsafe { (*core::ptr::addr_of_mut!(STATE)).sec_clba = lba; }
        true
    } else {
        false
    }
}

/// Return the FAT32 entry for cluster `c` (next cluster in chain, 28-bit).
fn fat_next(c: u32) -> u32 {
    let (fat_lba, cached_fat_lba) = unsafe {
        let s = &*core::ptr::addr_of!(STATE);
        (s.fat_lba, s.fat_clba)
    };
    let byte_off  = (c as usize) * 4;
    let sec_off   = byte_off / 512;
    let byte_in_s = byte_off % 512;
    let sec_lba   = fat_lba + sec_off as u32;

    if cached_fat_lba != sec_lba {
        let buf = unsafe { &mut (*core::ptr::addr_of_mut!(STATE)).fat_cache };
        if !blk::read_sector(sec_lba, buf) { return u32::MAX; }
        unsafe { (*core::ptr::addr_of_mut!(STATE)).fat_clba = sec_lba; }
    }

    let b = unsafe {
        let s = &*core::ptr::addr_of!(STATE);
        [s.fat_cache[byte_in_s],
         s.fat_cache[byte_in_s + 1],
         s.fat_cache[byte_in_s + 2],
         s.fat_cache[byte_in_s + 3]]
    };
    u32::from_le_bytes(b) & 0x0FFF_FFFF
}

/// Convert cluster number to its first LBA.
fn clus_to_lba(c: u32) -> u32 {
    let (data_lba, spc) = unsafe {
        let s = &*core::ptr::addr_of!(STATE);
        (s.data_lba, s.spc as u32)
    };
    data_lba + (c - 2) * spc
}

// ── Directory scanning ────────────────────────────────────────────────────────

/// Scan all directory entries in the cluster chain rooted at `start_clus`.
///
/// `cb` is invoked for every valid non-dot entry:
/// `cb(name, is_dir, file_size, first_cluster)`.
fn scan_dir<F>(start_clus: u32, cb: &mut F)
where
    F: FnMut(&[u8], bool, u32, u32),
{
    let spc = unsafe { (*core::ptr::addr_of!(STATE)).spc as u32 };

    // LFN accumulator (256 bytes on stack, cleared at start of each LFN sequence)
    let mut lfn_buf:  [u8; 256] = [0u8; 256];
    let mut lfn_have: bool = false;

    let mut clus = start_clus;
    'chain: loop {
        if clus < 2 || clus >= EOC { break; }

        let base_lba = clus_to_lba(clus);

        for sec_idx in 0..spc {
            let lba = base_lba + sec_idx;
            if !read_sec(lba) { break 'chain; }

            // Copy sector to stack so we can call fat_next later without
            // conflicting with STATE.sec_cache.
            let sec: [u8; 512] = unsafe { (*core::ptr::addr_of!(STATE)).sec_cache };

            for ent in 0..16usize {   // 16 × 32-byte entries per 512-byte sector
                let e = &sec[ent * 32..(ent + 1) * 32];

                match e[0] {
                    0x00 => break 'chain,  // end of directory
                    0xE5 => { lfn_have = false; continue; } // deleted
                    _ => {}
                }

                let attr = e[11];

                // ── LFN entry ──────────────────────────────────────────────
                if attr == ATTR_LFN {
                    let seq_raw = e[0];
                    let seq     = (seq_raw & 0x1F) as usize;
                    let is_last = seq_raw & 0x40 != 0;

                    if is_last {
                        lfn_buf  = [0u8; 256];
                        lfn_have = true;
                    } else if !lfn_have {
                        continue; // orphaned entry
                    }

                    if seq >= 1 && seq <= 20 {
                        let base = (seq - 1) * 13;
                        // UTF-16LE char slots in LFN entry: 5 at +1, 6 at +14, 2 at +28
                        let slices: [(usize, usize); 3] = [(1, 5), (14, 6), (28, 2)];
                        let mut ci = 0usize;
                        'chars: for (off, count) in slices {
                            for j in 0..count {
                                let lo = e[off + j * 2];
                                let hi = e[off + j * 2 + 1];
                                if lo == 0x00 && hi == 0x00 { break 'chars; }
                                if lo == 0xFF && hi == 0xFF { break 'chars; }
                                let d = base + ci;
                                if d < 255 {
                                    lfn_buf[d] = if lo >= 0x20 && lo.is_ascii() { lo } else { b'_' };
                                }
                                ci += 1;
                            }
                        }
                    }
                    continue;
                }

                // ── Regular / directory entry ──────────────────────────────

                // Skip volume label (but not dirs)
                if attr & ATTR_VOLUME_ID != 0 && attr & ATTR_DIRECTORY == 0 {
                    lfn_have = false;
                    continue;
                }
                // Skip . and ..
                if e[0] == b'.' { lfn_have = false; continue; }

                let is_dir = attr & ATTR_DIRECTORY != 0;
                let size   = u32::from_le_bytes([e[28], e[29], e[30], e[31]]);
                let c_hi   = u16::from_le_bytes([e[20], e[21]]) as u32;
                let c_lo   = u16::from_le_bytes([e[26], e[27]]) as u32;
                let fc     = (c_hi << 16) | c_lo;

                // ── Determine name ─────────────────────────────────────────
                if lfn_have {
                    // Find LFN length (first null byte in lfn_buf)
                    let mut lfn_len = 0;
                    while lfn_len < 255 && lfn_buf[lfn_len] != 0 { lfn_len += 1; }
                    lfn_have = false;
                    if lfn_len > 0 {
                        cb(&lfn_buf[..lfn_len], is_dir, size, fc);
                        continue;
                    }
                    // Fall through to 8.3 if LFN was empty
                }

                // Build 8.3 name: up to 8 name chars + '.' + 3 ext chars
                let mut sfn = [0u8; 13];
                let mut p   = 0usize;
                // Name part: strip trailing spaces
                let mut nend = 8;
                while nend > 0 && e[nend - 1] == b' ' { nend -= 1; }
                for i in 0..nend {
                    sfn[p] = e[i].to_ascii_lowercase();
                    p += 1;
                }
                // Extension part: strip trailing spaces
                let mut xend = 3;
                while xend > 0 && e[8 + xend - 1] == b' ' { xend -= 1; }
                if xend > 0 {
                    sfn[p] = b'.';
                    p += 1;
                    for i in 0..xend {
                        sfn[p] = e[8 + i].to_ascii_lowercase();
                        p += 1;
                    }
                }
                if p > 0 {
                    cb(&sfn[..p], is_dir, size, fc);
                }
            }
        }

        clus = fat_next(clus);
    }
}

// ── Path helpers ──────────────────────────────────────────────────────────────

/// Strip leading '/' and trailing NUL bytes.
fn trim_path(p: &[u8]) -> &[u8] {
    let p = if p.first() == Some(&b'/') { &p[1..] } else { p };
    let mut end = p.len();
    for (i, &b) in p.iter().enumerate() {
        if b == 0 { end = i; break; }
    }
    &p[..end]
}

/// Split at the first '/' giving `(component, rest)`.
/// If no '/' is present, `rest` is empty.
fn split_component(p: &[u8]) -> (&[u8], &[u8]) {
    let mut i = 0;
    while i < p.len() && p[i] != b'/' { i += 1; }
    (&p[..i], if i < p.len() { &p[i + 1..] } else { b"" })
}

/// Case-insensitive ASCII comparison.
fn eq_ci(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|(x, y)| x.to_ascii_lowercase() == y.to_ascii_lowercase())
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Walk a VFS path (e.g. `/bin/hello`) through the FAT32 tree.
///
/// Returns `None` if the path does not exist or FAT32 is unavailable.
pub fn lookup(path: &[u8]) -> Option<FileInfo> {
    if !available() { return None; }

    let path = trim_path(path);

    if path.is_empty() {
        // Root directory
        let rc = unsafe { (*core::ptr::addr_of!(STATE)).root_clus };
        return Some(FileInfo { first_cluster: rc, size: 0, is_dir: true });
    }

    let rc = unsafe { (*core::ptr::addr_of!(STATE)).root_clus };
    let mut cur_clus = rc;
    let mut remaining = path;

    loop {
        let (comp, rest) = split_component(remaining);
        if comp.is_empty() { break; }

        let mut found: Option<FileInfo> = None;
        scan_dir(cur_clus, &mut |name, is_dir, size, fc| {
            if found.is_none() && eq_ci(name, comp) {
                found = Some(FileInfo { first_cluster: fc, size, is_dir });
            }
        });

        match found {
            None => return None,
            Some(fi) => {
                if rest.is_empty() {
                    return Some(fi);
                } else if fi.is_dir {
                    cur_clus  = fi.first_cluster;
                    remaining = rest;
                } else {
                    return None; // non-dir in the middle of a path
                }
            }
        }
    }

    None
}

/// Enumerate the directory at `path`, calling `cb(name, is_dir, size)` for
/// each entry.  Returns `true` on success, `false` if unavailable or not a dir.
pub fn readdir<F>(path: &[u8], mut cb: F) -> bool
where
    F: FnMut(&[u8], bool, u32),
{
    if !available() { return false; }

    let dir_clus = if trim_path(path).is_empty() {
        unsafe { (*core::ptr::addr_of!(STATE)).root_clus }
    } else {
        match lookup(path) {
            Some(fi) if fi.is_dir => fi.first_cluster,
            _                     => return false,
        }
    };

    scan_dir(dir_clus, &mut |name, is_dir, size, _| cb(name, is_dir, size));
    true
}

/// Read up to 40 bytes of file `path` at `offset` into `out`.
///
/// Returns `Some((file_size, bytes_copied))`.
/// Returns `None` if unavailable, path not found, or is a directory.
/// Returns `Some((size, 0))` if `offset ≥ file_size`.
pub fn read_file(path: &[u8], offset: u32, out: &mut [u8; 40]) -> Option<(u32, u32)> {
    if !available() { return None; }

    let fi = lookup(path)?;
    if fi.is_dir { return None; }
    if offset >= fi.size { return Some((fi.size, 0)); }

    let spc = unsafe { (*core::ptr::addr_of!(STATE)).spc as u32 };
    let bytes_per_clus = spc * 512;

    // Walk cluster chain to the cluster containing `offset`
    let clus_idx = offset / bytes_per_clus;
    let byte_in_clus = offset % bytes_per_clus;

    let mut clus = fi.first_cluster;
    for _ in 0..clus_idx {
        let next = fat_next(clus);
        if next >= EOC || next < 2 { return Some((fi.size, 0)); }
        clus = next;
    }

    let sec_in_clus = byte_in_clus / 512;
    let byte_in_sec = (byte_in_clus % 512) as usize;
    let lba = clus_to_lba(clus) + sec_in_clus;

    if !read_sec(lba) { return None; }

    let remaining_file = (fi.size - offset) as usize;
    let remaining_sec  = 512 - byte_in_sec;
    let to_copy = 40.min(remaining_file).min(remaining_sec);

    let sec = unsafe { (*core::ptr::addr_of!(STATE)).sec_cache };
    out[..to_copy].copy_from_slice(&sec[byte_in_sec..byte_in_sec + to_copy]);

    Some((fi.size, to_copy as u32))
}
