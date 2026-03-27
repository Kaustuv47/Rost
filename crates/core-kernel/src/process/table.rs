use super::ProcessId;
use super::pcb::{ProcessControlBlock, ProcessState, Capability, CAP_TABLE_SIZE};

const MAX_PROCESSES: usize = 32;

// ── Heap-free fixed-capacity list ─────────────────────────────────────────────
//
// Replaces Vec<T> on scheduler hot paths so the BumpAllocator is never called
// during a timer tick.  Backed by a stack-allocated MaybeUninit array sized to
// MAX_PROCESSES (the maximum number of simultaneously live processes).
//
// T must be Copy; this is enforced both by the bound and by the array
// initialisation `[MaybeUninit::uninit(); N]` which requires Copy.
//
// IEC 61508 §7.4.5: no dynamic allocation on the scheduling critical path.

/// Fixed-capacity list of up to `MAX_PROCESSES` elements, allocated on the stack.
///
/// Replaces `Vec<T>` on scheduling hot paths so the global allocator is never
/// called during a timer tick.
pub struct ProcList<T: Copy> {
    buf: [core::mem::MaybeUninit<T>; MAX_PROCESSES],
    len: usize,
}

impl<T: Copy> ProcList<T> {
    #[inline]
    pub fn new() -> Self {
        ProcList {
            buf: [core::mem::MaybeUninit::uninit(); MAX_PROCESSES],
            len: 0,
        }
    }

    /// Append `val`.  Silently drops the value if capacity is exhausted.
    #[inline]
    pub fn push(&mut self, val: T) {
        if self.len < MAX_PROCESSES {
            self.buf[self.len].write(val);
            self.len += 1;
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool { self.len == 0 }

    #[inline]
    pub fn len(&self) -> usize { self.len }

    /// View the initialised portion as a slice.
    #[inline]
    pub fn as_slice(&self) -> &[T] {
        // Safety: buf[0..len] have been written via push().
        unsafe { core::slice::from_raw_parts(self.buf.as_ptr().cast::<T>(), self.len) }
    }

    #[inline]
    pub fn iter(&self) -> core::slice::Iter<'_, T> {
        self.as_slice().iter()
    }
}

impl<T: Copy> core::ops::Index<usize> for ProcList<T> {
    type Output = T;
    fn index(&self, i: usize) -> &T { &self.as_slice()[i] }
}

impl<'a, T: Copy> IntoIterator for &'a ProcList<T> {
    type Item = &'a T;
    type IntoIter = core::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter { self.iter() }
}

pub struct ProcessTable {
    processes: [Option<ProcessControlBlock>; MAX_PROCESSES],
    next_pid:  u32,
}

impl ProcessTable {
    pub fn new() -> Self {
        ProcessTable {
            processes: [
                None, None, None, None, None, None, None, None,
                None, None, None, None, None, None, None, None,
                None, None, None, None, None, None, None, None,
                None, None, None, None, None, None, None, None,
            ],
            next_pid: 1,
        }
    }

    pub fn create_process(
        &mut self,
        entry_point:     u64,
        stack_addr:      u64,
        page_table_base: u64,
    ) -> Option<ProcessId> {
        for slot in self.processes.iter_mut() {
            if slot.is_none() {
                let pid = ProcessId::new(self.next_pid);
                *slot = Some(ProcessControlBlock::new(pid, entry_point, stack_addr, page_table_base)?);
                self.next_pid += 1;
                return Some(pid);
            }
        }
        None
    }

    /// Create a ring-3 process using the IRETQ trampoline mechanism.
    ///
    /// `trampoline_addr` must be the address of `ring3_entry_trampoline`.
    /// `user_entry` is the ELF entry point; `user_stack_top` is the allocated
    /// user-mode stack top.
    pub fn create_ring3_process(
        &mut self,
        user_entry:      u64,
        user_stack_top:  u64,
        page_table_base: u64,
        trampoline_addr: u64,
    ) -> Option<ProcessId> {
        for slot in self.processes.iter_mut() {
            if slot.is_none() {
                let pid = ProcessId::new(self.next_pid);
                *slot = Some(ProcessControlBlock::new_ring3(
                    pid, user_entry, user_stack_top, page_table_base, trampoline_addr,
                )?);
                self.next_pid += 1;
                return Some(pid);
            }
        }
        None
    }

