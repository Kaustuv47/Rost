//! VFS IPC protocol constants.
//!
//! Shared between rost-vfs (server) and rost-shell (client).
//! Keep this file in sync with the opcodes in servers/shell/src/shell/commands.rs.
//!
//! # Request messages (shell → vfs)
//!
//! ## OP_LIST
//! List all files in the filesystem.
//! ```text
//!   data[0] = OP_LIST
//! ```
//! VFS replies with zero or more RESP_ENTRY messages, then one RESP_DONE.
//!
//! ## OP_READ
//! Read a chunk of a file starting at `byte_offset`.
//! ```text
//!   data[0] = OP_READ
//!   data[1] = byte_offset      (0 for first request)
//!   data[2] = name word 0      (bytes 0–7  of filename, little-endian)
//!   data[3] = name word 1      (bytes 8–15 of filename, null-padded)
//! ```
//! VFS replies with RESP_DATA (possibly multiple times) then RESP_DONE,
//! or RESP_ERROR if the file is not found.
//!
//! # Response messages (vfs → shell)
//!
//! ## RESP_ENTRY
//! One directory entry (response to OP_LIST).
//! ```text
//!   data[0] = RESP_ENTRY
//!   data[1] = flags     (bit 0 = executable)
//!   data[2] = file size in bytes
//!   data[3..7] = filename (40 bytes, null-terminated, little-endian byte packing)
//! ```
//!
//! ## RESP_DONE
//! End of listing or end of file data.
//! ```text
//!   data[0] = RESP_DONE
//! ```
//!
//! ## RESP_DATA
//! One chunk of file data (response to OP_READ).
//! ```text
//!   data[0] = RESP_DATA
//!   data[1] = total file size in bytes
//!   data[2] = bytes in this chunk  (1–40; 0 means nothing more)
//!   data[3..7] = up to 40 bytes of raw file data (little-endian packing)
//! ```
//! The shell should keep sending OP_READ with increasing byte_offset until
//! byte_offset >= total file size, or until RESP_DONE is received.
//!
//! ## RESP_ERROR
//! File not found or other error.
//! ```text
//!   data[0] = RESP_ERROR
//!   data[1] = error code  (1 = not found)
//! ```

// ── Opcodes ───────────────────────────────────────────────────────────────────
pub const OP_LIST:   u64 = 0x20;
pub const OP_READ:   u64 = 0x22;

// ── Responses ─────────────────────────────────────────────────────────────────
pub const RESP_ENTRY: u64 = 0x80;
pub const RESP_DONE:  u64 = 0x81;
pub const RESP_DATA:  u64 = 0x82;
pub const RESP_ERROR: u64 = 0x8F;

/// Maximum bytes of file data carried in one RESP_DATA message
/// (data[3..7] = 5 words × 8 bytes).
pub const CHUNK_SIZE: usize = 40;
