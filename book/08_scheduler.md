# Chapter 8 — The Scheduler

## 8.1 Overview

The scheduler decides which process runs on the CPU at any given moment.  In a
safety-critical system, the scheduler must guarantee that:

- Every process with work to do eventually runs (no starvation)
- High-priority processes are not indefinitely delayed by low-priority ones
- Real-time tasks meet their deadlines
- No process can monopolize the CPU (temporal partitioning)
- The scheduler's own execution time is bounded (no unbounded loops on the hot path)

Rost's scheduler is a three-tier priority system:

1. **Real-Time tier (EDF)** — processes with `rt_period > 0` are scheduled by
   Earliest Deadline First.  They always preempt the lower tiers.
2. **Priority tier** — processes with `rt_period = 0` are scheduled by lowest
   priority number (0 = highest priority, 255 = lowest).  Within the same priority
   level, round-robin is used.
3. **Idle process** — PID 2, priority 255.  Runs only when no other process is Ready.

## 8.2 The `Scheduler` Structure

```rust
pub struct Scheduler {
    process_table:   RefCell<ProcessTable>,
    current_process: RefCell<Option<ProcessId>>,
    queue_index:     RefCell<usize>,      // for round-robin within a priority level
    tick:            RefCell<u64>,        // internal tick counter
    audit:           RefCell<AuditLog>,   // IPC event log (64 entries)
}
```

The `RefCell` wrappers provide interior mutability.  Since Rost is single-core and
the scheduler runs with interrupts disabled on the hot path, there are no actual
concurrent accesses — `RefCell` just satisfies the borrow checker's demand for
a single mutable reference at a time.

The scheduler is stored in a `static mut GLOBAL_SCHEDULER: Option<Scheduler>`.
Access is through `get_global()` which returns an `Option<&'static Scheduler>`.

## 8.3 The Timer Tick: `timer_tick()`

`timer_tick()` is called from the timer ISR at 100 Hz.  It is the heart of the
scheduler.

```rust
pub fn timer_tick(
    &self,
) -> (*mut TaskContext, *const TaskContext, u64, u64) {
    let mut ptable = self.process_table.borrow_mut();
    let mut tick = self.tick.borrow_mut();
    *tick += 1;

    // 1. Unblock processes whose deadline has elapsed
    ptable.check_deadlines(*tick);

    // 2. Reset IPC rate counters every 100 ticks (1 second)
    if *tick % 100 == 0 {
        ptable.reset_ipc_rate_counters();
    }

    // 3. Reset CPU budget counters every 1000 ticks (10 seconds)
    if *tick % CPU_BUDGET_FRAME_TICKS == 0 {
        ptable.reset_cpu_budget_counters();
    }

    // 4. Advance current process's cpu_time and cpu_budget_used
    let current = self.current_process.borrow();
    if let Some(pid) = *current {
        if let Some(pcb) = ptable.get_process(pid) {
            pcb.cpu_time = pcb.cpu_time.saturating_add(1);
            pcb.cpu_budget_used = pcb.cpu_budget_used.saturating_add(1);
            pcb.total_cpu_ticks = pcb.total_cpu_ticks.saturating_add(1);

            // 5. Preempt if quantum expired OR budget exhausted
            let budget_expired = pcb.cpu_budget_ticks > 0
                && pcb.cpu_budget_used >= pcb.cpu_budget_ticks;
            let quantum_expired = pcb.cpu_time >= pcb.time_slice;

            if !quantum_expired && !budget_expired {
                return NO_SWITCH_RETURN; // continue running
            }
            pcb.cpu_time = 0;
            pcb.state = ProcessState::Ready;
        }
    }

    // 6. Pick the next process to run
    drop(current);
    let (old_ctx, new_ctx, new_pml4, new_kern_rsp) = self.pick_and_switch(&mut ptable, *tick);
    (old_ctx, new_ctx, new_pml4, new_kern_rsp)
}
```

### 8.3.1 The Critical Section

Notice that `timer_tick()` is called from the timer ISR.  The ISR runs with
interrupts disabled (the timer IDT entry is an "interrupt gate" — clears IF).
This means `timer_tick()` runs as an atomic critical section: no other timer ISR
can fire and no preemption can occur mid-tick.  On a single-core system, this
is sufficient to guarantee mutual exclusion without spinlocks.

## 8.4 Process Selection: `pick_next_priority()`

