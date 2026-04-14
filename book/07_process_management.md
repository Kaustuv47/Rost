# Chapter 7 — Process Management

## 7.1 What Is a Process in Rost?

In Rost, a process is the unit of:
- **Execution** — it has a saved CPU state (registers, stack pointer, instruction pointer)
- **Isolation** — it has its own virtual address space (PML4)
- **Resource ownership** — it has a kernel stack, physical frame quota, CPU budget,
  IPC rate limit, and a capability table

There is no concept of "threads" (shared address space, separate execution contexts)
in the current implementation.  Every execution context is a full process with its
own address space.  This is a deliberate simplification: shared address spaces
require TLB shootdown, memory ordering analysis, and lock hierarchies that add
significant complexity.

## 7.2 Process Identifiers

```rust
/// Newtype wrapper around u32 for type-safe process identification.
///
/// Using a newtype prevents accidentally passing a raw u32 where a
/// ProcessId is expected, and vice versa.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProcessId(u32);

impl ProcessId {
    pub fn new(id: u32) -> Self { ProcessId(id) }
    pub fn as_u32(self) -> u32 { self.0 }
}
```

Well-known PIDs are assigned by spawn order:
| PID | Process |
|-----|---------|
| 1 | rost-init (health monitor) |
| 2 | kernel idle process |
| 3 | uart-drv |
| 4 | rost-vfs |
| 5 | rost-shell |
| 6 | rost-net |
| 7 | pci-bus |
| 8 | block-drv |
| 9 | gop |
| 10 | ps2-kbd |

Dynamic processes (spawned by the shell via `exec`) get the next available PID
from the process table.

## 7.3 Process State

```rust
#[derive(Copy, Clone, Debug)]
pub enum ProcessState {
    Ready,       // in the run queue; can be scheduled
    Running,     // currently executing on the CPU
    Blocked,     // waiting for IPC or a timeout
    Terminated,  // exited; slot will be reclaimed
}
```

State transitions:
- `Ready → Running` — scheduler selects this process for execution
- `Running → Ready` — preempted by timer ISR or voluntary yield
- `Running → Blocked` — called `blocking_receive()` with no available messages
- `Blocked → Ready` — message arrived, or timeout elapsed
- `Running → Terminated` — called `SYS_EXIT`, or faulted in ring-3

## 7.4 The Process Control Block (PCB)

The `ProcessControlBlock` is the complete per-process kernel state:

```rust
pub struct ProcessControlBlock {
    pub pid:              ProcessId,
    pub state:            ProcessState,
    pub priority:         u8,          // 0=highest, 255=lowest, default=128

    // CPU context (saved on every context switch)
    pub context:          TaskContext,

    // Kernel stack
    pub kernel_stack_id:  usize,
    pub kernel_rsp:       u64,         // current top of kernel stack (= TSS.RSP0)

    // Address space
    pub page_table_base:  u64,         // physical address of PML4

    // Scheduling
    pub time_slice:       u32,         // ticks per quantum (default: 10)
    pub cpu_time:         u32,         // ticks consumed this quantum

    // Resource quotas
    pub memory_quota_pages: u32,       // max physical pages (0 = unlimited)
    pub memory_pages_used:  u32,       // pages mapped so far
    pub cpu_budget_ticks:   u32,       // max ticks per 10-second frame (0 = unlimited)
    pub cpu_budget_used:    u32,
    pub ipc_rate_limit:     u16,       // max IPC sends per 100-tick window (0 = unlimited)
    pub ipc_rate_used:      u16,

    // Timing
    pub total_cpu_ticks:    u64,       // lifetime CPU ticks consumed
    pub blocked_deadline:   u64,       // tick to unblock (u64::MAX = no timeout)

    // IPC
    pub mailbox:          MessageQueue, // inline, capacity 16

    // Priority inheritance
    pub waiting_for: Option<ProcessId>, // server this process last sent to

    // Capability table
    pub cap_table: [Capability; 64],   // 64 capability slots

    // User frame tracking (for reclaim on termination)
    pub user_frames:      [u64; 96],
    pub user_frame_count: usize,
    pub pml4_owned:       bool,

    // Real-time scheduling (EDF)
    pub rt_period:   u64,              // EDF period in ticks (0 = best-effort)
    pub rt_deadline: u64,              // absolute deadline tick
}
```

