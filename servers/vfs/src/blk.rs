//! Block-device IPC client for the VFS server.
//!
//! Forwards sector-read requests to the "block-drv" service via
//! synchronous IPC (SYS_CALL = syscall 17).
//!
//! If no block-drv is registered, `available()` returns `false` and all
//! reads silently fail so the VFS falls back to the static ROM tree.
//!
//! # Protocol — VFS → block-drv → VFS
//!
//! A full 512-byte sector is fetched 40 bytes at a time; VFS makes up to
//! 13 synchronous round-trips (SYS_CALL) per sector.
//!
//! ```text
//! Request  (VFS → block-drv):
//!   data[0] = OP_BLK_READ  (0x50)
//!   data[1] = LBA           (u32 sector number)
//!   data[2] = byte_offset   (0, 40, 80, … 480)
//!
//! Response (block-drv → VFS, via send_msg reply):
//!   data[0] = RESP_BLK_DATA (0x90)
//!   data[1] = 512           (sector size — always 512)
//!   data[2] = chunk_len     (1–40; may be < 40 for last chunk)
//!   data[3..8] = raw sector bytes (little-endian packed, 40 B max)
//!
//! OR on error:
//!   data[0] = RESP_ERROR    (0x8F)
//!   data[1] = errno
//! ```

use crate::syscall::{self, Msg};

// ── Protocol constants ────────────────────────────────────────────────────────

pub const OP_BLK_READ:   u64 = 0x50;
pub const RESP_BLK_DATA: u64 = 0x90;

// Shared with vfs proto
const RESP_ERROR: u64 = 0x8F;

// ── Cached PID ────────────────────────────────────────────────────────────────

static mut BLKDRV_PID: u64  = u64::MAX;
static mut BLK_PROBED: bool = false;

/// Returns `true` if a "block-drv" service is reachable.
///
/// The result is cached after the first call; the cache is never invalidated
/// within a single VFS session (restarting VFS resets it).
pub fn available() -> bool {
    unsafe {
        let probed = &mut *core::ptr::addr_of_mut!(BLK_PROBED);
        let pid    = &mut *core::ptr::addr_of_mut!(BLKDRV_PID);
        if !*probed {
            *pid    = syscall::lookup(b"block-drv\0\0\0\0\0\0\0");
            *probed = true;
        }
        *pid != u64::MAX
    }
}

// ── Sector read ───────────────────────────────────────────────────────────────

/// Read one 512-byte sector at LBA `lba` into `buf`.
///
/// Makes up to 13 synchronous IPC calls to the block-drv service.
/// Returns `true` on success, `false` if block-drv is unreachable or returns
/// an error for any chunk.
pub fn read_sector(lba: u32, buf: &mut [u8; 512]) -> bool {
    if !available() { return false; }
    let blkpid = unsafe { *core::ptr::addr_of!(BLKDRV_PID) };

    let mut total = 0usize;
    while total < 512 {
        let mut msg = Msg::zeroed();
        msg.data[0] = OP_BLK_READ;
        msg.data[1] = lba as u64;
        msg.data[2] = total as u64;

        if !syscall::call(blkpid, &mut msg) { return false; }

        match msg.data[0] {
            RESP_BLK_DATA => {
                let chunk_len = (msg.data[2] as usize).min(40).min(512 - total);
                if chunk_len == 0 { return false; }
                unpack_into(&msg.data[3..8], &mut buf[total..total + chunk_len], chunk_len);
                total += chunk_len;
            }
            RESP_ERROR | _ => return false,
        }
    }
    true
}

// ── Helper ────────────────────────────────────────────────────────────────────

/// Unpack `n` bytes from little-endian-packed `words` into `out[..n]`.
fn unpack_into(words: &[u64], out: &mut [u8], n: usize) {
    for i in 0..n {
        let wi = i / 8;
        let bi = i % 8;
        if wi < words.len() {
            out[i] = (words[wi] >> (bi * 8)) as u8;
        }
    }
}