```rust
fn pick_next_priority(
    ptable: &ProcessTable,
    tick:   u64,
    qi:     &mut usize,
) -> Option<ProcessId> {
    // Tier 1: EDF — find the ready RT process with the earliest deadline
    let mut best_edf: Option<(ProcessId, u64)> = None;
    for (idx, slot) in ptable.processes.iter().enumerate() {
        if let Some(pcb) = slot {
            if matches!(pcb.state, ProcessState::Ready)
               && pcb.rt_period > 0
            {
                let deadline = pcb.rt_deadline;
                match best_edf {
                    None => best_edf = Some((pcb.pid, deadline)),
                    Some((_, d)) if deadline < d => best_edf = Some((pcb.pid, deadline)),
                    _ => {}
                }
            }
        }
    }
    if let Some((pid, _)) = best_edf { return Some(pid); }

    // Tier 2: Priority + donation — collect all ready non-RT processes
    // with their effective priority (accounting for donation)
    let mut ready: ProcList<(ProcessId, u8)> = ProcList::new();
    let donated = ptable.get_blocked_waiters();  // donation table

    for slot in ptable.processes.iter() {
        if let Some(pcb) = slot {
            if matches!(pcb.state, ProcessState::Ready) && pcb.rt_period == 0 {
                let effective = donated.get(pcb.pid).copied()
                    .map(|donated_prio| donated_prio.min(pcb.priority))
                    .unwrap_or(pcb.priority);
                ready.push((pcb.pid, effective));
            }
        }
    }

    if ready.is_empty() { return None; }

    // Find lowest priority number (= highest priority)
    let best_prio = ready.iter().map(|&(_, p)| p).min()?;

    // Round-robin among equal-priority candidates
    let candidates: ProcList<ProcessId> = ready.iter()
        .filter(|&&(_, p)| p == best_prio)
        .map(|&(pid, _)| pid)
        .collect();

    *qi %= candidates.len();
    let chosen = candidates[*qi];
    *qi = (*qi + 1) % candidates.len();
    Some(chosen)
}
```

### 8.4.1 Priority Donation (Inheritance)

The `get_blocked_waiters()` method builds a map from server PIDs to the best
(lowest-number) priority among all processes currently blocked waiting for them.

Example: if process A (priority 10) and process B (priority 100) are both blocked
waiting for server S (priority 128), `get_blocked_waiters()` returns `{S: 10}`.
Server S's effective priority becomes `min(128, 10) = 10` — it runs as if it had
priority 10 until it unblocks A.

This prevents the classic priority inversion: without inheritance, a medium-priority
process (priority 50) could preempt S and delay A indefinitely.

## 8.5 Real-Time Scheduling (EDF)

Earliest Deadline First (EDF) is optimal for single-processor real-time scheduling:
given a set of independent periodic tasks with known deadlines, EDF achieves 100%
CPU utilization while meeting all deadlines.

A process opts into EDF scheduling by calling `SYS_SETRT` (syscall 16):

```rust
pub fn set_realtime(&self, pid: ProcessId, period_ticks: u64) {
    let tick = *self.tick.borrow();
    if let Some(pcb) = self.process_table.borrow_mut().get_process(pid) {
        pcb.rt_period   = period_ticks;
        pcb.rt_deadline = if period_ticks > 0 { tick + period_ticks } else { 0 };
    }
}
```

Deadline renewal in `timer_tick()`:

```rust
// If an RT process has passed its deadline, renew it
if pcb.rt_period > 0 && *tick >= pcb.rt_deadline {
    pcb.rt_deadline += pcb.rt_period;  // no drift: advance by exactly one period
}
```

