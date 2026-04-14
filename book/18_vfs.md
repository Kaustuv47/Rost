# Chapter 18 — The VFS Server

## 18.1 Architecture Overview

The Virtual Filesystem (`servers/vfs`, binary `rost-vfs`, PID 4) is a ring-3
IPC server that presents a unified filesystem namespace to all clients.  It
implements a three-layer storage hierarchy:

```
Client (shell, libc, etc.)
         │ IPC (SYS_CALL / send_msg)
         ▼
┌─────────────────────────────────────────┐
│           Rost VFS Server               │
│                                         │
│  Layer 1: Mutable Overlay (mutable.rs)  │  ← highest priority
│    32 nodes × 4 KB = 131 KB BSS         │    writable, survives reboot of VFS
│                                         │
│  Layer 2: FAT32 (fat32.rs)             │  ← medium priority
│    reads from block-drv via IPC         │    optional; dormant if no block-drv
│                                         │
│  Layer 3: Static ROM tree (fs.rs)       │  ← lowest priority
│    compiled into rodata, read-only      │    always available
└─────────────────────────────────────────┘
         │ IPC (SYS_CALL / send_msg)
         ▼
    block-drv (PID 8)   ← virtio-blk disk
```

**Resolution order**: the VFS checks Layer 1 first.  If the path is not found
there, it tries Layer 2 (FAT32).  If still not found, it falls through to
Layer 3 (static ROM tree).  This allows files to be created/modified at runtime
(Layer 1) while the base system files remain read-only (Layer 3).

## 18.2 Two Client APIs

The VFS supports two client APIs over the same IPC channel:

### 18.2.1 Stateless Path API (opcodes 0x20–0x29)

Every request carries the full path and byte offset.  No state is maintained
between requests.

| Opcode | Name | Description |
|--------|------|-------------|
| 0x20 | `OP_READDIR` | List directory entries |
| 0x22 | `OP_READ` | Read file bytes at offset (40 B chunks) |
| 0x23 | `OP_STAT` | Get file metadata (size, flags) |
| 0x24 | `OP_MOUNT` | Mount a filesystem at a path |
| 0x25 | `OP_WRITE_OPEN` | Open a write session for a path |
| 0x26 | `OP_WRITE_DATA` | Append data to the active write session |
| 0x27 | `OP_WRITE_CLOSE` | Finalize and close the write session |
| 0x28 | `OP_MKDIR` | Create a directory |
| 0x29 | `OP_UNLINK` | Delete a file or empty directory |

### 18.2.2 Stateful fd API (opcodes 0x30–0x35)

The VFS tracks per-client file descriptors (fd table in `fd.rs`).

| Opcode | Name | Description |
|--------|------|-------------|
| 0x30 | `OP_OPEN` | Open a file, return fd |
| 0x31 | `OP_CLOSE` | Close fd |
| 0x32 | `OP_READ_FD` | Read bytes at current position |
| 0x33 | `OP_WRITE_FD` | Write bytes at current position |
| 0x34 | `OP_SEEK` | Reposition file offset |
| 0x35 | `OP_FSTAT` | Get metadata for an open fd |

### 18.2.3 Response Codes

| Code | Name | Meaning |
|------|------|---------|
| 0x80 | `RESP_ENTRY` | Directory entry (name in data[1..5]) |
| 0x81 | `RESP_DONE` | No more entries / end of read |
| 0x82 | `RESP_DATA` | File chunk (40 bytes in data[1..5]) |
| 0x83 | `RESP_STAT` | Stat result (size in data[1], flags in data[2]) |
| 0x84 | `RESP_MOUNT` | Mount acknowledgement |
| 0x85 | `RESP_OK` | Generic success |
| 0x8F | `RESP_ERROR` | Failure (error code in data[1]) |

## 18.3 Path Encoding

Paths are packed into IPC messages as 6 × u64 (48 bytes):

```
data[1] = bytes  0– 7 of path (little-endian)
data[2] = bytes  8–15
data[3] = bytes 16–23
data[4] = bytes 24–31
data[5] = bytes 32–39
data[6] = bytes 40–47
```

This allows paths up to 48 bytes (including the null terminator).  Longer paths
are truncated.  All well-known paths in Rost are within this limit:
`/home/user/notes.txt` = 21 bytes.