    pub fn get_process(&mut self, pid: ProcessId) -> Option<&mut ProcessControlBlock> {
        self.processes.iter_mut()
            .filter_map(|s| s.as_mut())
            .find(|pcb| pcb.pid == pid)
    }

    /// Mark `pid` as Terminated and **reclaim its table slot and kernel stack**.
    ///
    /// - The process table slot is cleared (`Option` set to `None`), releasing
    ///   the PCB and its inline mailbox.
    /// - The kernel stack slot is zeroed and returned to the reclaim pool via
    ///   [`free_kernel_stack`](super::pcb::free_kernel_stack) so it can be
    ///   reused by a future [`create_process`] call without exhausting the
    ///   fixed-size `KERNEL_STACKS` BSS array.
    ///
    /// IEC 61508 §7.4.5: every terminated process must release its allocated
    /// resources back to the kernel before the table slot is cleared.
    pub fn terminate_process(&mut self, pid: ProcessId) {
        for slot in self.processes.iter_mut() {
            if let Some(pcb) = slot.as_ref() {
                if pcb.pid == pid {
                    // Clearing the slot drops the PCB, which calls
                    // `ProcessControlBlock::drop()` → `free_kernel_stack()`.
                    // The kernel stack slot is returned to the reclaim pool
                    // automatically without a separate explicit call.
                    *slot = None;
                    return;
                }
            }
        }
    }

    /// Return `(ProcessId, state_u8, priority)` for every non-`None` slot.
    ///
    /// `state_u8` encoding: 0 = Ready/Running, 1 = Blocked, 2 = Terminated.
    /// Used by `SYS_LIST_PROCS` to snapshot the process list for `ps`.
    pub fn list_all(&self) -> ProcList<(ProcessId, u8, u8)> {
        let mut out = ProcList::new();
        for s in self.processes.iter() {
            if let Some(pcb) = s.as_ref() {
                let state_u8 = match pcb.state {
                    ProcessState::Ready | ProcessState::Running => 0u8,
                    ProcessState::Blocked    => 1u8,
                    ProcessState::Terminated => 2u8,
                };
                out.push((pcb.pid, state_u8, pcb.priority));
            }
        }
        out
    }

    pub fn get_ready_processes(&self) -> ProcList<ProcessId> {
        let mut out = ProcList::new();
        for s in self.processes.iter() {
            if let Some(pcb) = s.as_ref() {
                if matches!(pcb.state, ProcessState::Ready | ProcessState::Running) {
                    out.push(pcb.pid);
                }
            }
        }
        out
    }

    /// Return `(ProcessId, priority)` for every Ready process.
    /// Used by the priority scheduler to pick the highest-priority next task.
    pub fn get_ready_with_priority(&self) -> ProcList<(ProcessId, u8)> {
        let mut out = ProcList::new();
        for s in self.processes.iter() {
            if let Some(pcb) = s.as_ref() {
                if matches!(pcb.state, ProcessState::Ready | ProcessState::Running) {
                    out.push((pcb.pid, pcb.priority));
                }
            }
        }
        out
    }

    /// Unblock any processes whose `blocked_deadline` has elapsed.
    /// Called on every timer tick.
    pub fn check_deadlines(&mut self, current_tick: u64) {
        for slot in self.processes.iter_mut() {
            if let Some(pcb) = slot.as_mut() {
                if matches!(pcb.state, ProcessState::Blocked)
                    && pcb.blocked_deadline <= current_tick
                {
                    pcb.state = ProcessState::Ready;
                    pcb.blocked_deadline = u64::MAX;
                    // Timeout: process is no longer waiting for a specific sender.
                    pcb.waiting_for = None;
                }
            }
        }
    }

    /// Return the smallest `blocked_deadline` among all Blocked processes.
    ///
    /// Returns `u64::MAX` if no process is currently Blocked with a finite
    /// deadline (i.e., all processes are Ready/Running, or all Blocked
    /// processes are waiting indefinitely with `blocked_deadline == u64::MAX`).
    ///
    /// Used by the scheduler's `ticks_until_next_event()` to determine how
    /// many ticks to program into the LAPIC one-shot timer.
    ///
    /// IEC 61508 §7.4.1: temporal partitioning guarantees that blocked
    /// processes are woken no later than their stated deadline.
    pub fn earliest_blocked_deadline(&self) -> u64 {
        self.processes.iter()
            .filter_map(|s| s.as_ref())
            .filter(|pcb| {
                matches!(pcb.state, ProcessState::Blocked)
                    && pcb.blocked_deadline != u64::MAX
            })
            .map(|pcb| pcb.blocked_deadline)
            .fold(u64::MAX, u64::min)
    }

