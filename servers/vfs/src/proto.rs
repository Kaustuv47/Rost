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
//! Mutable entries that shadow a static entry are de-duplicated: only the
//! mutable (updated) version is emitted.
//!
//! ## OP_READ — read a chunk of a file (stateless)
//! ```text
//! data[0] = OP_READ
//! data[1] = byte_offset  (0 for first request)
//! data[2..8] = path
//! ```
//! VFS replies: one RESP_DATA per call; loop until RESP_DONE.
//! Mutable overlay is checked before the static tree.
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
//! ## OP_WRITE_OPEN — open/create a file for streaming write (legacy session API)
//! ```text
//! data[0] = OP_WRITE_OPEN
//! data[1] = flags  (FLAG_EXE = 0x02)
//! data[2..8] = path (48 bytes, null-terminated)
//! ```
//! VFS replies: RESP_OK on success, RESP_ERROR(ENOSPC) if table full,
//! RESP_ERROR(EISDIR) if path is a directory.
//!
//! ## OP_WRITE_DATA — append a 40-byte chunk to the open write session
//! ```text
//! data[0] = OP_WRITE_DATA
//! data[1] = chunk_len  (1–40 bytes valid in data[2..8])
//! data[2..8] = up to 40 bytes of file data (little-endian packed)
//! ```
//! VFS replies: RESP_OK or RESP_ERROR(EBADF / ENOSPC).
//!
//! ## OP_WRITE_CLOSE — finalise and close the write session
//! ```text
//! data[0] = OP_WRITE_CLOSE
//! ```
//! VFS replies: RESP_OK or RESP_ERROR(EBADF).
//!
//! ## OP_MKDIR — create a directory in the mutable overlay
//! ```text
//! data[0] = OP_MKDIR
//! data[1] = flags  (reserved, set 0)
//! data[2..8] = path (48 bytes, null-terminated)
//! ```
//! VFS replies: RESP_OK or RESP_ERROR(ENOSPC / ENOTDIR).
//!
//! ## OP_UNLINK — remove a mutable file or directory
//! ```text
//! data[0] = OP_UNLINK
//! data[2..8] = path (48 bytes, null-terminated)
//! ```
//! VFS replies: RESP_OK or RESP_ERROR(ENOENT).
//!
//! ## OP_OPEN — open a file descriptor (stateful fd API)
//! ```text
//! data[0] = OP_OPEN
//! data[1] = oflags  (O_RDONLY=0, O_WRONLY=1, O_RDWR=2 | O_CREAT=4 | O_TRUNC=8 | O_APPEND=16)
//! data[2..8] = path (48 bytes, null-terminated)
//! ```
//! VFS replies: RESP_FD(fd_number) or RESP_ERROR(ENOENT / ENOSPC / EMFILE / EISDIR / EACCES).
//!
//! ## OP_CLOSE — close a file descriptor
//! ```text
//! data[0] = OP_CLOSE
//! data[1] = fd
//! ```
//! VFS replies: RESP_OK or RESP_ERROR(EBADF).
//!
//! ## OP_READ_FD — read next chunk via file descriptor (VFS tracks offset)
//! ```text
//! data[0] = OP_READ_FD
//! data[1] = fd
//! ```
//! VFS replies: RESP_DATA (same format as OP_READ) or RESP_DONE at EOF.
//! RESP_ERROR(EBADF) if fd invalid; RESP_ERROR(EACCES) if write-only.
//!
//! ## OP_WRITE_FD — write a chunk via file descriptor
//! ```text
//! data[0] = OP_WRITE_FD
//! data[1] = fd
//! data[2] = chunk_len  (1–40)
//! data[3..8] = up to 40 bytes of file data (little-endian packed)
//! ```
//! VFS replies: RESP_OK or RESP_ERROR(EBADF / EACCES / ENOSPC).
//!
//! ## OP_SEEK — reposition file descriptor offset
//! ```text
//! data[0] = OP_SEEK
//! data[1] = fd
//! data[2] = new_offset  (absolute byte position)
//! ```
//! VFS replies: RESP_OK or RESP_ERROR(EBADF).
//!
//! ## OP_FSTAT — stat an open file descriptor
//! ```text
//! data[0] = OP_FSTAT
//! data[1] = fd
//! ```
//! VFS replies: RESP_STAT or RESP_ERROR(EBADF).
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
//! ## RESP_DATA — one file data chunk (reply to OP_READ / OP_READ_FD)
//! ```text
//! data[0] = RESP_DATA
//! data[1] = total file size
//! data[2] = bytes in this chunk  (1–40)
//! data[3..8] = up to 40 bytes of raw file data (little-endian packed)
//! ```
//!
//! ## RESP_STAT — path metadata (reply to OP_STAT / OP_FSTAT)
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
//! ## RESP_OK — generic success (no payload)
//! ```text
//! data[0] = RESP_OK
//! ```
//!
//! ## RESP_FD — successful OP_OPEN; carries the new fd number
//! ```text
//! data[0] = RESP_FD
//! data[1] = fd_number  (0–7)
//! ```
//!
//! ## RESP_ERROR — operation failed
//! ```text
//! data[0] = RESP_ERROR
//! data[1] = errno
//! ```

