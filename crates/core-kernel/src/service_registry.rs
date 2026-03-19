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
    // Reject empty names — a zero-length key is indistinguishable from an
    // uninitialised table slot and would allow spurious lookup matches.
    if key.is_empty() { return false; }
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

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Each test uses a globally unique 15-char name (padded with NUL to 16 bytes)
    // so parallel test threads do not collide.  After each test the entry is
    // removed via `unregister_pid` to free the slot for subsequent runs.

    #[test]
    fn test_register_and_lookup() {
        let name = b"svc_reg_t01\0\0\0\0\0";
        assert!(register(name, 101), "register must succeed for a new name");
        assert_eq!(lookup(name), Some(101), "lookup must return the registered PID");
        unregister_pid(101);
    }

    #[test]
    fn test_lookup_nonexistent_returns_none() {
        let name = b"svc_noent_t02\0\0\0";
        // Ensure no entry exists first (may have been registered by a previous run).
        unregister_pid(99999); // harmless if not present
        assert_eq!(lookup(name), None, "unknown name must return None");
    }

    #[test]
    fn test_register_overwrites_existing() {
        let name = b"svc_overw_t03\0\0\0";
        assert!(register(name, 201));
        // Register the same name again with a different PID — must overwrite.
        assert!(register(name, 202));
        assert_eq!(lookup(name), Some(202),
            "second register must overwrite the first PID");
        unregister_pid(202);
    }

    #[test]
    fn test_unregister_makes_lookup_return_none() {
        let name = b"svc_unreg_t04\0\0\0";
        assert!(register(name, 301));
        assert!(unregister_pid(301), "unregister must return true when entry exists");
        assert_eq!(lookup(name), None, "lookup must return None after unregister");
    }

    #[test]
    fn test_unregister_nonexistent_returns_false() {
        // PID 99998 is not registered (we use unique PIDs per test).
        assert!(!unregister_pid(99998),
            "unregister of a non-existent PID must return false");
    }

    #[test]
    fn test_trim_name_stops_at_nul() {
        // Register using a name with an embedded NUL — only the prefix matters.
        let name_with_nul = b"hello\0world\0\0\0\0\0";
        let name_prefix   = b"hello\0\0\0\0\0\0\0\0\0\0\0";
        assert!(register(name_with_nul, 401));
        // Looking up just the prefix must find the entry (both trim to "hello").
        assert_eq!(lookup(name_prefix), Some(401),
            "lookup with matching prefix must find the entry");
        unregister_pid(401);
    }

    #[test]
    fn test_lookup_is_case_sensitive() {
        let lower = b"svc_case_t06\0\0\0\0";
        let upper = b"SVC_CASE_T06\0\0\0\0";
        assert!(register(lower, 501));
        // Names differ in case — upper-case lookup must NOT match.
        assert_eq!(lookup(upper), None,
            "lookup must be case-sensitive");
        unregister_pid(501);
    }
}
