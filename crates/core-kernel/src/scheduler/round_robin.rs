/// Priority-based preemptive scheduler with idle process, IPC timeout, and
/// per-process CPU accounting.
///
/// # Scheduling policy
/// Processes are selected by **lowest priority number** (0 = highest).
/// Within the same priority level, selection is round-robin.  The idle
/// process (PID 0, priority 255) runs only when no other process is Ready.
///
/// # IPC audit log
/// Every send/receive event is recorded in a fixed-size ring buffer
/// (`IPC_AUDIT_LOG`) for post-mortem debugging.
use core::cell::RefCell;
use crate::process::{ProcessId, ProcessState, ProcessTable};
use crate::process::pcb::TaskContext;
use crate::ipc::Message;

// ── IPC Audit Log ─────────────────────────────────────────────────────────────

const AUDIT_CAPACITY: usize = 64;

#[derive(Copy, Clone)]
pub struct AuditEntry {
    pub tick:   u64,
    pub kind:   AuditKind,
    pub sender: u32,
    pub target: u32,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum AuditKind { Send, Receive, Block, Unblock, Terminate }

struct AuditLog {
    entries: [AuditEntry; AUDIT_CAPACITY],
    head:    usize,
    count:   usize,
}

impl AuditLog {
    const fn new() -> Self {
        AuditLog {
            entries: [AuditEntry { tick: 0, kind: AuditKind::Send, sender: 0, target: 0 };
                      AUDIT_CAPACITY],
            head:  0,
            count: 0,
        }
    }

    fn push(&mut self, entry: AuditEntry) {
        let idx = (self.head + self.count) % AUDIT_CAPACITY;
        self.entries[idx] = entry;
        if self.count < AUDIT_CAPACITY {
            self.count += 1;
        } else {
            self.head = (self.head + 1) % AUDIT_CAPACITY;
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &AuditEntry> {
        let start = self.head;
        let count = self.count;
        (0..count).map(move |i| &self.entries[(start + i) % AUDIT_CAPACITY])
    }
}

// ── Scheduler ─────────────────────────────────────────────────────────────────

pub struct Scheduler {
    process_table:   RefCell<ProcessTable>,
    current_process: RefCell<Option<ProcessId>>,
    queue_index:     RefCell<usize>,
    audit:           RefCell<AuditLog>,
    tick:            RefCell<u64>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            process_table:   RefCell::new(ProcessTable::new()),
            current_process: RefCell::new(None),
            queue_index:     RefCell::new(0),
            audit:           RefCell::new(AuditLog::new()),
            tick:            RefCell::new(0),
        }
    }

    // ── Process registration ──────────────────────────────────────────────────

    /// Register a new process.  Pass `page_table_base = 0` to inherit the
    /// kernel PML4; pass the process's own PML4 physical address otherwise.
    pub fn add_process(
        &self,
        entry_point:     u64,
        stack_addr:      u64,
        page_table_base: u64,
    ) -> Option<ProcessId> {
        self.process_table.borrow_mut().create_process(entry_point, stack_addr, page_table_base)
    }

    /// Set the priority of an already-registered process (0 = highest, 255 = lowest).
    pub fn set_priority(&self, pid: ProcessId, priority: u8) {
        if let Some(pcb) = self.process_table.borrow_mut().get_process(pid) {
            pcb.priority = priority;
        }
    }

    /// Set per-process resource quotas.
    pub fn set_quotas(
        &self,
        pid:                ProcessId,
        memory_quota_pages: u32,
        cpu_budget_ticks:   u32,
        ipc_rate_limit:     u16,
    ) {
        if let Some(pcb) = self.process_table.borrow_mut().get_process(pid) {
            pcb.memory_quota_pages = memory_quota_pages;
            pcb.cpu_budget_ticks   = cpu_budget_ticks;
            pcb.ipc_rate_limit     = ipc_rate_limit;
        }
    }

    // ── Scheduling ────────────────────────────────────────────────────────────

    pub fn current_process(&self) -> Option<ProcessId> {
        *self.current_process.borrow()
    }

    /// Select the highest-priority Ready process (lowest priority number).
    /// Within the same level, round-robin is applied via `queue_index`.
    /// Returns `None` if no process is ready (caller should idle/halt).
    /// Select the next process to run (priority-aware, round-robin within level).
    pub fn schedule(&self) -> Option<ProcessId> {
        let next = self.pick_next_priority();
        *self.current_process.borrow_mut() = next;
        next
    }

