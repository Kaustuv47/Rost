use alloc::vec::Vec;

/// Process state enumeration
#[derive(Copy, Clone, Debug)]
pub enum ProcessState {
    Ready,
    Running,
    Blocked,
    Terminated,
}

/// Process ID type
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ProcessId(pub u32);

impl ProcessId {
    pub fn new(id: u32) -> Self {
        ProcessId(id)
    }

    pub fn as_u32(&self) -> u32 {
        self.0
    }
}

/// Process Control Block (PCB)
pub struct ProcessControlBlock {
    pub pid: ProcessId,
    pub state: ProcessState,
    pub priority: u8,

    // CPU context
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rsp: u64,
    pub rbp: u64,
    pub rip: u64,
    pub rflags: u64,

    // Memory
    pub page_table_base: u64,

    // Scheduling
    pub time_slice: u32,
    pub cpu_time: u32,
}

impl ProcessControlBlock {
    /// Create a new process control block
    pub fn new(pid: ProcessId, entry_point: u64, stack_addr: u64) -> Self {
        ProcessControlBlock {
            pid,
            state: ProcessState::Ready,
            priority: 10,

            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            rsi: 0,
            rdi: 0,
            rsp: stack_addr + 4096, // Top of stack
            rbp: stack_addr + 4096,
            rip: entry_point,
            rflags: 0x202, // IF (interrupt flag) set, IOPL = 0

            page_table_base: 0,

            time_slice: 10, // 10ms time slice
            cpu_time: 0,
        }
    }

    /// Save CPU context (called during context switch)
    pub fn save_context(&mut self) {
        // In real implementation, this would be called from assembly
        // to save the current CPU registers
    }

    /// Restore CPU context (called during context switch)
    pub fn restore_context(&self) {
        // In real implementation, this would restore registers
        // and jump to the process entry point
    }
}

/// Process table for managing all processes
pub struct ProcessTable {
    processes: [Option<ProcessControlBlock>; 32],
    next_pid: u32,
}

impl ProcessTable {
    /// Create a new process table
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

    /// Create a new process
    pub fn create_process(&mut self, entry_point: u64, stack_addr: u64) -> Option<ProcessId> {
        let pid = ProcessId::new(self.next_pid);
        self.next_pid += 1;

        for i in 0..32 {
            if self.processes[i].is_none() {
                self.processes[i] = Some(ProcessControlBlock::new(pid, entry_point, stack_addr));
                return Some(pid);
            }
        }
        None
    }

    /// Get a process by ID
    pub fn get_process(&mut self, pid: ProcessId) -> Option<&mut ProcessControlBlock> {
        for i in 0..32 {
            if let Some(ref mut pcb) = self.processes[i] {
                if pcb.pid == pid {
                    return Some(&mut self.processes[i] as &mut Option<ProcessControlBlock>)
                        .and_then(|opt| opt.as_mut());
                }
            }
        }
        None
    }

    /// Terminate a process
    pub fn terminate_process(&mut self, pid: ProcessId) {
        for i in 0..32 {
            if let Some(ref mut pcb) = self.processes[i] {
                if pcb.pid == pid {
                    pcb.state = ProcessState::Terminated;
                    return;
                }
            }
        }
    }

    /// Get all ready processes
    pub fn get_ready_processes(&mut self) -> Vec<ProcessId> {
        let mut ready = Vec::new();
        for i in 0..32 {
            if let Some(ref pcb) = self.processes[i] {
                if matches!(pcb.state, ProcessState::Ready) {
                    ready.push(pcb.pid);
                }
            }
        }
        ready
    }
}
