use alloc::vec::Vec;
use super::ProcessId;
use super::pcb::{ProcessControlBlock, ProcessState};

const MAX_PROCESSES: usize = 32;

pub struct ProcessTable {
    processes: [Option<ProcessControlBlock>; MAX_PROCESSES],
    next_pid: u32,
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

    pub fn create_process(&mut self, entry_point: u64, stack_addr: u64) -> Option<ProcessId> {
        let pid = ProcessId::new(self.next_pid);
        self.next_pid += 1;
        for slot in self.processes.iter_mut() {
            if slot.is_none() {
                *slot = Some(ProcessControlBlock::new(pid, entry_point, stack_addr));
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

    pub fn terminate_process(&mut self, pid: ProcessId) {
        if let Some(pcb) = self.get_process(pid) {
            pcb.state = ProcessState::Terminated;
        }
    }

    pub fn get_ready_processes(&self) -> Vec<ProcessId> {
        self.processes.iter()
            .filter_map(|s| s.as_ref())
            .filter(|pcb| matches!(pcb.state, ProcessState::Ready))
            .map(|pcb| pcb.pid)
            .collect()
    }
}
