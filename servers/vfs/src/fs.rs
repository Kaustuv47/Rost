//! RostFS — static RAM-backed directory tree.
//!
//! The filesystem is defined as a tree of `Node` values compiled into rodata.
//! There is no binary disk format to parse; the tree is pure Rust statics.
//! When a real block driver exists, this module would be replaced by a
//! FAT32/ext2 parser reading from the block device.
//!
//! # Layout
//! ```
//! /
//! ├── bin/
//! │   └── hello              (ELF stub, executable)
//! ├── etc/
//! │   ├── hosts
//! │   ├── motd
//! │   └── passwd
//! ├── home/
//! │   └── user/
//! │       ├── notes.txt
//! │       └── readme.txt
//! ├── root/
//! │   └── .profile
//! ├── var/
//! │   └── log/               (empty)
//! ├── motd.txt
//! └── version.txt
//! ```

// ── Node types ────────────────────────────────────────────────────────────────

pub struct Node {
    pub kind: NodeType,
}

pub enum NodeType {
    File { data: &'static [u8], flags: u32 },
    Dir  { children: &'static [DirEntry]    },
}

pub struct DirEntry {
    pub name: &'static [u8],
    pub node: &'static Node,
}

impl Node {
    pub const fn file(data: &'static [u8], flags: u32) -> Self {
        Node { kind: NodeType::File { data, flags } }
    }
    pub const fn dir(children: &'static [DirEntry]) -> Self {
        Node { kind: NodeType::Dir { children } }
    }
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, NodeType::Dir { .. })
    }
    pub fn size(&self) -> u64 {
        match &self.kind {
            NodeType::File { data, .. } => data.len() as u64,
            NodeType::Dir { children }  => children.len() as u64,
        }
    }
    /// Flags word:  bit 0 = is_directory,  bit 1 = executable.
    pub fn flags(&self) -> u64 {
        match &self.kind {
            NodeType::File { flags, .. } => *flags as u64,
            NodeType::Dir  { .. }        => 0x01,
        }
    }
}

// ── File contents ─────────────────────────────────────────────────────────────

static F_VERSION: Node = Node::file(b"\
rost-kernel   0.1.0\n\
rost-shell    0.1.0\n\
rost-vfs      0.1.0\n", 0);

static F_MOTD_ROOT: Node = Node::file(b"\
Welcome to Rost OS!\n\
Type 'help' for available commands.\n\
Use 'ls [path]' to list files, 'cat <path>' to read them.\n", 0);

static F_MOTD_ETC: Node = Node::file(b"\
Rost Microkernel \xE2\x80\x94 UEFI x86_64\n\
Kernel ring-0; all servers run ring-3 (hardware via IPC only).\n", 0);

static F_PASSWD: Node = Node::file(b"\
root:x:0:0:root:/root:/bin/sh\n\
user:x:1000:1000::/home/user:/bin/sh\n", 0);

static F_HOSTS: Node = Node::file(b"\
127.0.0.1   localhost\n\
::1         localhost\n", 0);

static F_PROFILE: Node = Node::file(b"\
# Root login profile\n\
PATH=/bin\n\
export PATH\n", 0);

static F_README: Node = Node::file(b"\
Rost Microkernel v0.1.0\n\
=======================\n\
\n\
Architecture:\n\
  crates/kernel       ring-0 kernel  (UEFI, x86_64-unknown-uefi)\n\
  crates/arch-x86_64  GDT/IDT/syscall/paging\n\
  crates/core-kernel  scheduler, IPC, process table\n\
  crates/hal          UART COM1 driver\n\
  servers/shell       ring-3 interactive shell  (rost-shell)\n\
  servers/vfs         ring-3 VFS/RAM-disk server (rost-vfs)\n\
\n\
Syscall table:\n\
  0 SYS_YIELD      cooperative yield\n\
  1 SYS_EXIT       terminate process\n\
  2 SYS_GETPID     return own PID\n\
  3 SYS_SEND       send 2-word message (uart-drv I/O)\n\
  4 SYS_RECV       receive word0\n\
  5 SYS_NOTIFY     lightweight notification bitmask\n\
  6 SYS_RECV_MSG   receive full 72-byte Message\n\
  7 SYS_SEND_MSG   send full 72-byte Message\n\
\n\
Next steps: ELF loader, block driver, uart-drv server.\n", 0);

