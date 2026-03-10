pub mod pcb;
mod table;

pub use pcb::{ProcessControlBlock, ProcessState, TaskContext};
pub use table::ProcessTable;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ProcessId(pub u32);

impl ProcessId {
    pub fn new(id: u32) -> Self { ProcessId(id) }
    pub fn as_u32(&self) -> u32 { self.0 }
}