The no-drift renewal (`rt_deadline += period` rather than `rt_deadline = now + period`)
ensures that deadline jitter does not accumulate.  If a process misses its deadline
(it didn't finish within its period), the deadline advances by exactly one period,
giving the task its full next-period allocation.

## 8.6 Cooperative Yield: `yield_switch()`

A process can voluntarily give up the CPU with `SYS_YIELD` (syscall 0):

```rust
pub fn yield_switch(&self) -> Option<()> {
    // Reset current process's quantum
    if let Some(pid) = *self.current_process.borrow() {
        if let Some(pcb) = self.process_table.borrow_mut().get_process(pid) {
            pcb.cpu_time = 0;
            pcb.state = ProcessState::Ready;
        }
    }
    // Immediately pick the next process and switch
    let (old, new, pml4, kern_rsp) = self.pick_and_switch_now();
    if let Some((o, n, p, k)) = (old, new, pml4, kern_rsp) {
        set_rsp0(k);
        unsafe { switch_context_noints(o, n, p); }
    }
    // Arm one-shot LAPIC timer to keep deadline wakeups working
    arm_oneshot(1);
    Some(())
}
```

The `arm_oneshot(n)` call programs the LAPIC one-shot timer to fire in `n` ticks.
This is necessary because `yield_switch` bypasses the normal timer ISR path —
without it, the LAPIC timer might not fire and blocked-with-deadline processes
would not be unblocked.

## 8.7 Blocking Receive: `blocking_receive()`

When a process calls `SYS_RECV_MSG` or `SYS_CALL` and no message is available:

```rust
pub fn blocking_receive(
    &self,
    pid:           ProcessId,
    timeout_ticks: u64,
) -> Option<Message> {
    let mut ptable = self.process_table.borrow_mut();

    // Non-blocking poll (timeout=0)
    if timeout_ticks == 0 {
        return ptable.get_process(pid)?.mailbox.dequeue();
    }

    // Check if a message is already waiting
    if let Some(msg) = ptable.get_process(pid)?.mailbox.dequeue() {
        return Some(msg);
    }

    // Block the process
    let tick = *self.tick.borrow();
    if let Some(pcb) = ptable.get_process(pid) {
        pcb.state = ProcessState::Blocked;
        pcb.blocked_deadline = if timeout_ticks == u64::MAX {
            u64::MAX
        } else {
            tick + timeout_ticks
        };
    }
    drop(ptable);

    // Context switch to another process
    arm_oneshot(1);
    unsafe { switch_context_noints(old_ctx, new_ctx, new_pml4); }

    // When this process is unblocked and rescheduled, execution resumes here
    self.process_table.borrow_mut()
        .get_process(pid)?
        .mailbox.dequeue()
}
```

The `switch_context_noints` variant (no `sti`) is used here because the IRETQ in
the timer ISR will restore RFLAGS (including IF) when this process next runs — no
explicit `sti` is needed.

## 8.8 Deadlock Detection

`SYS_CALL` (syscall 17) sends a message and blocks for a reply.  If two processes
both call `SYS_CALL` to each other simultaneously, they deadlock.  Rost detects this:

```rust
pub fn detect_deadlock(&self, waiter: ProcessId, target: ProcessId) -> bool {
    self.process_table.borrow().detect_cycle(waiter, target)
}

// In ProcessTable:
pub fn detect_cycle(&self, waiter: ProcessId, target: ProcessId) -> bool {
    // O(32) iterative DFS over the waiting_for graph
    let mut visited = [false; MAX_PROCESSES];
    let mut current = target;

    loop {
        let idx = current.as_u32() as usize;
        if idx >= MAX_PROCESSES || visited[idx] { break; }
        visited[idx] = true;

        if current == waiter { return true; }  // cycle found

        match self.processes[idx].as_ref().and_then(|p| p.waiting_for) {
            Some(next) => current = next,
            None => break,
        }
    }
    false
}
```

If a cycle is detected, `SYS_CALL` returns `EDEADLK` (-8) to the caller.

IEC 61508 §7.4.4: every blocking operation must have a bounded wait time.  Deadlock
detection provides a hard guarantee independent of application-level timeouts.

## 8.9 IPC Audit Log

Every send, receive, block, unblock, and terminate event is recorded in a 64-entry
ring buffer:

```rust
pub struct AuditEntry {
    pub tick:   u64,
    pub kind:   AuditKind,  // Send, Receive, Block, Unblock, Terminate
    pub sender: u32,
    pub target: u32,
}
```

The audit log is readable by the shell via the `log` command.  It provides a
post-mortem trace of IPC activity for debugging liveness issues (stuck servers,
lost messages).

## 8.10 Temporal Partitioning

CPU budget quotas implement temporal partitioning — the guarantee that no process
can consume more than a fixed fraction of CPU time over a given window:

```
┌──────────────────────────────────────┐
│ Frame = 1000 ticks (10 seconds)      │
│  Process A: budget=200 ticks (20%)   │
│  Process B: budget=500 ticks (50%)   │
│  Process C: budget=100 ticks (10%)   │
│  Remaining: 200 ticks unbudgeted     │
└──────────────────────────────────────┘
```

When a process's `cpu_budget_used >= cpu_budget_ticks`, it is preempted and
not rescheduled until the frame resets.  The frame resets every 1000 ticks
(`CPU_BUDGET_FRAME_TICKS`) via `reset_cpu_budget_counters()`.

This is a soft guarantee — if a process sleeps for half the frame and then
runs for its entire budget in the second half, it will have consumed 50% in the
last 500 ticks.  For a hard guarantee, EDF with utilization analysis is the
appropriate tool.

## 8.11 Summary

The Rost scheduler implements:

- **Three-tier selection** — RT/EDF preempts priority; idle runs last
- **Priority inheritance** — blocked clients donate priority to servers
- **Temporal partitioning** — CPU budget quotas per process per frame
- **Deadlock detection** — O(32) DFS before every `SYS_CALL` block
- **IPC audit** — 64-entry ring buffer of all IPC events
- **Bounded hot path** — O(N) process selection but N≤32, no allocation
- **True preemption** — timer ISR context switch, not deferred