    /// Return `(ProcessId, rt_deadline)` for every Ready real-time process.
    ///
    /// A process is "realtime" when `rt_period > 0`.  These processes are
    /// scheduled by EDF (Earliest Deadline First) and preempt all best-effort
    /// (priority-based) processes.
    pub fn get_ready_realtime(&self) -> ProcList<(ProcessId, u64)> {
        let mut out = ProcList::new();
        for s in self.processes.iter() {
            if let Some(pcb) = s.as_ref() {
                if matches!(pcb.state, ProcessState::Ready | ProcessState::Running)
                    && pcb.rt_period > 0
                {
                    out.push((pcb.pid, pcb.rt_deadline));
                }
            }
        }
        out
    }

    /// Return `(priority, target_pid)` for every Blocked process that has
    /// `waiting_for` set.  Used by the scheduler to compute priority inheritance.
    pub fn get_blocked_waiters(&self) -> ProcList<(u8, ProcessId)> {
        let mut out = ProcList::new();
        for s in self.processes.iter() {
            if let Some(pcb) = s.as_ref() {
                if matches!(pcb.state, ProcessState::Blocked) {
                    if let Some(target) = pcb.waiting_for {
                        out.push((pcb.priority, target));
                    }
                }
            }
        }
        out
    }

    // ── Capability table operations ───────────────────────────────────────────

    /// Store `cap` in the first empty slot of `pid`'s capability table.
    ///
    /// Returns the slot index on success, or `None` if the table is full or
    /// the process does not exist.
    pub fn cap_alloc(&mut self, pid: ProcessId, cap: Capability) -> Option<usize> {
        let pcb = self.get_process(pid)?;
        for (idx, slot) in pcb.cap_table.iter_mut().enumerate() {
            if slot.is_empty() {
                *slot = cap;
                return Some(idx);
            }
        }
        None // table full
    }

    /// Find the first slot in `pid`'s table that matches `kind` and `object_id`.
    ///
    /// Returns `(slot_index, capability)` or `None` if no such capability exists.
    pub fn cap_find(
        &self,
        pid:       ProcessId,
        kind:      super::pcb::CapKind,
        object_id: u32,
    ) -> Option<(usize, Capability)> {
        let pcb = self.processes.iter()
            .filter_map(|s| s.as_ref())
            .find(|p| p.pid == pid)?;
        pcb.cap_table.iter().enumerate()
            .find(|(_, c)| c.kind == kind && c.object_id == object_id)
            .map(|(idx, c)| (idx, *c))
    }

    /// Revoke the capability at `slot_idx` in `pid`'s table.
    ///
    /// The slot is zeroed (set to `Capability::empty()`).
    pub fn cap_revoke(&mut self, pid: ProcessId, slot_idx: usize) {
        if slot_idx >= CAP_TABLE_SIZE { return; }
        if let Some(pcb) = self.get_process(pid) {
            pcb.cap_table[slot_idx] = Capability::empty();
        }
    }

    /// Grant the capability at `slot_idx` of `from_pid`'s table to `to_pid`.
    ///
    /// The capability is copied into the first free slot of `to_pid`'s table.
    /// The transfer only succeeds if the source capability has the `CAP_G` (grant) right.
    ///
    /// Returns `Some(new_slot_idx)` on success; `None` on any failure.
    pub fn cap_grant(
        &mut self,
        from_pid: ProcessId,
        slot_idx: usize,
        to_pid:   ProcessId,
    ) -> Option<usize> {
        if slot_idx >= CAP_TABLE_SIZE { return None; }

        // Extract the source capability without holding a borrow on the table.
        let cap = {
            let from = self.get_process(from_pid)?;
            let c = from.cap_table[slot_idx];
            if c.is_empty() { return None; }
            if c.rights & super::pcb::CAP_G == 0 { return None; } // no grant right
            c
        };

        // Store in the recipient's first empty slot.
        self.cap_alloc(to_pid, cap)
    }

    /// Reset per-frame IPC rate counters (call at the start of each 100-tick window).
    pub fn reset_ipc_rate_counters(&mut self) {
        for slot in self.processes.iter_mut() {
            if let Some(pcb) = slot.as_mut() {
                pcb.ipc_rate_used = 0;
            }
        }
    }

