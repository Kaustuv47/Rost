use core::cell::RefCell;
use crate::process::{ProcessId, ProcessState, ProcessTable};
use crate::process::pcb::TaskContext;
use crate::ipc::Message;

pub struct Scheduler {
    process_table:   RefCell<ProcessTable>,
    current_process: RefCell<Option<ProcessId>>,
    queue_index:     RefCell<usize>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            process_table:   RefCell::new(ProcessTable::new()),
            current_process: RefCell::new(None),
            queue_index:     RefCell::new(0),
        }
    }

    pub fn add_process(&self, entry_point: u64, stack_addr: u64) -> Option<ProcessId> {
        self.process_table.borrow_mut().create_process(entry_point, stack_addr)
    }

    pub fn current_process(&self) -> Option<ProcessId> {
        *self.current_process.borrow()
    }

    /// Select the next ready process in round-robin order.
    /// Returns the chosen PID, or `None` if no process is ready.
    pub fn schedule(&self) -> Option<ProcessId> {
        let table = self.process_table.borrow();
        let ready = table.get_ready_processes();
        if ready.is_empty() { return None; }

        let mut idx = self.queue_index.borrow_mut();
        if *idx >= ready.len() { *idx = 0; }
        let next = ready[*idx];
        *idx += 1;
        drop(idx);
        *self.current_process.borrow_mut() = Some(next);
        Some(next)
    }

    /// Called from the timer ISR path (e.g., after TICK_COUNT changes).
    ///
    /// Advances the current process's cpu_time.  When the quantum expires the
    /// process is marked Ready and the next candidate is selected.
    ///
    /// Returns raw pointers `(old_ctx, new_ctx)` when a context switch must
    /// occur, so the architecture layer can call `switch_context(old, new)`.
    /// Returns `None` if the current process still has time left.
    pub fn timer_tick(&self) -> Option<(*mut TaskContext, *const TaskContext)> {
        let mut table = self.process_table.borrow_mut();
        let current_pid = *self.current_process.borrow();

        // Advance cpu_time and check for quantum expiry.
        if let Some(cpid) = current_pid {
            if let Some(pcb) = table.get_process(cpid) {
                pcb.cpu_time += 1;
                if pcb.cpu_time < pcb.time_slice {
                    return None; // still in quantum
                }
                pcb.cpu_time = 0;
                if matches!(pcb.state, ProcessState::Running) {
                    pcb.state = ProcessState::Ready;
                }
            }
        }

        // Select next ready process.
        let ready = table.get_ready_processes();
        if ready.is_empty() { return None; }

        let mut idx = self.queue_index.borrow_mut();
        if *idx >= ready.len() { *idx = 0; }
        let next_pid = ready[*idx];
        *idx += 1;
        drop(idx);

        if Some(next_pid) == current_pid {
            // Same process; no switch needed — just mark it Running again.
            if let Some(pcb) = table.get_process(next_pid) {
                pcb.state = ProcessState::Running;
            }
            *self.current_process.borrow_mut() = Some(next_pid);
            return None;
        }

        *self.current_process.borrow_mut() = Some(next_pid);

        // Gather raw pointers to both TaskContexts.
        // SAFETY: PCBs live in a fixed-size array that is never reallocated; the
        // pointers remain valid for the entire kernel lifetime.
        let old_ptr = current_pid.and_then(|cpid| {
            table.get_process(cpid).map(|pcb| &mut pcb.context as *mut TaskContext)
        });
        let new_ptr = table.get_process(next_pid).map(|pcb| {
            pcb.state = ProcessState::Running;
            &pcb.context as *const TaskContext
        });

        match (old_ptr, new_ptr) {
            (Some(old), Some(new)) => Some((old, new)),
            _ => None,
        }
    }

    /// Deliver `msg` to `to_pid`'s mailbox.
    /// If the target was `Blocked` waiting for a message, it is unblocked.
    pub fn send_message(&self, to_pid: ProcessId, msg: Message) -> bool {
        let mut table = self.process_table.borrow_mut();
        if let Some(pcb) = table.get_process(to_pid) {
            if !pcb.mailbox.send(msg) { return false; }
            if matches!(pcb.state, ProcessState::Blocked) {
                pcb.state = ProcessState::Ready;
            }
            true
        } else {
            false
        }
    }

    /// Try to receive a message for `pid`.
    /// If the mailbox is empty the process is marked `Blocked` and `None` is
    /// returned — the scheduler will not run it again until a message arrives.
    pub fn blocking_receive(&self, pid: ProcessId) -> Option<Message> {
        let mut table = self.process_table.borrow_mut();
        if let Some(pcb) = table.get_process(pid) {
            if let Some(msg) = pcb.mailbox.receive() {
                return Some(msg);
            }
            // No message — block the process.
            pcb.state = ProcessState::Blocked;
        }
        None
    }

    pub fn terminate_process(&self, pid: ProcessId) {
        self.process_table.borrow_mut().terminate_process(pid);
        if *self.current_process.borrow() == Some(pid) {
            *self.current_process.borrow_mut() = None;
        }
    }
}
