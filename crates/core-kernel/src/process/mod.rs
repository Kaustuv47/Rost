pub mod pcb;
mod table;

pub use pcb::{ProcessControlBlock, ProcessState, TaskContext,
              kernel_stack_guard_addr, MAX_KERNEL_STACKS,
              Capability, CapKind, CAP_TABLE_SIZE,
              CAP_R, CAP_W, CAP_G, CAP_X};
pub use table::{ProcessTable, ProcList};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ProcessId(pub u32);

impl ProcessId {
    pub fn new(id: u32) -> Self { ProcessId(id) }
    pub fn as_u32(&self) -> u32 { self.0 }
}
