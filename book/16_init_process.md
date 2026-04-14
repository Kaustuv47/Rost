# Chapter 16 — The Init Process (PID 1)

## 16.1 Role of Init

Init (`servers/init`) is the first user-space process spawned by the kernel (PID 1).
It owns three responsibilities:

1. **Service registry** — registers under the name `"init"` so other processes can
   send it messages by name
2. **Health monitor** — watches registered servers for crashes and heartbeat timeouts
3. **Lifecycle controller** — decides whether to restart a crashed server or initiate
   an ordered shutdown

Init has no device access and no special kernel privileges beyond being PID 1.
Fault notifications flow to it because the kernel hard-codes PID 1 as the recipient
in `terminate_faulting_process`.

## 16.2 IPC Protocol

Init speaks a simple message-based protocol over the standard IPC mechanism.

### 16.2.1 Fault Notification (Kernel → Init)

When a ring-3 process causes a hardware exception (#DE, #GP, #PF), the kernel:
1. Terminates the faulting process
2. Sends a fault notification to PID 1 before switching to the next process

```
Message layout:
  data[0] = fault_code    (0xDE / 0x0D / 0x0E)
  data[1] = faulting_pid
  sender  = kernel-stamped PID of the faulting process
```

Fault codes are chosen to match x86 exception vector numbers and are below 0x100,
making them distinct from IPC opcodes which start at 0x0001.

### 16.2.2 Heartbeat (Server → Init)

Servers prove they are alive by periodically sending:

```
  data[0] = OP_HEARTBEAT (0x0001)
  data[1] = sender's own PID
```

Init records `last_beat[slot] = clock()`.  If a slot's timestamp goes stale
beyond `HEARTBEAT_TIMEOUT_NS = 5_000_000_000` (5 seconds), init logs a warning.
Note: the heartbeat timeout currently only logs — it does not proactively kill
the process.  A missed heartbeat combined with a subsequent crash will trigger
the normal restart path.

### 16.2.3 Boot Log Read (Any Process → Init)

```
Request:  data[0] = OP_LOG_READ (0x0002)
          data[1] = seq         (entry index to fetch)

Reply:    data[0] = OP_LOG_READ_REPLY (0x8002)
          data[1] = echoed seq
          data[2] = next_seq    (one past the last valid seq in the ring)
          data[3] = timestamp_ns
          data[4..7] = 32 bytes of text packed little-endian into 4 × u64
```

This allows a diagnostic shell command (`log`) to read init's internal event log
without kernel involvement.  The shell uses `SYS_CALL(init_pid, ...)` and blocks
until init replies.

### 16.2.4 Shutdown Request (Any Process → Init)

```
  data[0] = OP_SHUTDOWN (0x00FF)
```

Any process that knows init's PID (via `SYS_LOOKUP("init")`) can trigger an
ordered shutdown.

## 16.3 Service Watchlist

Init monitors three well-known services:

```rust
let mut services: [ServiceEntry; 3] = [
    ServiceEntry::new(b"uart-drv\0\0\0\0\0\0\0\0", true),   // critical
    ServiceEntry::new(b"rost-vfs\0\0\0\0\0\0\0\0", true),   // critical
    ServiceEntry::new(b"rost-shell\0\0\0\0\0\0",   false),  // non-critical
];
```

**Critical services** (uart-drv, vfs): if they crash and cannot be restarted
within `MAX_RESTARTS = 3` attempts, init calls `ordered_shutdown()` — the system halts.

**Non-critical services** (shell): if they crash and exhaust restart budget,
init logs a warning and continues.  The shell is non-critical because a shell
crash doesn't affect ongoing kernel or device operations.

### 16.3.1 Lazy PID Resolution

Init does not know the PIDs of other servers at startup.  It resolves them lazily:

```rust
loop {
    for svc in services.iter_mut() {
        if svc.pid == u64::MAX {
            let pid = syscall::lookup(svc.name);
            if pid != u64::MAX {
                svc.pid = pid;
                // log resolution
            }
        }
    }
    // ... process messages ...
}
```

PIDs are u64 values; `u64::MAX` is the sentinel for "not yet resolved".  Once
resolved, the PID is cached and not re-looked up until the service restarts.

## 16.4 The Boot Log Ring Buffer

```rust
const LOG_ENTRIES: usize = 16;

struct LogEntry {
    seq:  u64,       // monotonically increasing sequence number
    ts:   u64,       // timestamp in nanoseconds (from SYS_CLOCK)
    text: [u8; 32],  // null-padded event text
}

struct LogBuf {
    entries:  [LogEntry; LOG_ENTRIES],
    write:    usize,   // next write slot (0..LOG_ENTRIES)
    count:    usize,   // entries currently in ring (0..=LOG_ENTRIES)
    next_seq: u64,     // seq assigned to next append
}
```

The ring buffer holds the last 16 events.  Events appended by init include:
- `"init started pid=1"`
- `"resolved uart-drv pid=3"`
- `"FAULT #PF(page-fault) pid=5"`
- `"RESTART OK pid=11 rost-shell"`
- `"HB TIMEOUT pid=6 rost-net"`

The shell `log` command reads entries by sequence number via `OP_LOG_READ`.
Since `next_seq` is returned in every reply, the shell walks backward from
`next_seq - 1` to `next_seq - count` to print all available entries.

### 16.4.1 Text Packing

Event text (32 bytes) is packed into 4 × u64 in the IPC reply:

```rust
for chunk in 0..4usize {
    let off = chunk * 8;
    let mut word = 0u64;
    for byte_idx in 0..8usize {
        word |= (e.text[off + byte_idx] as u64) << (byte_idx * 8);
    }
    reply.data[4 + chunk] = word;
}
```

Little-endian byte order matches x86 native order, so the shell can unpack with
a simple byte slice cast.

## 16.5 Message Dispatch Loop

```rust
loop {
    // 1. Resolve unresolved service PIDs
    for svc in services.iter_mut() { ... }

    // 2. Drain any queued messages (non-blocking)
    while syscall::recv_msg(0, &mut msg) {
        handle_msg(&mut msg, ...);
    }

    // 3. Block for up to 50 ticks (500 ms) waiting for the next message
    if syscall::recv_msg(HEARTBEAT_CHECK_TICKS, &mut msg) {
        handle_msg(&mut msg, ...);
    }

    // 4. Check heartbeat timestamps
    if warmup_ticks == 0 {
        let now = syscall::clock();
        for (i, svc) in services.iter().enumerate() {
            if now.wrapping_sub(last_beat[i]) > HEARTBEAT_TIMEOUT_NS {
                // log warning, reset last_beat
            }
        }
    } else {
        warmup_ticks -= 1;  // ~300 ms grace period at boot
    }
}
```

**Why block instead of yield?**: Init runs at priority 32 (higher than shell at 128,
lower number = higher priority in Rost's priority scheme).  If init called
`SYS_YIELD`, it would remain in the Ready state — the scheduler would preempt
lower-priority processes and keep running init repeatedly.  This would starve
uart-drv (priority 64) and shell (priority 128).

Blocking with `recv_msg(timeout)` transitions init to Blocked.  While Blocked,
it is invisible to the scheduler, and uart-drv / shell run freely.  After
`HEARTBEAT_CHECK_TICKS = 50` ticks (500 ms), the deadline wakes init.

## 16.6 Restart Logic

When a fault notification arrives, init searches the watchlist for the faulting PID:

```rust
if let Some(i) = matched_idx {
    let svc = &mut services[i];

    if svc.restart_count < MAX_RESTARTS {
        svc.restart_count += 1;

        match syscall::restart_server(svc.name) {
            Some(new_pid) => {
                svc.pid = new_pid as u64;  // update cached PID
                // log success
            }
            None => {
                // log failure
                if svc.critical { ordered_shutdown(); }
            }
        }
    } else {
        // Budget exhausted
        svc.pid = u64::MAX;  // mark as gone
        if svc.critical { ordered_shutdown(); }
        else { /* warn and continue */ }
    }
}
```

`syscall::restart_server(name)` calls `SYS_RESTART_SERVER` (27).  The kernel
looks up the ELF blob by name in its embedded table, calls `spawn_elf`, registers
the new process under the same service name (overwriting the stale entry), and
returns the new PID.

### 16.6.1 Restart Counter Reset

The restart counter `svc.restart_count` is **not** reset after a successful
restart.  This is intentional: if a server crashes repeatedly (e.g. due to a
bug triggered by specific input), the counter accumulates across crashes.  After
`MAX_RESTARTS = 3` total crashes, init gives up.

A more sophisticated policy (exponential backoff, counter reset after N seconds
of stability) is a future improvement.

## 16.7 Ordered Shutdown

```rust
fn ordered_shutdown() -> ! {
    print("[init] ordered shutdown — system halting\n");
    syscall::exit(1);
}
```

Currently, `ordered_shutdown` exits PID 1.  The kernel terminates init, and
without any remaining processes to schedule (all servers have crashed or been
terminated), the scheduler runs the idle process permanently.  After 10 seconds,
the hardware watchdog fires and resets the machine.

Future improvement: send `OP_SHUTDOWN` to each registered service and wait for
acknowledgements before calling `syscall::exit(1)`.  This would allow servers
to flush buffers, sync disk state, and close connections before reset.

## 16.8 The TextBuf Builder

Init cannot use `format!` or `write!` because it is `no_std` without an allocator.
The `TextBuf` struct provides a minimal string builder for constructing log messages:

```rust
struct TextBuf {
    buf: [u8; 32],
    pos: usize,
}

impl TextBuf {
    fn push(&mut self, s: &[u8]) { /* copy bytes, truncate at 31 */ }
    fn push_dec(&mut self, mut v: u64) { /* decimal integer → bytes */ }
    fn as_bytes(&self) -> &[u8] { &self.buf[..self.pos] }
}
```

Used for constructing log entries like:

```rust
let mut t = TextBuf::new();
t.push(b"FAULT #PF(page-fault) pid=");
t.push_dec(fault_pid);
log.append(ts, t.as_bytes());
```

## 16.9 Init vs. the Kernel: Division of Responsibility

| Responsibility | Kernel | Init |
|----------------|--------|------|
| Detect ring-3 fault | Yes (#PF/#GP/#DE handler) | No |
| Terminate faulting process | Yes | No |
| Decide to restart | No | Yes |
| Execute restart (spawn ELF) | Yes (SYS_RESTART_SERVER) | Requests it |
| Decide to halt | No | Yes |
| Perform halt | No (watchdog resets) | Exits, watchdog fires |

This separation keeps the kernel minimal.  Kernel policy is: "ring-3 fault →
terminate and notify PID 1."  Init policy is: "notification received → apply
restart policy."  The kernel doesn't need to know whether a server is critical
or how many times it has been restarted.

## 16.10 Summary

The init process provides:

- **Fault handling** — receives kernel fault notifications for all ring-3 crashes
- **Service restart** — up to 3 attempts per service via `SYS_RESTART_SERVER`
- **Critical/non-critical policy** — uart-drv and vfs are critical; shell is not
- **Heartbeat monitoring** — 5-second timeout with warning logging
- **Boot event log** — 16-entry ring buffer, accessible via `OP_LOG_READ` IPC
- **Shutdown coordination** — initiates ordered halt on unrecoverable service failure
- **Blocking dispatch loop** — yields CPU to other servers while waiting for messages