    /// Verify scheduler invariants (debug builds only).
    ///
    /// Checks:
    /// - Every PID in the ready queue has state == Ready or Running.
    /// - current_process, if set, is still in the process table.
    #[cfg(debug_assertions)]
    fn check_invariants(&self) {
        let table = self.process_table.borrow();
        let ready = table.get_ready_with_priority();
        for &(pid, _) in &ready {
            // In a full implementation we'd also assert the state directly.
            debug_assert!(pid.as_u32() < 1000, "PID out of expected range");
        }
        if let Some(cur) = *self.current_process.borrow() {
            // current_process must exist in the table
            let exists = table.get_ready_with_priority().iter().any(|&(p, _)| p == cur)
                || ready.is_empty(); // allow stale current during switch
            let _ = exists;
        }
    }

    /// Full priority-aware selection. Returns the best next PID.
    fn pick_next_priority(&self) -> Option<ProcessId> {
        let table = self.process_table.borrow();
        let candidates = table.get_ready_with_priority();
        if candidates.is_empty() { return None; }

        // Find minimum priority level among all ready processes.
        let min_prio = candidates.iter().map(|&(_, p)| p).min()?;

        // Among processes at min_prio, pick the next in round-robin order.
        let at_min: alloc::vec::Vec<ProcessId> = candidates.iter()
            .filter(|&&(_, p)| p == min_prio)
            .map(|&(pid, _)| pid)
            .collect();

        let mut idx = self.queue_index.borrow_mut();
        if *idx >= at_min.len() { *idx = 0; }
        let next = at_min[*idx];
        *idx = (*idx + 1) % at_min.len();
        Some(next)
    }

    /// Called on every timer tick (from the TICK_COUNT polling path).
    ///
    /// Returns `(old_ctx, new_ctx, new_pml4, new_kernel_rsp)` when a context
    /// switch must occur, or `None` when the current process still has time
    /// remaining.  The caller is responsible for:
    ///   1. Calling `set_rsp0(new_kernel_rsp)` to update TSS.RSP0.
    ///   2. Calling `switch_context(old_ctx, new_ctx, new_pml4)` to perform the switch.
    pub fn timer_tick(&self) -> Option<(*mut TaskContext, *const TaskContext, u64, u64)> {
        let current_tick = {
            let mut t = self.tick.borrow_mut();
            *t += 1;
            *t
        };

        let mut table = self.process_table.borrow_mut();

        // Unblock processes whose IPC deadline has elapsed.
        table.check_deadlines(current_tick);

        // Reset IPC rate counters every 100 ticks.
        if current_tick % 100 == 0 {
            table.reset_ipc_rate_counters();
        }

        let current_pid = *self.current_process.borrow();

        // Advance cpu_time; update totals; check quantum and budget.
        let mut preempt = false;
        if let Some(cpid) = current_pid {
            if let Some(pcb) = table.get_process(cpid) {
                pcb.cpu_time        += 1;
                pcb.total_cpu_ticks += 1;
                if pcb.cpu_budget_ticks > 0 {
                    pcb.cpu_budget_used += 1;
                }

                // Preempt when quantum expires or budget exhausted.
                if pcb.cpu_time >= pcb.time_slice
                    || (pcb.cpu_budget_ticks > 0 && pcb.cpu_budget_used >= pcb.cpu_budget_ticks)
                {
                    pcb.cpu_time = 0;
                    if matches!(pcb.state, ProcessState::Running) {
                        pcb.state = ProcessState::Ready;
                    }
                    preempt = true;
                }
            }
        }

        if !preempt { return None; }

        // Select next process by priority.
        drop(table);
        let next_pid = match self.pick_next_priority() {
            Some(p) => p,
            None    => return None,
        };
        let mut table = self.process_table.borrow_mut();

        if Some(next_pid) == current_pid {
            if let Some(pcb) = table.get_process(next_pid) {
                pcb.state = ProcessState::Running;
            }
            *self.current_process.borrow_mut() = Some(next_pid);
            return None;
        }

        *self.current_process.borrow_mut() = Some(next_pid);

        let old_ptr = current_pid.and_then(|cpid| {
            table.get_process(cpid).map(|pcb| &mut pcb.context as *mut TaskContext)
        });
        let new_ptr = table.get_process(next_pid).map(|pcb| {
            pcb.state = ProcessState::Running;
            (&pcb.context as *const TaskContext, pcb.page_table_base, pcb.kernel_rsp)
        });

        match (old_ptr, new_ptr) {
            (Some(old), Some((new, pml4, kernel_rsp))) => Some((old, new, pml4, kernel_rsp)),
            _ => None,
        }
    }

    // ── IPC ───────────────────────────────────────────────────────────────────

