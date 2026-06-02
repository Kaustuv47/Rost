mod paging;
mod physical;
pub mod pool;

pub use paging::{
    map_page, map_page_global, translate_address, identity_map_region,
    split_huge_page_global, unmap_page, remap_page_flags,
    merge_kernel_into_user_pml4, map_crash_log_page,
    PageTable,
    PTE_PRESENT, PTE_WRITABLE, PTE_USER, PTE_NO_EXECUTE, PTE_ADDR_MASK,
};
pub use physical::{
    PhysicalAllocator, init_global_allocator, global_alloc_4k, global_free_4k,
    FrameKind, frame_tag, frame_kind, frame_stats,
};
pub use pool::{
    pool_init, pool_alloc_pt, pool_free_pt, pool_available, pool_capacity,
    PT_POOL_CAP,
};
