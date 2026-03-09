use core::cell::RefCell;
use crate::process::{ProcessId, ProcessState, ProcessTable};

pub struct Scheduler {
    process_table: RefCell<ProcessTable>,
    current_process: RefCell<Option<ProcessId>>,
    queue_index: RefCell<usize>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            process_table: RefCell::new(ProcessTable::new()),
            current_process: RefCell::new(None),
            queue_index: RefCell::new(0),
        }
    }

    pub fn add_process(&self, entry_point: u64, stack_addr: u64) -> Option<ProcessId> {
        self.process_table.borrow_mut().create_process(entry_point, stack_addr)
    }

    pub fn schedule(&self) -> Option<ProcessId> {
        let table = self.process_table.borrow();
        let ready = table.get_ready_processes();
        if ready.is_empty() { return None; }

        let mut idx = self.queue_index.borrow_mut();
        if *idx >= ready.len() { *idx = 0; }
        let next = ready[*idx];
        *idx += 1;
        *self.current_process.borrow_mut() = Some(next);
        Some(next)
    }

    pub fn current_process(&self) -> Option<ProcessId> {
        *self.current_process.borrow()
    }

    pub fn context_switch(&self) {
        if let Some(pid) = self.schedule() {
            let mut table = self.process_table.borrow_mut();
            if let Some(pcb) = table.get_process(pid) {
                pcb.state = ProcessState::Running;
                pcb.restore_context();
            }
        }
    }
}
