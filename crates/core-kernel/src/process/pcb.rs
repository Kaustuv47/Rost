use super::ProcessId;

#[derive(Copy, Clone, Debug)]
pub enum ProcessState {
    Ready,
    Running,
    Blocked,
    Terminated,
}

pub struct ProcessControlBlock {
    pub pid: ProcessId,
    pub state: ProcessState,
    pub priority: u8,

    // CPU context (saved/restored on context switch)
    pub rax: u64, pub rbx: u64, pub rcx: u64, pub rdx: u64,
    pub rsi: u64, pub rdi: u64, pub rsp: u64, pub rbp: u64,
    pub rip: u64, pub rflags: u64,

    pub page_table_base: u64,
    pub time_slice: u32,
    pub cpu_time: u32,
}

impl ProcessControlBlock {
    pub fn new(pid: ProcessId, entry_point: u64, stack_addr: u64) -> Self {
        ProcessControlBlock {
            pid,
            state: ProcessState::Ready,
            priority: 10,
            rax: 0, rbx: 0, rcx: 0, rdx: 0,
            rsi: 0, rdi: 0,
            rsp: stack_addr + 4096,
            rbp: stack_addr + 4096,
            rip: entry_point,
            rflags: 0x202, // IF set, IOPL=0
            page_table_base: 0,
            time_slice: 10,
            cpu_time: 0,
        }
    }

    /// Save CPU context (stub — called from assembly in a real implementation)
    pub fn save_context(&mut self) {}

    /// Restore CPU context (stub)
    pub fn restore_context(&self) {}
}
