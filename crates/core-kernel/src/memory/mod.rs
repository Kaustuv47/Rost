mod paging;
mod physical;

pub use paging::{
    map_page, translate_address, identity_map_region,
    PageTable,
    PTE_PRESENT, PTE_WRITABLE, PTE_USER, PTE_NO_EXECUTE,
};
pub use physical::PhysicalAllocator;