Every field has a specific function.  Let's walk through the important ones.

### 7.4.1 Kernel Stack Fields

Each process has a dedicated 8 KB kernel stack.  The stack is used when:
- The process calls a syscall (syscall_entry switches to the kernel stack)
- The process is interrupted (timer/IRQ uses RSP0 as the stack top)
- The process executes kernel code (ISR handlers, context switch path)

`kernel_stack_id` identifies which of the 32 pre-allocated stack slots this
process uses.  `kernel_rsp` is the current top of the stack (= TSS.RSP0 when
this process is running).

### 7.4.2 Time Slice and CPU Time

The scheduler gives each process `time_slice` ticks before preempting it.  The
default time slice is 10 ticks (100 ms at 100 Hz).  `cpu_time` counts the number
of ticks consumed in the current quantum.  When `cpu_time >= time_slice`, the
process is preempted in the next timer ISR.

### 7.4.3 Resource Quotas

Resource quotas implement IEC 61508 §7.4.5 (resource partitioning):

- **`memory_quota_pages`** — maximum physical pages this process can map.
  Checked at every `SYS_MAP` and `SYS_MAP_SHARE` call.  0 = unlimited (kernel processes).
- **`cpu_budget_ticks`** — maximum CPU ticks per 10-second frame.  When the budget
  is exhausted, the process is preempted and not rescheduled until the frame resets.
- **`ipc_rate_limit`** — maximum IPC messages per 100-tick window.  Prevents a
  misbehaving server from flooding the IPC subsystem.

### 7.4.4 Blocked Deadline

`blocked_deadline` enables timeout-based unblocking.  When a process calls
`blocking_receive(pid, timeout_ticks)` with a non-zero timeout, the scheduler sets:

```
pcb.blocked_deadline = current_tick + timeout_ticks
```

The timer ISR calls `check_deadlines(current_tick)` which unblocks any process
whose deadline has elapsed.  Setting `blocked_deadline = u64::MAX` means "no
timeout" — the process remains blocked until a message arrives.

### 7.4.5 Priority Inheritance

The `waiting_for` field implements priority inheritance, which prevents priority
inversion — a scenario where a high-priority process is blocked waiting for a
low-priority server that keeps getting preempted by medium-priority processes.

When process A sends a message to server B and blocks, `waiting_for` is set to B's PID.
The scheduler's `pick_next_priority()` scans all blocked processes and "donates" each
one's priority to the process it's waiting for.  The effective priority of B becomes:
`min(B.priority, A.priority)` — as high as the highest-priority waiter.

This is required by IEC 61508 for SIL-3/4 systems that have priority-based scheduling.

### 7.4.6 Capability Table

The capability table (`cap_table`) contains 64 `Capability` entries.  Each capability
grants the holder the right to use a specific kernel object.  See Chapter 9 (IPC)
for a full description of the capability system.

## 7.5 The Process Table

The process table is a fixed-size array of optional PCBs:

```rust
pub struct ProcessTable {
    processes: [Option<ProcessControlBlock>; 32],
    next_pid:  u32,
}
```

This design has several advantages over a dynamic allocation:
- **O(1) access by PID** — `processes[pid.as_u32() as usize]`
- **No heap** — the table lives in BSS; no allocator needed
- **Bounded size** — the kernel can never have more than 32 processes,
  which is deterministic for IEC 61508 analysis

### 7.5.1 `create_ring3_process()`

Creating a ring-3 process involves:

1. Allocating the next PID
2. Calling `PCB::new_ring3(pid, user_entry, user_stack_top, pml4_phys, trampoline)`
3. Storing the PCB in the process table
4. Adding the PID to the scheduler's ready queue

```rust
pub fn create_ring3_process(
    &mut self,
    user_entry:      u64,
    user_stack_top:  u64,
    page_table_base: u64,
    trampoline_addr: u64,
) -> Option<ProcessId> {
    let pid = self.alloc_pid()?;
    let pcb = ProcessControlBlock::new_ring3(
        pid, user_entry, user_stack_top, page_table_base, trampoline_addr
    )?;
    self.processes[pid.as_u32() as usize] = Some(pcb);
    Some(pid)
}
```