// ── Opcodes ───────────────────────────────────────────────────────────────────
// Stateless path-based operations (0x20–0x29)
pub const OP_READDIR:    u64 = 0x20;
pub const OP_READ:       u64 = 0x22;
pub const OP_STAT:       u64 = 0x23;
pub const OP_MOUNT:      u64 = 0x24;
pub const OP_WRITE_OPEN: u64 = 0x25;
pub const OP_WRITE_DATA: u64 = 0x26;
pub const OP_WRITE_CLOSE:u64 = 0x27;
pub const OP_MKDIR:      u64 = 0x28;
pub const OP_UNLINK:     u64 = 0x29;

// Stateful fd-based operations (0x30–0x35)
pub const OP_OPEN:       u64 = 0x30;
pub const OP_CLOSE:      u64 = 0x31;
pub const OP_READ_FD:    u64 = 0x32;
pub const OP_WRITE_FD:   u64 = 0x33;
pub const OP_SEEK:       u64 = 0x34;
pub const OP_FSTAT:      u64 = 0x35;

// Legacy alias kept for any code still sending the old opcode.
pub const OP_LIST: u64 = OP_READDIR;

// ── Responses ─────────────────────────────────────────────────────────────────
pub const RESP_ENTRY: u64 = 0x80;
pub const RESP_DONE:  u64 = 0x81;
pub const RESP_DATA:  u64 = 0x82;
pub const RESP_STAT:  u64 = 0x83;
pub const RESP_MOUNT: u64 = 0x84;
pub const RESP_OK:    u64 = 0x85;
pub const RESP_FD:    u64 = 0x86;
pub const RESP_ERROR: u64 = 0x8F;

// ── errno values ──────────────────────────────────────────────────────────────
pub const ENOENT:  u64 = 1; // no such file or directory
pub const ENOTDIR: u64 = 2; // not a directory
pub const EISDIR:  u64 = 3; // is a directory
pub const ENOSYS:  u64 = 4; // unsupported operation
pub const ENOSPC:  u64 = 5; // no space left (mutable table or fd table full)
pub const EBADF:   u64 = 6; // bad file descriptor (invalid or closed)
pub const EMFILE:  u64 = 7; // too many open files (per-process fd limit)
pub const EACCES:  u64 = 8; // permission denied (e.g. write to read-only static file)

// ── Sizes ─────────────────────────────────────────────────────────────────────
/// Bytes of path packed per IPC message (data[2..8] = 6 words × 8 bytes).
pub const PATH_BYTES:  usize = 48;
/// Maximum file-data bytes per RESP_DATA (data[3..8] = 5 words × 8 bytes).
pub const CHUNK_SIZE:  usize = 40;
/// Name bytes in RESP_ENTRY (data[3..8] = 5 words × 8 bytes).
pub const NAME_BYTES:  usize = 40;
