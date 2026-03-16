mod paging;
mod physical;

pub use paging::{
    map_page, map_page_global, translate_address, identity_map_region,
    PageTable,
    PTE_PRESENT, PTE_WRITABLE, PTE_USER, PTE_NO_EXECUTE,
};
pub use physical::{PhysicalAllocator, init_global_allocator, global_alloc_4k};
