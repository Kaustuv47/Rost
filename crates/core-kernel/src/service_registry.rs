//! Microkernel service registry — maps short ASCII names to PIDs.
//!
//! Ring-3 servers register themselves with `SYS_REGISTER` (syscall 9) after
//! start-up.  Clients discover them with `SYS_LOOKUP` (syscall 10).
//!
//! # Design constraints
//! - `no_std`, no heap required (fixed-size static table).
//! - Maximum 16 services, names ≤ 15 bytes + NUL.
//! - Single-core: no locking needed; syscalls run with interrupts disabled.

const MAX_SERVICES: usize = 16;
const NAME_LEN:     usize = 16; // 15 printable chars + NUL

struct ServiceEntry {
    name: [u8; NAME_LEN],
    pid:  u32,
    used: bool,
}

impl ServiceEntry {
    const fn empty() -> Self {
        ServiceEntry { name: [0u8; NAME_LEN], pid: 0, used: false }
    }
}

static mut TABLE: [ServiceEntry; MAX_SERVICES] = [
    ServiceEntry::empty(), ServiceEntry::empty(), ServiceEntry::empty(), ServiceEntry::empty(),
    ServiceEntry::empty(), ServiceEntry::empty(), ServiceEntry::empty(), ServiceEntry::empty(),
    ServiceEntry::empty(), ServiceEntry::empty(), ServiceEntry::empty(), ServiceEntry::empty(),
    ServiceEntry::empty(), ServiceEntry::empty(), ServiceEntry::empty(), ServiceEntry::empty(),
];

// ── Public API ────────────────────────────────────────────────────────────────

/// Register `pid` under `name`.
///
/// - `name` must be a null-terminated byte slice (only the bytes before the
///   first `\0` are compared).
/// - If a registration for the same name already exists it is **overwritten**.
/// - Returns `false` if the table is full and no existing entry matches.
pub fn register(name: &[u8], pid: u32) -> bool {
    let key = trim_name(name);
    unsafe {
        // Update existing entry if name is already registered.
        for entry in TABLE.iter_mut() {
            if entry.used && trim_name(&entry.name) == key {
                entry.pid = pid;
                return true;
            }
        }
        // Find a free slot.
        for entry in TABLE.iter_mut() {
            if !entry.used {
                entry.name = [0u8; NAME_LEN];
                let copy_len = key.len().min(NAME_LEN - 1);
                entry.name[..copy_len].copy_from_slice(&key[..copy_len]);
                entry.pid  = pid;
                entry.used = true;
                return true;
            }
        }
    }
    false // table full
}

/// Look up the PID registered under `name`.
///
/// Returns `Some(pid)` if found, `None` otherwise.
pub fn lookup(name: &[u8]) -> Option<u32> {
    let key = trim_name(name);
    unsafe {
        for entry in TABLE.iter() {
            if entry.used && trim_name(&entry.name) == key {
                return Some(entry.pid);
            }
        }
    }
    None
}

/// Unregister the entry for `pid`.  Returns `true` if an entry was removed.
pub fn unregister_pid(pid: u32) -> bool {
    unsafe {
        for entry in TABLE.iter_mut() {
            if entry.used && entry.pid == pid {
                entry.used = false;
                return true;
            }
        }
    }
    false
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Return the slice up to (but not including) the first NUL byte.
fn trim_name(name: &[u8]) -> &[u8] {
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    &name[..end]
}