static F_NOTES: Node = Node::file(b"\
Remaining work\n\
==============\n\
[ ] ELF loader (SYS_MAP + SYS_SPAWN kernel syscalls)\n\
[ ] uart-drv server (currently assumed PID 2, not yet running)\n\
[ ] block-drv server (virtio-blk / IDE port-I/O)\n\
[ ] FAT32 parser on top of block-drv\n\
[ ] service registry (name -> PID lookup, replace hardcoded PIDs)\n\
[ ] SYS_MAP: map physical pages into a process address space\n\
[ ] SYS_SPAWN: create a new process from an entry point\n\
[ ] True preemptive ISR scheduling (currently deferred at hlt boundaries)\n", 0);

// ELF magic header — placeholder until a real binary is loaded.
static F_HELLO: Node = Node::file(b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00", 0x02);

// ── Directory structure ───────────────────────────────────────────────────────
// Leaf directories first so parent statics can reference them.

static ENTRIES_BIN: [DirEntry; 1] = [
    DirEntry { name: b"hello", node: &F_HELLO },
];
static D_BIN: Node = Node::dir(&ENTRIES_BIN);

static ENTRIES_ETC: [DirEntry; 3] = [
    DirEntry { name: b"hosts",  node: &F_HOSTS    },
    DirEntry { name: b"motd",   node: &F_MOTD_ETC },
    DirEntry { name: b"passwd", node: &F_PASSWD   },
];
static D_ETC: Node = Node::dir(&ENTRIES_ETC);

static ENTRIES_HOME_USER: [DirEntry; 2] = [
    DirEntry { name: b"notes.txt",  node: &F_NOTES  },
    DirEntry { name: b"readme.txt", node: &F_README },
];
static D_HOME_USER: Node = Node::dir(&ENTRIES_HOME_USER);

static ENTRIES_HOME: [DirEntry; 1] = [
    DirEntry { name: b"user", node: &D_HOME_USER },
];
static D_HOME: Node = Node::dir(&ENTRIES_HOME);

static ENTRIES_ROOT_HOME: [DirEntry; 1] = [
    DirEntry { name: b".profile", node: &F_PROFILE },
];
static D_ROOT_HOME: Node = Node::dir(&ENTRIES_ROOT_HOME);

static ENTRIES_VAR_LOG: [DirEntry; 0] = [];
static D_VAR_LOG: Node = Node::dir(&ENTRIES_VAR_LOG);

static ENTRIES_VAR: [DirEntry; 1] = [
    DirEntry { name: b"log", node: &D_VAR_LOG },
];
static D_VAR: Node = Node::dir(&ENTRIES_VAR);

static ENTRIES_ROOT: [DirEntry; 7] = [
    DirEntry { name: b"bin",         node: &D_BIN       },
    DirEntry { name: b"etc",         node: &D_ETC       },
    DirEntry { name: b"home",        node: &D_HOME      },
    DirEntry { name: b"root",        node: &D_ROOT_HOME },
    DirEntry { name: b"var",         node: &D_VAR       },
    DirEntry { name: b"motd.txt",    node: &F_MOTD_ROOT },
    DirEntry { name: b"version.txt", node: &F_VERSION   },
];

/// The filesystem root node.
pub static ROOT: Node = Node::dir(&ENTRIES_ROOT);

// ── Path lookup ───────────────────────────────────────────────────────────────

/// Look up an absolute or root-relative path in the filesystem tree.
/// Leading `/` is stripped automatically.
pub fn lookup(path: &[u8]) -> Option<&'static Node> {
    let p = trim_null(path);
    let p = if p.starts_with(b"/") { &p[1..] } else { p };
    walk(&ROOT, p)
}

fn walk(node: &'static Node, remaining: &[u8]) -> Option<&'static Node> {
    if remaining.is_empty() {
        return Some(node);
    }
    match &node.kind {
        NodeType::Dir { children } => {
            let (component, rest) = split_first(remaining);
            if component.is_empty() {
                // Trailing slash — this node is the target.
                return Some(node);
            }
            for entry in *children {
                if trim_null(entry.name) == component {
                    return walk(entry.node, rest);
                }
            }
            None
        }
        NodeType::File { .. } => None, // can't descend into a file
    }
}

fn split_first(path: &[u8]) -> (&[u8], &[u8]) {
    match path.iter().position(|&b| b == b'/') {
        Some(pos) => (&path[..pos], &path[pos + 1..]),
        None      => (path, &[]),
    }
}

/// Strip trailing null bytes (artifacts of packed-word IPC transport).
pub fn trim_null(s: &[u8]) -> &[u8] {
    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    &s[..end]
}