    /// Reset per-major-frame CPU budget counters.
    ///
    /// Called every `CPU_BUDGET_FRAME_TICKS` ticks by the scheduler so that
    /// processes with a `cpu_budget_ticks` quota can consume their allocation
    /// again in the next scheduling frame.
    ///
    /// IEC 61508 §7.4.1 (temporal partitioning): no process may accumulate
    /// CPU time across frames; budgets are window-relative.
    pub fn reset_cpu_budget_counters(&mut self) {
        for slot in self.processes.iter_mut() {
            if let Some(pcb) = slot.as_mut() {
                pcb.cpu_budget_used = 0;
            }
        }
    }

    /// Return an immutable reference to `pid`'s PCB.
    ///
    /// Used by routines that need to inspect multiple PCBs simultaneously
    /// without a mutable borrow (e.g., cycle detection).
    pub fn get_process_ref(&self, pid: ProcessId) -> Option<&ProcessControlBlock> {
        self.processes.iter()
            .filter_map(|s| s.as_ref())
            .find(|pcb| pcb.pid == pid)
    }

    /// Detect a cycle in the `waiting_for` wait-for graph.
    ///
    /// Returns `true` iff adding the directed edge `waiter → target` (i.e.,
    /// `waiter` is about to block waiting for `target`) would create a cycle,
    /// meaning `target` already transitively waits for `waiter`.
    ///
    /// # Algorithm
    /// Iterative traversal from `target`, following `waiting_for` links.  A
    /// `visited` bitmap (one bit per process slot) prevents looping on
    /// already-existing cycles.  With `MAX_PROCESSES == 32` this is O(32) —
    /// fully deterministic, zero heap allocation, bounded execution time.
    ///
    /// # IEC 61508 §7.4.4
    /// Blocking a process when the resulting wait graph contains a cycle creates
    /// a deadlock: the involved processes will never run again.  Detecting and
    /// rejecting such blocks prevents permanent scheduling starvation without
    /// requiring any external timeout.
    pub fn detect_cycle(&self, waiter: ProcessId, target: ProcessId) -> bool {
        let mut visited = [false; MAX_PROCESSES];
        let mut current = target;

        loop {
            // Locate current's slot index in the flat array.
            let slot_idx = match self.processes.iter().position(|s| {
                s.as_ref().map_or(false, |p| p.pid == current)
            }) {
                Some(i) => i,
                None    => return false, // process not found — no path
            };

            if visited[slot_idx] {
                // We've been here before: the existing graph has a cycle that
                // does not involve `waiter` — safe to block.
                return false;
            }
            visited[slot_idx] = true;

            match &self.processes[slot_idx] {
                None => return false,
                Some(pcb) => match pcb.waiting_for {
                    None       => return false, // no outgoing edge — no path to waiter
                    Some(next) => {
                        if next == waiter {
                            return true; // cycle: adding waiter→target closes the loop
                        }
                        current = next;
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::pcb::{CapKind, CAP_R, CAP_W, CAP_G};

    /// create_process allocates a new slot and returns a valid PID.
    #[test]
    fn test_create_and_get() {
        let mut table = ProcessTable::new();
        let pid = table.create_process(0x1000, 0, 0).expect("create failed");
        let pcb = table.get_process(pid).expect("get failed");
        assert_eq!(pcb.pid, pid);
        assert!(matches!(pcb.state, ProcessState::Ready));
    }

    /// PIDs are unique and monotonically increasing within the same table.
    #[test]
    fn test_unique_pids() {
        let mut table = ProcessTable::new();
        let p1 = table.create_process(0x1000, 0, 0).unwrap();
        let p2 = table.create_process(0x2000, 0, 0).unwrap();
        let p3 = table.create_process(0x3000, 0, 0).unwrap();
        assert_ne!(p1, p2);
        assert_ne!(p2, p3);
        assert_ne!(p1, p3);
    }

    /// get_ready_with_priority returns only Ready/Running processes.
    #[test]
    fn test_get_ready_with_priority() {
        let mut table = ProcessTable::new();
        let p1 = table.create_process(0x1000, 0, 0).unwrap();
        let p2 = table.create_process(0x2000, 0, 0).unwrap();
        // Mark p2 Terminated.
        if let Some(pcb) = table.get_process(p2) {
            pcb.state = ProcessState::Terminated;
        }
        let ready = table.get_ready_with_priority();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].0, p1);
    }

    /// terminate_process clears the slot; the PID is no longer findable.
    #[test]
    fn test_terminate_clears_slot() {
        let mut table = ProcessTable::new();
        let pid = table.create_process(0x1000, 0, 0).unwrap();
        assert!(table.get_process(pid).is_some());
        table.terminate_process(pid);
        assert!(table.get_process(pid).is_none());
    }

    /// A terminated slot can be reused by a subsequent create_process call.
    #[test]
    fn test_slot_reuse_after_terminate() {
        let mut table = ProcessTable::new();
        let p1 = table.create_process(0x1000, 0, 0).unwrap();
        table.terminate_process(p1);
        // The slot is now free; a new process should succeed.
        let p2 = table.create_process(0x2000, 0, 0).unwrap();
        assert!(table.get_process(p2).is_some());
    }

    /// check_deadlines unblocks only processes whose deadline has elapsed.
    #[test]
    fn test_check_deadlines() {
        let mut table = ProcessTable::new();
        let p1 = table.create_process(0x1000, 0, 0).unwrap();
        let p2 = table.create_process(0x2000, 0, 0).unwrap();
        // Block both with different deadlines.
        if let Some(pcb) = table.get_process(p1) {
            pcb.state = ProcessState::Blocked;
            pcb.blocked_deadline = 50;
        }
        if let Some(pcb) = table.get_process(p2) {
            pcb.state = ProcessState::Blocked;
            pcb.blocked_deadline = 200;
        }
        // Tick 60: p1 should be unblocked; p2 should remain blocked.
        table.check_deadlines(60);
        assert!(matches!(table.get_process(p1).unwrap().state, ProcessState::Ready));
        assert!(matches!(table.get_process(p2).unwrap().state, ProcessState::Blocked));
    }

    // ── earliest_blocked_deadline tests ──────────────────────────────────────

    /// No blocked processes → MAX sentinel.
    #[test]
    fn test_earliest_deadline_no_blocked() {
        let table = ProcessTable::new();
        assert_eq!(table.earliest_blocked_deadline(), u64::MAX);
    }

    /// All processes Ready → MAX sentinel.
    #[test]
    fn test_earliest_deadline_all_ready() {
        let mut table = ProcessTable::new();
        table.create_process(0x1000, 0, 0).unwrap();
        table.create_process(0x2000, 0, 0).unwrap();
        assert_eq!(table.earliest_blocked_deadline(), u64::MAX);
    }

    /// Single blocked process with a finite deadline → that deadline.
    #[test]
    fn test_earliest_deadline_single_blocked() {
        let mut table = ProcessTable::new();
        let p1 = table.create_process(0x1000, 0, 0).unwrap();
        if let Some(pcb) = table.get_process(p1) {
            pcb.state = ProcessState::Blocked;
            pcb.blocked_deadline = 42;
        }
        assert_eq!(table.earliest_blocked_deadline(), 42);
    }

    /// Multiple blocked processes → minimum deadline is returned.
    #[test]
    fn test_earliest_deadline_multiple_blocked() {
        let mut table = ProcessTable::new();
        let p1 = table.create_process(0x1000, 0, 0).unwrap();
        let p2 = table.create_process(0x2000, 0, 0).unwrap();
        let p3 = table.create_process(0x3000, 0, 0).unwrap();
        if let Some(pcb) = table.get_process(p1) {
            pcb.state = ProcessState::Blocked;
            pcb.blocked_deadline = 100;
        }
        if let Some(pcb) = table.get_process(p2) {
            pcb.state = ProcessState::Blocked;
            pcb.blocked_deadline = 50; // earliest
        }
        if let Some(pcb) = table.get_process(p3) {
            pcb.state = ProcessState::Blocked;
            pcb.blocked_deadline = 200;
        }
        assert_eq!(table.earliest_blocked_deadline(), 50);
    }

    /// Blocked processes with MAX deadline (indefinite wait) are excluded.
    #[test]
    fn test_earliest_deadline_skips_indefinite() {
        let mut table = ProcessTable::new();
        let p1 = table.create_process(0x1000, 0, 0).unwrap();
        let p2 = table.create_process(0x2000, 0, 0).unwrap();
        if let Some(pcb) = table.get_process(p1) {
            pcb.state = ProcessState::Blocked;
            pcb.blocked_deadline = u64::MAX; // indefinite wait
        }
        if let Some(pcb) = table.get_process(p2) {
            pcb.state = ProcessState::Blocked;
            pcb.blocked_deadline = 77;
        }
        assert_eq!(table.earliest_blocked_deadline(), 77);
    }

    // ── Capability table tests ────────────────────────────────────────────────

    /// cap_alloc stores a capability and returns its slot index.
    #[test]
    fn test_cap_alloc_returns_slot() {
        let mut table = ProcessTable::new();
        let pid = table.create_process(0x1000, 0, 0).unwrap();
        let cap = Capability::new(CapKind::Channel, CAP_R | CAP_W | CAP_G, 42);
        let slot = table.cap_alloc(pid, cap).expect("cap_alloc failed");
        assert!(slot < CAP_TABLE_SIZE, "slot index must be in bounds");
    }

    /// cap_alloc and cap_find together allow retrieval by kind + object_id.
    #[test]
    fn test_cap_find_after_alloc() {
        let mut table = ProcessTable::new();
        let pid = table.create_process(0x1000, 0, 0).unwrap();
        let cap = Capability::new(CapKind::Channel, CAP_W, 7);
        let slot = table.cap_alloc(pid, cap).unwrap();
        let (found_slot, found_cap) = table
            .cap_find(pid, CapKind::Channel, 7)
            .expect("cap_find failed");
        assert_eq!(found_slot, slot);
        assert_eq!(found_cap.object_id, 7);
        assert_eq!(found_cap.rights & CAP_W, CAP_W);
    }

    /// cap_grant copies capability to another process; source still exists.
    #[test]
    fn test_cap_grant_copies_to_target() {
        let mut table = ProcessTable::new();
        let p1 = table.create_process(0x1000, 0, 0).unwrap();
        let p2 = table.create_process(0x2000, 0, 0).unwrap();
        let cap = Capability::new(CapKind::Memory, CAP_R | CAP_W | CAP_G, 0x1234);
        let slot1 = table.cap_alloc(p1, cap).unwrap();
        let slot2 = table.cap_grant(p1, slot1, p2).expect("cap_grant failed");
        // Source slot still populated.
        assert!(table.cap_find(p1, CapKind::Memory, 0x1234).is_some());
        // Target slot now has a copy.
        let (_, granted) = table.cap_find(p2, CapKind::Memory, 0x1234).unwrap();
        assert_eq!(granted.object_id, 0x1234);
        assert_eq!(slot2, table.cap_find(p2, CapKind::Memory, 0x1234).unwrap().0);
    }

    /// cap_grant is rejected when the source lacks CAP_G.
    #[test]
    fn test_cap_grant_requires_cap_g() {
        let mut table = ProcessTable::new();
        let p1 = table.create_process(0x1000, 0, 0).unwrap();
        let p2 = table.create_process(0x2000, 0, 0).unwrap();
        let cap = Capability::new(CapKind::Channel, CAP_W, 5); // no CAP_G
        let slot = table.cap_alloc(p1, cap).unwrap();
        assert!(table.cap_grant(p1, slot, p2).is_none(), "grant without CAP_G must fail");
    }

    /// cap_revoke zeroes the slot; cap_find no longer returns it.
    #[test]
    fn test_cap_revoke_clears_slot() {
        let mut table = ProcessTable::new();
        let pid = table.create_process(0x1000, 0, 0).unwrap();
        let cap = Capability::new(CapKind::Channel, CAP_R | CAP_W | CAP_G, 99);
        let slot = table.cap_alloc(pid, cap).unwrap();
        assert!(table.cap_find(pid, CapKind::Channel, 99).is_some());
        table.cap_revoke(pid, slot);
        assert!(table.cap_find(pid, CapKind::Channel, 99).is_none());
    }

    /// cap_alloc fills the table; the 65th allocation fails.
    #[test]
    fn test_cap_table_full_returns_none() {
        let mut table = ProcessTable::new();
        let pid = table.create_process(0x1000, 0, 0).unwrap();
        // Fill all 64 slots.
        for i in 0..CAP_TABLE_SIZE {
            let cap = Capability::new(CapKind::Channel, CAP_W, i as u32);
            assert!(table.cap_alloc(pid, cap).is_some(), "slot {i} should succeed");
        }
        // 65th allocation must fail.
        let cap = Capability::new(CapKind::Channel, CAP_W, 0xFF);
        assert!(table.cap_alloc(pid, cap).is_none(), "overflow must return None");
    }

    /// After cap_revoke a slot is reused by the next cap_alloc.
    #[test]
    fn test_cap_slot_reuse_after_revoke() {
        let mut table = ProcessTable::new();
        let pid = table.create_process(0x1000, 0, 0).unwrap();
        let cap = Capability::new(CapKind::Service, CAP_R, 1);
        let slot = table.cap_alloc(pid, cap).unwrap();
        table.cap_revoke(pid, slot);
        // The same slot (now empty) should be reused by the next alloc.
        let cap2 = Capability::new(CapKind::Service, CAP_R, 2);
        let slot2 = table.cap_alloc(pid, cap2).unwrap();
        assert_eq!(slot, slot2, "revoked slot must be reused");
    }

    // ── detect_cycle tests ────────────────────────────────────────────────────

    /// No waiting_for links → no cycle.
    #[test]
    fn test_detect_cycle_no_links() {
        let mut table = ProcessTable::new();
        let a = table.create_process(0x1000, 0, 0).unwrap();
        let b = table.create_process(0x2000, 0, 0).unwrap();
        assert!(!table.detect_cycle(a, b), "no links → no cycle");
    }

    /// b → c but c has no outgoing edge → adding a → b is safe.
    #[test]
    fn test_detect_cycle_chain_no_cycle() {
        let mut table = ProcessTable::new();
        let a = table.create_process(0x1000, 0, 0).unwrap();
        let b = table.create_process(0x2000, 0, 0).unwrap();
        let c = table.create_process(0x3000, 0, 0).unwrap();
        // b waits for c; c has no outgoing edge.
        if let Some(pcb) = table.get_process(b) {
            pcb.waiting_for = Some(c);
        }
        assert!(!table.detect_cycle(a, b),
            "chain b→c with no link back to a must not be a cycle");
    }

    /// Direct cycle: b waits for a; adding a → b creates a cycle.
    #[test]
    fn test_detect_cycle_direct() {
        let mut table = ProcessTable::new();
        let a = table.create_process(0x1000, 0, 0).unwrap();
        let b = table.create_process(0x2000, 0, 0).unwrap();
        if let Some(pcb) = table.get_process(b) {
            pcb.waiting_for = Some(a);
        }
        assert!(table.detect_cycle(a, b),
            "direct cycle a→b→a must be detected");
    }

    /// Indirect cycle: b → c → a; adding a → b creates a three-party cycle.
    #[test]
    fn test_detect_cycle_indirect() {
        let mut table = ProcessTable::new();
        let a = table.create_process(0x1000, 0, 0).unwrap();
        let b = table.create_process(0x2000, 0, 0).unwrap();
        let c = table.create_process(0x3000, 0, 0).unwrap();
        if let Some(pcb) = table.get_process(b) { pcb.waiting_for = Some(c); }
        if let Some(pcb) = table.get_process(c) { pcb.waiting_for = Some(a); }
        assert!(table.detect_cycle(a, b),
            "three-party cycle a→b→c→a must be detected");
    }

    /// detect_cycle with an unknown target PID returns false (safe to proceed).
    #[test]
    fn test_detect_cycle_unknown_pid() {
        let table = ProcessTable::new();
        let ghost = ProcessId::new(999);
        let a     = ProcessId::new(1);
        assert!(!table.detect_cycle(a, ghost),
            "unknown target pid must be treated as no-path (not a cycle)");
    }

    // ── reset_cpu_budget_counters test ────────────────────────────────────────

    /// reset_cpu_budget_counters zeroes cpu_budget_used for all processes.
    #[test]
    fn test_reset_cpu_budget_counters() {
        let mut table = ProcessTable::new();
        let p1 = table.create_process(0x1000, 0, 0).unwrap();
        let p2 = table.create_process(0x2000, 0, 0).unwrap();
        if let Some(pcb) = table.get_process(p1) { pcb.cpu_budget_used = 42; }
        if let Some(pcb) = table.get_process(p2) { pcb.cpu_budget_used = 99; }
        table.reset_cpu_budget_counters();
        assert_eq!(table.get_process(p1).unwrap().cpu_budget_used, 0);
        assert_eq!(table.get_process(p2).unwrap().cpu_budget_used, 0);
    }
}
