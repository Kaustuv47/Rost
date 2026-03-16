//! RostFS RAM disk — static read-only file entries.
//!
//! Files are defined directly in source.  When a real block driver exists,
//! this module would be replaced by a parser that reads from disk.
//!
//! Flags: bit 0 = executable.

pub struct FileEntry {
    pub name:  &'static [u8],
    pub data:  &'static [u8],
    pub flags: u32,
}

/// The in-memory filesystem.
pub static FILES: &[FileEntry] = &[
    FileEntry {
        name:  b"readme.txt",
        flags: 0,
        data:  b"Rost Microkernel v0.1.0\n\
                 =======================\n\
                 A Rust no_std UEFI microkernel for x86_64.\n\
                 \n\
                 Architecture:\n\
                   crates/   ring-0 kernel (UEFI, x86_64-unknown-uefi)\n\
                   servers/  ring-3 servers (ELF, x86_64-unknown-none)\n\
                 \n\
                 Servers:\n\
                   rost-shell  interactive shell (PID 2 by convention)\n\
                   rost-vfs    virtual filesystem server (PID 3)\n\
                 \n\
                 Use 'ls' to list files, 'cat <file>' to read them.\n",
    },
    FileEntry {
        name:  b"motd.txt",
        flags: 0,
        data:  b"Welcome to Rost OS!\n\
                 Type 'help' for available commands.\n",
    },
    FileEntry {
        name:  b"hello",
        flags: 1, // executable (placeholder — needs ELF loader)
        data:  b"\x7fELF",  // ELF magic (stub — not a real binary)
    },
    FileEntry {
        name:  b"version.txt",
        flags: 0,
        data:  b"rost-kernel  0.1.0\n\
                 rost-shell   0.1.0\n\
                 rost-vfs     0.1.0\n",
    },
];

/// Find a file by name (null-terminated or exact slice match).
pub fn find(name: &[u8]) -> Option<&'static FileEntry> {
    // Trim trailing null bytes from the query (may come from packed words).
    let name = trim_null(name);
    FILES.iter().find(|f| trim_null(f.name) == name)
}

fn trim_null(s: &[u8]) -> &[u8] {
    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    &s[..end]
}