### 7.5.2 `terminate_process()`

Terminating a process:

1. Looks up the PCB
2. Sets state to `Terminated`
3. Removes it from the service registry (so no stale PIDs remain discoverable)
4. Calls `service_registry::unregister_pid(pid)`
5. Sets `processes[idx] = None` — drops the PCB, which fires `Drop`

```rust
pub fn terminate_process(&mut self, pid: ProcessId) -> bool {
    let idx = pid.as_u32() as usize;
    if idx >= MAX_PROCESSES { return false; }
    if self.processes[idx].is_none() { return false; }
    // Unregister any service names this process registered
    crate::service_registry::unregister_pid(pid.as_u32());
    // Drop the PCB — fires ProcessControlBlock::drop()
    self.processes[idx] = None;
    true
}
```

The PCB's `Drop` impl (described in Chapter 3) reclaims:
- The kernel stack slot (returned to reclaim pool)
- All user physical frames (returned to bitmap allocator)
- The PML4 frame (if owned)

## 7.6 The `ProcList<T>` Type

The scheduler's hot path (picking the next process) cannot allocate memory.
Instead of using `Vec<ProcessId>`, it uses a fixed-capacity stack-allocated list:

```rust
pub struct ProcList<T: Copy> {
    buf: [MaybeUninit<T>; MAX_PROCESSES],
    len: usize,
}
```

`ProcList<ProcessId>` is used in `get_ready_with_priority()` to collect all
ready processes without touching the heap.  The `len` field ensures that only
initialized entries are accessed.

## 7.7 Resource Quotas in Practice

Resource quotas are applied via `SYS_SETQUOTA` (syscall 19):

```rust
// Set memory quota: max 1024 pages (4 MB)
syscall::setquota(pid, 1024, 0, 0);

// Set CPU budget: max 500 ticks per 10-second frame (5%)
syscall::setquota(pid, 0, 500, 0);

// Set IPC rate limit: max 100 messages per 100-tick window
syscall::setquota(pid, 0, 0, 100);
```

These can be set at any time during the process's lifetime.  They take effect
at the next scheduling event for CPU budgets, and at the next allocation for
memory quotas.

## 7.8 The Idle Process

```rust
extern "C" fn idle_process() -> ! {
    arch_x86_64::cpu::enable_interrupts();
    let mut tick_count: u64 = 0;
    loop {
        arch_x86_64::cpu::halt();      // hlt — wait for next interrupt
        tick_count += 1;
        if tick_count % 50 == 0 {
            hal::watchdog::kick();     // pet the hardware watchdog
        }
    }
}
```

The idle process (PID 2, priority 255) runs when no other process is Ready.
Its entire purpose is to:
1. Execute `hlt` so the CPU stops burning power
2. Return when an interrupt fires (the timer or a device IRQ)
3. Pet the hardware watchdog every 500 ms

If the scheduler ever stops delivering timer ticks (e.g., due to a software bug
that leaves interrupts disabled), the watchdog fires after 10 seconds and resets
the hardware.  The idle process is the watchdog's trigger.

## 7.9 The `list_processes()` Function

The `SYS_LIST_PROCS` (28) syscall lets ring-3 processes query the process table.
Each entry is 24 bytes:

```rust
#[repr(C)]
pub struct ProcEntry {
    pid:        u32,
    state:      u32,   // 0=Ready 1=Running 2=Blocked 3=Terminated
    priority:   u8,
    _pad:       [u8; 3],
    cpu_ticks:  u64,   // total lifetime CPU ticks
}
```

The shell's `ps` command calls `SYS_LIST_PROCS` and displays the result in a
formatted table.

## 7.10 Summary

Process management in Rost provides:

- **Isolation** — every ring-3 process has its own PML4 and physical frames
- **Type safety** — `ProcessId` newtype prevents silent PID confusion
- **Resource quotas** — memory, CPU, and IPC limits with kernel enforcement
- **Priority inheritance** — prevents priority inversion for blocked clients
- **Complete reclaim** — PCB Drop reclaims kernel stack, user frames, and PML4
- **Watchdog integration** — idle process pets the hardware watchdog
- **Deterministic capacity** — maximum 32 processes, no dynamic allocation