Entry names use 5 × u64 (40 bytes) in responses.

## 18.4 Layer 3: Static ROM Tree (`fs.rs`)

The lowest-priority layer is a Rust static data structure compiled into the
VFS binary's `.rodata`:

```rust
pub struct Node {
    pub kind: NodeType,
}

pub enum NodeType {
    File { data: &'static [u8], flags: u32 },
    Dir  { children: &'static [DirEntry]   },
}

pub struct DirEntry {
    pub name: &'static [u8],
    pub node: &'static Node,
}
```

The tree is defined as Rust statics:

```
/
├── bin/
│   ├── hello       (Rust ELF demo)
│   └── hello-c     (C ELF demo)
├── etc/
│   ├── hosts
│   ├── motd
│   └── passwd
├── home/
│   └── user/
│       ├── notes.txt
│       └── readme.txt
├── root/
│   └── .profile
├── var/
│   └── log/
├── motd.txt
└── version.txt
```

The ELF binaries (`/bin/hello`, `/bin/hello-c`) are embedded via:

```rust
static HELLO_ELF:   &[u8] = include_bytes!("../../hello-world/target/.../hello-world");
static HELLO_C_ELF: &[u8] = include_bytes!("../../hello-c/hello-c");
```

This means the VFS binary itself contains the ELF images for executables.
`exec /bin/hello` reads the bytes from the VFS, which returns them from rodata.

**Lookup**: path resolution is a recursive descent through the `DirEntry` arrays
using linear scan.  For 10–20 entries per directory this is O(N) but fast enough
in practice.

## 18.5 Layer 1: Mutable Overlay (`mutable.rs`)

The mutable overlay holds up to 32 nodes, each with up to 4 KB of content:

```rust
const MAX_NODES: usize = 32;
const MAX_CONTENT: usize = 4096;

struct MutNode {
    path:       [u8; 64],   // full path (null-padded)
    path_len:   usize,
    is_dir:     bool,
    content:    [u8; MAX_CONTENT],
    content_len: usize,
    used:       bool,
}

static mut OVERLAY: [MutNode; MAX_NODES] = [/* zeros */];
```

Total BSS: 32 × (64 + 8 + 1 + 4096 + 8 + 1) ≈ 131 KB.

### 18.5.1 Write Session Protocol

Creating or overwriting a file uses a three-message protocol:

```
Client → VFS:  OP_WRITE_OPEN  (data[1..4] = path)
VFS → Client:  RESP_OK

Client → VFS:  OP_WRITE_DATA  (data[1] = offset, data[2..5] = 32 bytes)
  (repeated until all data sent)

Client → VFS:  OP_WRITE_CLOSE
VFS → Client:  RESP_OK
```

Only one write session can be open at a time (serialized by the VFS dispatch loop
which is single-threaded).

### 18.5.2 Overlay Priority

When resolving a path, `mutable.rs` is checked before `fs.rs`.  This means a
file created in the overlay at `/etc/hosts` shadows the static `/etc/hosts` from
`fs.rs`.  Deleting the overlay entry (`OP_UNLINK`) restores visibility of the
static entry.

## 18.6 Layer 2: FAT32 (`fat32.rs`)

The FAT32 layer reads from a virtio-blk disk via the block-drv IPC server:

```rust
// blk.rs — read one sector from block-drv
fn read_sector(lba: u32, buf: &mut [u8; 512]) -> bool {
    let pid = get_blk_pid()?;
    // Send OP_BLK_READ with (lba, byte_offset=0)
    // Receive up to 13 RESP_BLK_DATA replies (13 × 40 = 520 bytes covers one 512-byte sector)
    ...
}
```

### 18.6.1 FAT32 Structures

```rust
struct Bpb {
    bytes_per_sector:    u16,
    sectors_per_cluster: u8,
    reserved_sectors:    u16,
    num_fats:            u8,
    fat_size_32:         u32,
    root_cluster:        u32,
}
```

The BPB (BIOS Parameter Block) is parsed from sector 0.  File data is accessed
by:
1. Walking the FAT chain from the root cluster to find the directory
2. Scanning directory entries (with LFN support for long filenames up to 255 chars)
3. Following the FAT chain for the file's data clusters

