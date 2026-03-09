mod paging;
mod physical;

pub use paging::{map_page, translate_address, PageTable, PageTableEntry};
pub use physical::PhysicalAllocator;
