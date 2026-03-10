mod paging;
mod physical;

pub use paging::{map_page, translate_address, PageTable, PTE_PRESENT, PTE_WRITABLE, PTE_USER};
pub use physical::PhysicalAllocator;
