use alloc::vec::Vec;
use super::ProcessId;
use super::pcb::{ProcessControlBlock, ProcessState};

const MAX_PROCESSES: usize = 32;

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

    pub fn get_process(&mut self, pid: ProcessId) -> Option<&mut ProcessControlBlock> {
        self.processes.iter_mut()
            .filter_map(|s| s.as_mut())
            .find(|pcb| pcb.pid == pid)
    }

    /// Mark `pid` as Terminated and **reclaim its table slot**.
    ///
    /// The kernel stack slot is returned to the `NEXT_STACK` counter by
    /// resetting it (simple bump allocator — slots are reused in order).
    /// For a bump allocator, actual page-frame reclaim requires the physical
    /// allocator; that is deferred until a free-list allocator is in place.
    pub fn terminate_process(&mut self, pid: ProcessId) {
        for slot in self.processes.iter_mut() {
            if let Some(pcb) = slot.as_ref() {
                if pcb.pid == pid {
                    // Clear the slot — drops the PCB and frees its mailbox.
                    *slot = None;
                    return;
                }
            }
        }
    }

    pub fn get_ready_processes(&self) -> Vec<ProcessId> {
        self.processes.iter()
            .filter_map(|s| s.as_ref())
            .filter(|pcb| matches!(pcb.state, ProcessState::Ready | ProcessState::Running))
            .map(|pcb| pcb.pid)
            .collect()
    }

    /// Return `(ProcessId, priority)` for every Ready process.
    /// Used by the priority scheduler to pick the highest-priority next task.
    pub fn get_ready_with_priority(&self) -> Vec<(ProcessId, u8)> {
        self.processes.iter()
            .filter_map(|s| s.as_ref())
            .filter(|pcb| matches!(pcb.state, ProcessState::Ready | ProcessState::Running))
            .map(|pcb| (pcb.pid, pcb.priority))
            .collect()
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
                }
            }
        }
    }

    /// Reset per-frame IPC rate counters (call at the start of each 100-tick window).
    pub fn reset_ipc_rate_counters(&mut self) {
        for slot in self.processes.iter_mut() {
            if let Some(pcb) = slot.as_mut() {
                pcb.ipc_rate_used = 0;
            }
        }
    }
}
