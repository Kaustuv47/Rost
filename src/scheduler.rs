use crate::process::{ProcessId, ProcessTable, ProcessState};
use core::cell::RefCell;

/// Round-robin scheduler
pub struct Scheduler {
    process_table: RefCell<ProcessTable>,
    current_process: RefCell<Option<ProcessId>>,
    queue_index: RefCell<usize>,
}

impl Scheduler {
    /// Create a new scheduler
    pub fn new() -> Self {
        Scheduler {
            process_table: RefCell::new(ProcessTable::new()),
            current_process: RefCell::new(None),
            queue_index: RefCell::new(0),
        }
    }

    /// Add a process to the scheduler
    pub fn add_process(&self, entry_point: u64, stack_addr: u64) -> Option<ProcessId> {
        self.process_table.borrow_mut().create_process(entry_point, stack_addr)
    }

    /// Schedule the next process (round-robin)
    pub fn schedule(&self) -> Option<ProcessId> {
        let mut table = self.process_table.borrow_mut();
        let ready_pids = table.get_ready_processes();

        if ready_pids.is_empty() {
            return None;
        }

        let mut queue_idx = self.queue_index.borrow_mut();
        if *queue_idx >= ready_pids.len() {
            *queue_idx = 0;
        }

        let next_pid = ready_pids[*queue_idx];
        *queue_idx += 1;

        *self.current_process.borrow_mut() = Some(next_pid);
        Some(next_pid)
    }

    /// Get current process
    pub fn current_process(&self) -> Option<ProcessId> {
        *self.current_process.borrow()
    }

    /// Context switch to next process
    pub fn context_switch(&self) {
        if let Some(next_pid) = self.schedule() {
            let mut table = self.process_table.borrow_mut();
            if let Some(pcb) = table.get_process(next_pid) {
                pcb.state = ProcessState::Running;
                pcb.restore_context();
            }
        }
    }
}