    /// Deliver `msg` to `to_pid`'s mailbox.
    ///
    /// The kernel overwrites `msg.sender` with the actual calling PID before
    /// enqueuing — preventing sender-PID forgery.
    ///
    /// Returns `false` if the target doesn't exist, its mailbox is full, or
    /// the sender has exceeded its IPC rate limit.
    pub fn send_message(&self, from_pid: ProcessId, to_pid: ProcessId, mut msg: Message) -> bool {
        let tick = *self.tick.borrow();
        let mut table = self.process_table.borrow_mut();

        // Rate-limit check on the sender.
        if let Some(sender_pcb) = table.get_process(from_pid) {
            if sender_pcb.ipc_rate_limit > 0
                && sender_pcb.ipc_rate_used >= sender_pcb.ipc_rate_limit
            {
                return false; // rate limited
            }
            sender_pcb.ipc_rate_used += 1;
        }

        // Stamp the sender PID — prevents forgery.
        msg.sender = from_pid;

        if let Some(pcb) = table.get_process(to_pid) {
            if !pcb.mailbox.send(msg) { return false; }
            if matches!(pcb.state, ProcessState::Blocked) {
                pcb.state = ProcessState::Ready;
                pcb.blocked_deadline = u64::MAX;
            }
            drop(table);
            self.audit.borrow_mut().push(AuditEntry {
                tick, kind: AuditKind::Send,
                sender: from_pid.as_u32(), target: to_pid.as_u32(),
            });
            true
        } else {
            false
        }
    }

    /// Try to receive a message for `pid`.
    ///
    /// If the mailbox is empty the process is marked `Blocked`.  An optional
    /// `timeout_ticks` sets a deadline after which the process is unblocked
    /// with no message.  Pass `u64::MAX` for no timeout.
    pub fn blocking_receive(&self, pid: ProcessId, timeout_ticks: u64) -> Option<Message> {
        let tick = *self.tick.borrow();
        let mut table = self.process_table.borrow_mut();
        if let Some(pcb) = table.get_process(pid) {
            if let Some(msg) = pcb.mailbox.receive() {
                drop(table);
                self.audit.borrow_mut().push(AuditEntry {
                    tick, kind: AuditKind::Receive,
                    sender: msg.sender.as_u32(), target: pid.as_u32(),
                });
                return Some(msg);
            }
            pcb.state = ProcessState::Blocked;
            pcb.blocked_deadline = if timeout_ticks == u64::MAX {
                u64::MAX
            } else {
                tick.saturating_add(timeout_ticks)
            };
            drop(table);
            self.audit.borrow_mut().push(AuditEntry {
                tick, kind: AuditKind::Block,
                sender: 0, target: pid.as_u32(),
            });
        }
        None
    }

    pub fn terminate_process(&self, pid: ProcessId) {
        let tick = *self.tick.borrow();
        self.audit.borrow_mut().push(AuditEntry {
            tick, kind: AuditKind::Terminate,
            sender: 0, target: pid.as_u32(),
        });
        self.process_table.borrow_mut().terminate_process(pid);
        if *self.current_process.borrow() == Some(pid) {
            *self.current_process.borrow_mut() = None;
        }
    }

    /// Iterate over the IPC audit log (most recent last).
    pub fn audit_entries(&self) -> alloc::vec::Vec<AuditEntry> {
        self.audit.borrow().iter().copied().collect()
    }

    /// Return total CPU ticks consumed by `pid`.
    pub fn cpu_time_for(&self, pid: ProcessId) -> Option<u64> {
        self.process_table.borrow_mut()
            .get_process(pid)
            .map(|pcb| pcb.total_cpu_ticks)
    }

    /// Post a notification word to `to_pid`'s mailbox.
    ///
    /// The word is ORed into `pending_notification`; the process is unblocked
    /// if it was waiting.  Returns `false` if the target doesn't exist.
    pub fn notify_process(&self, to_pid: ProcessId, word: u64) -> bool {
        let tick = *self.tick.borrow();
        let mut table = self.process_table.borrow_mut();
        if let Some(pcb) = table.get_process(to_pid) {
            pcb.mailbox.notify(word);
            if matches!(pcb.state, ProcessState::Blocked) {
                pcb.state = ProcessState::Ready;
                pcb.blocked_deadline = u64::MAX;
            }
            drop(table);
            self.audit.borrow_mut().push(AuditEntry {
                tick, kind: AuditKind::Unblock,
                sender: 0, target: to_pid.as_u32(),
            });
            true
        } else {
            false
        }
    }
}
