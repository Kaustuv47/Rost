# Chapter 12 — Service Registry

## 12.1 The Discovery Problem

In a microkernel system, processes communicate by PID.  But PIDs are dynamic —
a server that crashes and restarts gets a new PID.  Hard-coding PIDs in client
code would mean updating and recompiling all clients every time a server restarts.

The service registry solves this: servers register a stable ASCII name, and
clients look up the current PID for that name.

## 12.2 The Registry Implementation

```rust
/// One entry in the service registry.
struct ServiceEntry {
    name: [u8; 16],   // null-padded ASCII name (max 15 chars)
    pid:  u32,
    used: bool,
}

/// Static 16-entry table — no heap required.
static mut REGISTRY: [ServiceEntry; 16] = [/* zeros */];
```

The registry is a simple linear-scan table with 16 entries.  This is intentional:
the kernel has at most 10 well-known servers plus a few dynamic ones.  A hash map
would be faster but adds complexity with no practical benefit for 10–20 entries.

### 12.2.1 Registration

```rust
pub fn register(name: &[u8], pid: u32) -> bool {
    // Scan for existing entry with the same name (overwrite)
    for entry in unsafe { REGISTRY.iter_mut() } {
        if entry.used && name_matches(&entry.name, name) {
            entry.pid = pid;
            return true;
        }
    }
    // Scan for a free slot
    for entry in unsafe { REGISTRY.iter_mut() } {
        if !entry.used {
            copy_name(&mut entry.name, name);
            entry.pid = pid;
            entry.used = true;
            return true;
        }
    }
    false  // table full (max 16 entries)
}
```

A server is allowed to overwrite its own registration.  This is used when a
server restarts — it re-registers with its new PID.

### 12.2.2 Lookup

```rust
pub fn lookup(name: &[u8]) -> Option<u32> {
    for entry in unsafe { REGISTRY.iter() } {
        if entry.used && name_matches(&entry.name, name) {
            return Some(entry.pid);
        }
    }
    None
}
```

Lookup is O(N) where N ≤ 16.

### 12.2.3 Unregistration on Termination

```rust
pub fn unregister_pid(pid: u32) {
    for entry in unsafe { REGISTRY.iter_mut() } {
        if entry.used && entry.pid == pid {
            entry.used = false;
            // Zero the name to prevent information leakage
            entry.name = [0u8; 16];
        }
    }
}
```

`terminate_process()` calls `unregister_pid()` unconditionally.  This ensures
dead PIDs cannot be discovered by clients who haven't noticed the server has died.

## 12.3 Name Encoding

Service names are null-padded ASCII strings of up to 15 characters in a 16-byte
buffer.  The comparison strips trailing null bytes:

```rust
fn name_matches(stored: &[u8; 16], query: &[u8]) -> bool {
    let end = stored.iter().position(|&b| b == 0).unwrap_or(16);
    let stored_str = &stored[..end];
    let query_end = query.iter().position(|&b| b == 0).unwrap_or(query.len());
    let query_str = &query[..query_end];
    stored_str == query_str  // case-sensitive comparison
}
```

Lookup is case-sensitive.  "UART-DRV" and "uart-drv" are different names.

## 12.4 Well-Known Service Names

| Name | Server | PID (typical) |
|------|--------|---------------|
| `"uart-drv"` | UART driver | 3 |
| `"rost-vfs"` | Virtual filesystem | 4 |
| `"rost-shell"` | Interactive shell | 5 |
| `"rost-net"` | Network stack | 6 |
| `"pci-bus"` | PCI bus scanner | 7 |
| `"block-drv"` | virtio-blk driver | 8 |
| `"gop"` | GOP framebuffer driver | 9 |
| `"ps2-kbd"` | PS/2 keyboard driver | 10 |
| `"init"` | Init process | 1 |

## 12.5 Named Endpoint Capabilities

For higher security, clients can use `SYS_LOOKUP_CAP` (syscall 24) instead of
`SYS_LOOKUP` (syscall 11).  The difference:

- `SYS_LOOKUP` — returns the raw PID.  The client then uses `SYS_SEND_MSG` with
  the raw PID.  If an attacker knows a PID, they can also send to it.

- `SYS_LOOKUP_CAP` — returns a capability slot index.  The kernel creates a
  `Channel` capability in the caller's cap table pointing to the server.  The
  client uses `SYS_SEND_CAP` with the slot index.  The raw PID is never exposed.

```rust
SYS_LOOKUP_CAP => {
    let name_ptr = a0;
    if !validate_user_ptr(name_ptr, 16, 1) { return EINVAL; }

    let name = /* read 16 bytes from user */ ;
    let pid = service_registry::lookup(&name)
        .ok_or(ENOENT)?;

    // Create a Channel capability in the caller's cap table
    let cap = Capability::new(CapKind::Channel, CAP_W, pid);
    let slot = sched.cap_alloc(current_pid, cap)
        .ok_or(ENOMEM)?;  // ENOMEM if cap table full

    slot as u64
}
```

## 12.6 Lazy PID Lookup in Servers

Ring-3 servers often need to contact other servers.  They use a lazy-lookup
pattern with an atomic cache:

```rust
// servers/vfs/src/blk.rs:
static BLK_PID: AtomicU32 = AtomicU32::new(0);

fn get_blk_pid() -> Option<u32> {
    let cached = BLK_PID.load(Ordering::Relaxed);
    if cached != 0 { return Some(cached); }

    // Lookup and cache
    let pid = syscall::lookup(b"block-drv")?;
    BLK_PID.store(pid, Ordering::Relaxed);
    Some(pid)
}
```

This pattern:
1. Returns the cached PID on all subsequent calls (O(1))
2. Handles the case where the block device driver hasn't registered yet
   (returns `None`, VFS falls back to the static ROM tree)
3. Is thread-safe (single-core, AtomicU32 for future-proofing)

## 12.7 Service Restart

When init restarts a crashed server via `SYS_RESTART_SERVER`:
1. The kernel calls `spawn_elf` with the embedded ELF
2. The new process gets a new PID
3. The new process calls `SYS_REGISTER` with the same name
4. The new registration overwrites the old (now-stale) entry
5. Clients that do lazy lookup will discover the new PID on their next request

Clients that have cached the old PID may experience one failed IPC before
discovering the new PID.  The VFS blk.rs handles this by checking the RESP_ERROR
code and retrying with a fresh lookup.

## 12.8 Summary

The service registry provides:
- **Name-to-PID mapping** — stable string names survive server restarts
- **Automatic cleanup** — `terminate_process` removes stale registrations
- **Named capabilities** — `SYS_LOOKUP_CAP` provides unforgeable access tokens
- **Lazy lookup pattern** — atomic cache for O(1) repeat lookups
- **16-entry limit** — deterministic capacity, no heap