### 18.6.2 Long Filename Support

FAT32 long filenames are stored in Unicode LFN directory entries.  The parser
collects LFN entries and assembles the filename:

```rust
fn collect_lfn(entries: &[DirEntry83]) -> [u8; 256] {
    // LFN entries are stored in reverse order
    // Each entry carries 13 UTF-16 characters (26 bytes)
    // We keep only the ASCII subset (UTF-16 → ASCII by taking low byte)
}
```

Only the ASCII subset is supported; non-ASCII Unicode filenames fall back to
the 8.3 short name.

### 18.6.3 Dormant Fallback

If the block-drv server is not registered (`SYS_LOOKUP("block-drv")` returns
`u64::MAX`), all FAT32 lookups return `None` immediately.  The VFS then falls
through to the static ROM tree.  This means the system boots and functions
without a disk.

## 18.7 READDIR Deduplication

When a client sends `OP_READDIR`, the VFS merges entries from all three layers.
Without deduplication, a path that exists in both the overlay and the ROM tree
would appear twice.

The `SeenNames` struct prevents this:

```rust
struct SeenNames {
    names: [[u8; 40]; 32],
    count: usize,
}

impl SeenNames {
    fn has(&self, name: &[u8]) -> bool { /* linear scan */ }
    fn add(&mut self, name: &[u8]) { /* store first 40 bytes */ }
}
```

The VFS sends each entry name exactly once (first seen wins, which gives the
overlay priority).

## 18.8 Stateful fd Table (`fd.rs`)

The fd table enables POSIX-compatible file I/O for rost-libc clients:

```rust
const FD_MAX: usize = 16;

struct FdEntry {
    used:    bool,
    pid:     u32,    // owning process (fds are per-process)
    path:    [u8; 64],
    offset:  u64,
    is_dir:  bool,
    writable: bool,
}

static mut FD_TABLE: [FdEntry; FD_MAX] = [/* zeros */];
```

`OP_OPEN` allocates an fd slot; `OP_CLOSE` frees it.  `OP_READ_FD` and
`OP_WRITE_FD` advance `offset` automatically.  `OP_SEEK` repositions it.

The fd table is keyed by `(pid, fd_index)`.  Different processes can have the
same fd number (e.g. both have fd 3) without conflict.

## 18.9 The VFS Dispatch Loop

```rust
fn dispatch_loop() -> ! {
    loop {
        let mut req = Msg::zeroed();
        if !recv_msg(u64::MAX, &mut req) { continue; }

        let from = req.sender;

        match req.data[0] {
            OP_READDIR  => handle_readdir(from, &req),
            OP_READ     => handle_read(from, &req),
            OP_STAT     => handle_stat(from, &req),
            OP_WRITE_OPEN  => handle_write_open(from, &req),
            OP_WRITE_DATA  => handle_write_data(from, &req),
            OP_WRITE_CLOSE => handle_write_close(from, &req),
            OP_MKDIR    => handle_mkdir(from, &req),
            OP_UNLINK   => handle_unlink(from, &req),
            OP_OPEN     => handle_open(from, &req),
            OP_CLOSE    => handle_close(from, &req),
            OP_READ_FD  => handle_read_fd(from, &req),
            OP_WRITE_FD => handle_write_fd(from, &req),
            OP_SEEK     => handle_seek(from, &req),
            OP_FSTAT    => handle_fstat(from, &req),
            _           => { /* unknown opcode — send RESP_ERROR */ }
        }
    }
}
```

The loop is fully single-threaded (Rost is single-core).  There are no locks
or concurrency concerns.

## 18.10 Summary

The Rost VFS server provides:

- **Three-layer hierarchy** — mutable overlay → FAT32 → static ROM tree
- **131 KB mutable overlay** — 32 nodes × 4 KB, writable at runtime
- **FAT32 support** — full cluster chain traversal, LFN filenames, lazy activation
- **Static ROM tree** — zero-cost fallback compiled into the VFS binary
- **Stateless API** — path + offset on every call, no session state
- **Stateful fd API** — POSIX-compatible open/read/write/seek/close
- **READDIR dedup** — overlay shadows ROM entries, no duplicate listings
- **IPC protocol** — 40-byte chunks, 48-byte paths, response codes
