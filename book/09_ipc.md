# Chapter 9 — Inter-Process Communication

## 9.1 IPC as the Only Legal Channel

In a microkernel, IPC is not an optional feature — it is the entire point.  Every
interaction between ring-3 processes must go through IPC.  A device driver cannot
directly touch a file system's data structures.  A shell cannot directly touch a
network driver's ring buffers.  The kernel enforces this: spatial isolation means
there is no shared memory unless explicitly set up with `SYS_MAP_SHARE`.

This architectural constraint has a profound implication for security: if you know
exactly which IPC channels exist and what can flow through each one, you know the
entire communication topology of the system.  This makes formal verification of
the system's information flow properties tractable.

## 9.2 The `Message` Type

```rust
/// An IPC message: 72 bytes.
///
/// The first 8 bytes are the sender PID (stamped by the kernel — unforgeable).
/// The next 64 bytes are the payload: 8 × u64 = 64 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Message {
    pub sender: ProcessId,   // 4 bytes — set by kernel, cannot be forged
    _pad:       [u8; 4],
    pub data:   [u64; 8],    // 64 bytes — payload
}
```

The 8-word data array is large enough to hold:
- A complete VFS request (opcode + path + offset + flags)
- A complete network packet header
- Any IPC protocol designed for Rost

The kernel stamps `Message.sender` with the calling process's PID before
enqueueing the message.  User-space code cannot set the sender field — any
value written there is overwritten by the kernel.  This provides unforgeable
sender authentication.

## 9.3 The `MessageQueue`

Each process has an inline `MessageQueue` in its PCB:

```rust
pub struct MessageQueue {
    entries:            [Message; 16],
    head:               usize,
    tail:               usize,
    count:              usize,
    pending_notification: u64,
}
```

The circular buffer holds 16 messages.  `is_full()` means the 17th send would
be dropped.  The queue is inline in the PCB to avoid heap allocation.

### 9.3.1 Notification Word

The `pending_notification` field is a separate 64-bit bitmask that accumulates
notification bits.  When process A calls `SYS_NOTIFY(B, word)`, the kernel ORs
`word` into B's `pending_notification`.  Process B reads it by calling `SYS_RECV`
which returns the accumulated word and clears it atomically.

Notifications are faster than full messages — they don't go through the queue
and don't require a context switch.  They are used for lightweight signaling
(e.g., "IRQ delivered", "shutdown requested").

## 9.4 The Synchronous Call Primitive: `SYS_CALL`

`SYS_CALL` (syscall 17) provides synchronous request/reply semantics:

```rust
// Ring-3 client:
let mut reply = Message::zero();
let ok = syscall::call(to_pid, &request, &mut reply, timeout_ticks);
// 'ok' is true if the reply arrived before the timeout
// 'reply' contains the server's response
```

The kernel's implementation:
1. Send the request message to `to_pid`'s mailbox
2. Check for deadlock: `detect_cycle(caller, to_pid)`
3. Block the caller with `blocking_receive(caller, timeout_ticks)`
4. When the caller is unblocked (server replied), copy the reply to the user buffer

The server sends its reply by calling `SYS_SEND_MSG` back to the client.  The
kernel unblocks the client when the reply arrives.

This pattern — send + block + server replies + unblock — is the fundamental
building block of the entire Rost IPC protocol.  Every VFS call, every device
driver call, every network call uses this pattern via the `syscall::call()` wrapper.

### 9.4.1 Deadlock Detection in `SYS_CALL`

Between the send and the block, the kernel runs deadlock detection:

```rust
fn handle_sys_call(to_pid, send_buf, reply_buf, timeout_ticks) -> u64 {
    let caller = current_pid();
    let sched = get_global();

    // Send the request
    sched.send_message(caller, to_pid, request_msg);

    // Check for circular wait (A→B→A or A→B→C→A)
    if sched.detect_deadlock(caller, to_pid) {
        return EDEADLK;  // -8
    }

    // Record that caller is waiting for to_pid (for priority inheritance)
    sched.set_waiting_for(caller, to_pid);

    // Block until reply
    match sched.blocking_receive(caller, timeout_ticks) {
        Some(reply) => { /* copy reply to user buffer */ 0 }
        None        => ETIMEDOUT,  // -6
    }
}
```

## 9.5 The Capability System

Raw PID-based IPC has a security problem: if process A knows process B's PID,
it can send B messages even if it should not have access to B.  The capability
system fixes this.

### 9.5.1 What Is a Capability?

```rust
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Capability {
    pub kind:      CapKind,    // Channel, Process, Memory, Service, None
    pub rights:    u8,         // bitmask: CAP_R | CAP_W | CAP_G | CAP_X
    _pad:          [u8; 2],
    pub object_id: u32,        // PID, frame number, service ID, etc.
}
```

A `Capability` is an unforgeable kernel-issued token.  Holding a capability means
"the kernel has explicitly granted this process the right to perform operation X
on object Y."  Without the capability, the operation is rejected with `EPERM`.

Rights:
- `CAP_R` (0x01) — read / receive
- `CAP_W` (0x02) — write / send
- `CAP_G` (0x04) — grant (transfer the capability to another process)
- `CAP_X` (0x08) — execute / manage (e.g., kill a process)

### 9.5.2 The Capability Table

Each PCB has a 64-slot capability table:

```rust
pub cap_table: [Capability; CAP_TABLE_SIZE],  // CAP_TABLE_SIZE = 64
```

Capability operations:
- `SYS_CHAN_BIND (20)` — create a Channel cap pointing to a target PID
- `SYS_SEND_CAP (21)` — send a message via a Channel cap (checks CAP_W)
- `SYS_CAP_GRANT (18)` — transfer a cap to another process (checks CAP_G)
- `SYS_LOOKUP_CAP (24)` — resolve service name → Channel cap (without raw PID exposure)
- `SYS_MAP_SHARE (22)` — allocate shared memory + create Memory cap
- `SYS_MAP_CAP (23)` — map shared memory via a Memory cap

