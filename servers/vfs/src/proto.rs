//! VFS IPC protocol — opcodes and response codes.
//!
//! All paths are packed little-endian into `msg.data[2..8]` (6 × u64 = 48 bytes,
//! null-terminated).  Byte 0 of the path is in the LSB of data[2].
//!
//! # Request messages  (client → vfs)
//!
//! ## OP_READDIR — list a directory
//! ```text
//! data[0] = OP_READDIR
//! data[1] = 0 (reserved)
//! data[2..8] = path  (e.g. b"/etc\0")
//! ```
//! VFS replies: zero or more RESP_ENTRY, then RESP_DONE.
//! RESP_ERROR(ENOTDIR) if path names a file; RESP_ERROR(ENOENT) if missing.
//!
//! ## OP_READ — read a chunk of a file
//! ```text
//! data[0] = OP_READ
//! data[1] = byte_offset  (0 for first request)
//! data[2..8] = path
//! ```
//! VFS replies: one RESP_DATA per call until the end of file, then RESP_DONE.
//! Shell loops: send OP_READ with increasing offset until offset ≥ total_size.
//! RESP_ERROR(ENOENT) if path not found; RESP_ERROR(EISDIR) for a directory.
//!
//! ## OP_STAT — stat a path
//! ```text
//! data[0] = OP_STAT
//! data[1] = 0
//! data[2..8] = path
//! ```
//! VFS replies with RESP_STAT or RESP_ERROR.
//!
//! ## OP_MOUNT — query the mount table
//! ```text
//! data[0] = OP_MOUNT
//! ```
//! VFS replies: zero or more RESP_MOUNT, then RESP_DONE.
//!
//! # Response messages  (vfs → client)
//!
//! ## RESP_ENTRY — one directory entry (reply to OP_READDIR)
//! ```text
//! data[0] = RESP_ENTRY
//! data[1] = flags    (bit 0 = is_dir, bit 1 = executable)
//! data[2] = size     (bytes for file, child count for dir)
//! data[3..8] = name  (40 bytes, null-terminated, little-endian packed)
//! ```
//!
//! ## RESP_DONE — end of listing or end of file
//! ```text
//! data[0] = RESP_DONE
//! ```
//!
//! ## RESP_DATA — one file data chunk (reply to OP_READ)
//! ```text
//! data[0] = RESP_DATA
//! data[1] = total file size
//! data[2] = bytes in this chunk  (1–40)
//! data[3..8] = up to 40 bytes of raw file data (little-endian packed)
//! ```
//!
//! ## RESP_STAT — path metadata (reply to OP_STAT)
//! ```text
//! data[0] = RESP_STAT
//! data[1] = flags  (bit 0 = is_dir, bit 1 = executable)
//! data[2] = size
//! ```
//!
//! ## RESP_MOUNT — one mount point (reply to OP_MOUNT)
//! ```text
//! data[0] = RESP_MOUNT
//! data[1] = flags  (currently 0)
//! data[2..5] = mount path  (32 bytes)
//! data[6..8] = fs type     (16 bytes, e.g. "ramfs\0")
//! ```
//!
//! ## RESP_ERROR — operation failed
//! ```text
//! data[0] = RESP_ERROR
//! data[1] = errno  (1=ENOENT, 2=ENOTDIR, 3=EISDIR, 4=ENOSYS)
//! ```

// ── Opcodes ───────────────────────────────────────────────────────────────────
pub const OP_READDIR: u64 = 0x20;
pub const OP_READ:    u64 = 0x22;
pub const OP_STAT:    u64 = 0x23;
pub const OP_MOUNT:   u64 = 0x24;

// Legacy alias kept for any code still sending the old opcode.
pub const OP_LIST: u64 = OP_READDIR;

// ── Responses ─────────────────────────────────────────────────────────────────
pub const RESP_ENTRY: u64 = 0x80;
pub const RESP_DONE:  u64 = 0x81;
pub const RESP_DATA:  u64 = 0x82;
pub const RESP_STAT:  u64 = 0x83;
pub const RESP_MOUNT: u64 = 0x84;
pub const RESP_ERROR: u64 = 0x8F;

// ── errno values ──────────────────────────────────────────────────────────────
pub const ENOENT:  u64 = 1; // no such file or directory
pub const ENOTDIR: u64 = 2; // not a directory
pub const EISDIR:  u64 = 3; // is a directory
pub const ENOSYS:  u64 = 4; // unsupported operation

// ── Sizes ─────────────────────────────────────────────────────────────────────
/// Bytes of path packed per IPC message (data[2..8] = 6 words × 8 bytes).
pub const PATH_BYTES:  usize = 48;
/// Maximum file-data bytes per RESP_DATA (data[3..8] = 5 words × 8 bytes).
pub const CHUNK_SIZE:  usize = 40;
/// Name bytes in RESP_ENTRY (data[3..8] = 5 words × 8 bytes).
pub const NAME_BYTES:  usize = 40;