### 9.5.3 The Named Endpoint Model

The capability system supports a "named endpoint" model where servers don't
expose their raw PIDs to clients.  Instead:

```
Server: registers as "rost-vfs" via SYS_REGISTER
Client: calls SYS_LOOKUP_CAP("rost-vfs")
        → kernel resolves "rost-vfs" → PID 4
        → kernel creates Channel cap (CAP_W) in caller's cap table
        → returns cap slot index
Client: calls SYS_SEND_CAP(slot, msg)
        → kernel checks CapKind::Channel + CAP_W
        → kernel extracts PID from cap.object_id
        → kernel forwards message with unforgeable sender
```

The client never learns PID 4.  If the VFS server restarts and gets a new PID,
the client just calls `SYS_LOOKUP_CAP` again.

### 9.5.4 Shared Memory via Capabilities

For bulk data transfer (large files, framebuffer data), raw IPC messages are
inefficient.  Shared memory avoids copying:

```rust
// Sender:
let cap_slot = syscall::map_share(vaddr, true /* writable */)?;
// Writes data to vaddr ...
syscall::cap_grant(cap_slot, receiver_pid)?;
syscall::send_msg(receiver_pid, NOTIFY_DATA_READY_MSG);

// Receiver (after receiving the notification):
syscall::map_cap(their_vaddr, cap_slot, false /* read-only */)?;
// Reads data from their_vaddr ...
```

The kernel's implementation:
- `SYS_MAP_SHARE`: allocates a zeroed physical frame, maps it in the caller's
  VAS with PTE_USER, creates a `CapKind::Memory` entry with `object_id = PFN`
- `SYS_MAP_CAP`: verifies the cap is `CapKind::Memory + CAP_R`, derives
  `frame_phys = cap.object_id << 12`, maps in the receiver's VAS

No physical copy occurs — both processes see the same physical frame at their
respective virtual addresses.

## 9.6 Rate Limiting

To prevent a misbehaving process from flooding the IPC subsystem:

```rust
fn send_message(&self, ...) -> bool {
    let ptable = self.process_table.borrow_mut();
    if let Some(pcb) = ptable.get_process(from_pid) {
        // Check rate limit
        if pcb.ipc_rate_limit > 0
           && pcb.ipc_rate_used >= pcb.ipc_rate_limit
        {
            return false;  // drop the message
        }
        pcb.ipc_rate_used = pcb.ipc_rate_used.saturating_add(1);
    }
    // ... enqueue message
}
```

`ipc_rate_used` is reset to 0 every 100 ticks by `reset_ipc_rate_counters()`.
A process with `ipc_rate_limit = 100` can send at most 100 messages per second.

## 9.7 IPC Protocol Conventions

Rost uses a consistent IPC protocol across all servers:

```
data[0] = opcode (high u32 = category, low u32 = operation)
data[1..7] = operation-specific arguments
```

Response opcodes have bit 7 of the high byte set:
```
OP_READ   = 0x22  → RESP_DATA  = 0x82
OP_OPEN   = 0x30  → RESP_FD    = 0x86
OP_STAT   = 0x23  → RESP_STAT  = 0x83
OP_ERROR  = ----  → RESP_ERROR = 0x8F
```

Error responses pack an errno value in `data[1]`:
```
ENOENT  = 1,  ENOTDIR = 2,  EISDIR = 3,  ENOSYS = 4
ENOSPC  = 5,  EBADF   = 6,  EMFILE = 7,  EACCES = 8
```

This consistent encoding means a single IPC dispatcher loop can handle all
server protocols uniformly.

## 9.8 The syscall::call() Wrapper

All ring-3 servers use a common wrapper for synchronous IPC:

```rust
// In servers/vfs/src/syscall.rs:
pub fn call(to_pid: u32, req: &Msg, reply: &mut Msg, timeout: u64) -> bool {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      17u64,           // SYS_CALL
            in("rdi")      to_pid as u64,
            in("rsi")      req  as *const Msg as u64,
            in("rdx")      reply as *mut Msg as u64,
            in("r10")      timeout,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret == 0
}
```

The 72-byte `Msg` struct is passed by pointer — the kernel copies from the user
buffer with STAC/CLAC protection.

## 9.9 The Short-Form IPC Syscalls

For simple two-word messages (e.g., uart-drv forwarding a keystroke byte):

```rust
// SYS_SEND (3): send two words to a process
// SYS_RECV (4): blocking-receive one word with timeout
// SYS_NOTIFY (5): OR bits into notification word
// SYS_SEND_MSG (7): send a full 72-byte message
// SYS_RECV_MSG (6): receive a full 72-byte message
```

`SYS_SEND` and `SYS_RECV` are the inner loop of uart-drv's keystroke forwarding.
They don't copy a full 72-byte message — they just transfer two `u64` values.
This minimizes latency for high-frequency I/O.

## 9.10 Summary

Rost's IPC subsystem provides:

- **Unforgeable sender** — kernel stamps PID before enqueue; cannot be spoofed
- **Synchronous call/reply** — `SYS_CALL` with deadlock detection and timeout
- **Capabilities** — unforgeable access tokens; named endpoint model
- **Shared memory** — zero-copy bulk transfer via `SYS_MAP_SHARE` + `SYS_MAP_CAP`
- **Rate limiting** — per-process IPC rate limits prevent flooding
- **Notification word** — lightweight bitmask for high-frequency signaling
- **Audit log** — 64-entry ring buffer of all IPC events
- **Priority inheritance** — servers inherit waiter priorities via `waiting_for`
