/// x86_64 4-level page table implementation (PML4 → PDPT → PD → PT).
///
/// All tables are 4 KB, 4 KB-aligned, and contain 512 × 8-byte entries.
/// Virtual address decomposition:
///   bits 47:39  PML4 index
///   bits 38:30  PDPT index
///   bits 29:21  PD   index
///   bits 20:12  PT   index
///   bits 11: 0  page offset
///
/// Since the kernel uses identity mapping (phys == virt) before CR3 is loaded,
/// physical addresses of intermediate tables can be dereferenced directly.
use super::physical::PhysicalAllocator;

// ── Entry flags ───────────────────────────────────────────────────────────────

pub const PTE_PRESENT:        u64 = 1 << 0;
pub const PTE_WRITABLE:       u64 = 1 << 1;
pub const PTE_USER:           u64 = 1 << 2;
/// Page-Size bit in a PD entry — makes it a 2 MB huge-page leaf.
pub const PTE_HUGE_PAGE:      u64 = 1 << 7;
/// No-Execute bit (requires EFER.NXE = 1 to be active).
pub const PTE_NO_EXECUTE:     u64 = 1u64 << 63;

/// Physical-address mask for 4 KB page entries (PT / PDPT / PML4 entries).
pub const PTE_ADDR_MASK:      u64 = 0x000F_FFFF_FFFF_F000;
/// Physical-address mask for 2 MB huge-page PD entries (bits[51:21]).
pub const PTE_HUGE_ADDR_MASK: u64 = 0x000F_FFFF_FFE0_0000;

// ── Page table structure ──────────────────────────────────────────────────────

/// A single 4 KB page table at any level (PML4, PDPT, PD, or PT).
#[repr(C, align(4096))]
pub struct PageTable {
    pub entries: [u64; 512],
}

impl PageTable {
    pub const fn new() -> Self {
        PageTable { entries: [0u64; 512] }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Dereference a physical address as a mutable PageTable reference.
///
/// # Safety
/// Requires identity mapping (phys == virt) to hold at the call site.
unsafe fn phys_to_table(phys: u64) -> &'static mut PageTable {
    &mut *(phys as *mut PageTable)
}

/// Allocate and zero-initialise a new 4 KB page table.
fn alloc_table(alloc: &mut PhysicalAllocator) -> Option<u64> {
    let phys = alloc.allocate(4096)? as u64;
    // Zero-fill the new table (identity mapping: can dereference directly).
    unsafe { core::ptr::write_bytes(phys as *mut u8, 0, 4096); }
    Some(phys)
}

/// Return the physical address of the next-level table, creating it if absent.
fn ensure_table(entry: &mut u64, alloc: &mut PhysicalAllocator) -> Option<u64> {
    if *entry & PTE_PRESENT == 0 {
        let phys = alloc_table(alloc)?;
        *entry = (phys & PTE_ADDR_MASK) | PTE_PRESENT | PTE_WRITABLE;
    }
    Some(*entry & PTE_ADDR_MASK)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Map `virt` → `phys` in the given PML4 table using an explicit flags word.
///
/// `flags` should include at least `PTE_PRESENT`; add `PTE_WRITABLE` / `PTE_USER`
/// / `PTE_NO_EXECUTE` as needed.  Intermediate tables are allocated from `alloc`.
/// Returns `true` on success, `false` if allocation fails.
pub fn map_page(
    pml4:  &mut PageTable,
    virt:  u64,
    phys:  u64,
    flags: u64,
    alloc: &mut PhysicalAllocator,
) -> bool {
    let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
    let pd_idx   = ((virt >> 21) & 0x1FF) as usize;
    let pt_idx   = ((virt >> 12) & 0x1FF) as usize;

    let pdpt_phys = match ensure_table(&mut pml4.entries[pml4_idx], alloc) {
        Some(p) => p, None => return false,
    };
    let pd_phys = unsafe {
        match ensure_table(&mut phys_to_table(pdpt_phys).entries[pdpt_idx], alloc) {
            Some(p) => p, None => return false,
        }
    };
    let pt_phys = unsafe {
        match ensure_table(&mut phys_to_table(pd_phys).entries[pd_idx], alloc) {
            Some(p) => p, None => return false,
        }
    };

    unsafe { phys_to_table(pt_phys).entries[pt_idx] = (phys & PTE_ADDR_MASK) | (flags & !PTE_HUGE_PAGE); }
    true
}

/// Map `virt` → `phys` using the **global** bump allocator for intermediate tables.
///
/// This is the syscall-facing variant used by `SYS_MAP`; it does not require
/// passing a local `PhysicalAllocator`.  Returns `true` on success.
pub fn map_page_global(
    pml4:  &mut PageTable,
    virt:  u64,
    phys:  u64,
    flags: u64,
) -> bool {
    use super::physical::global_alloc_4k;
    use super::pool::pool_alloc_pt;

    // Try the O(1) page-table pool first; fall back to the O(N/64) bitmap
    // allocator so mapping never silently fails during pool exhaustion.
    let alloc_pt_frame = || pool_alloc_pt().or_else(global_alloc_4k);

    // x86-64 rule: ALL levels of the page walk (PML4 → PDPT → PD → PT) must
    // have U/S=1 for a user-mode access to succeed.  Propagate PTE_USER from
    // the leaf flags into every intermediate table entry we create or touch.
    let user_flag = flags & PTE_USER;

    let alloc_fn = |entry: &mut u64| -> Option<u64> {
        if *entry & PTE_PRESENT == 0 {
            let p = alloc_pt_frame()?;
            unsafe { core::ptr::write_bytes(p as *mut u8, 0, 4096); }
            *entry = (p & PTE_ADDR_MASK) | PTE_PRESENT | PTE_WRITABLE | user_flag;
        } else if *entry & PTE_HUGE_PAGE != 0 {
            // PD entry is a 2 MB huge page — split it into 512 × 4 KB entries
            // so subsequent PT-level mapping can proceed safely.
            let huge_base = *entry & PTE_HUGE_ADDR_MASK;
            let flags     = *entry & !PTE_HUGE_ADDR_MASK & !PTE_HUGE_PAGE;
            let p = alloc_pt_frame()?;
            unsafe {
                core::ptr::write_bytes(p as *mut u8, 0, 4096);
                let pt = p as *mut u64;
                for i in 0..512usize {
                    *pt.add(i) = (huge_base + i as u64 * 4096) | flags;
                }
            }
            *entry = (p & PTE_ADDR_MASK) | PTE_PRESENT | PTE_WRITABLE | user_flag;
        } else if user_flag != 0 && (*entry & PTE_USER == 0) {
            // Upgrade an existing supervisor-only entry to also allow user
            // access (can happen when a kernel PT is later reused for user).
            *entry |= PTE_USER;
        }
        Some(*entry & PTE_ADDR_MASK)
    };

    let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
    let pd_idx   = ((virt >> 21) & 0x1FF) as usize;
    let pt_idx   = ((virt >> 12) & 0x1FF) as usize;

    let pdpt_phys = match alloc_fn(&mut pml4.entries[pml4_idx]) {
        Some(p) => p, None => return false,
    };
    let pd_phys = unsafe {
        match alloc_fn(&mut phys_to_table(pdpt_phys).entries[pdpt_idx]) {
            Some(p) => p, None => return false,
        }
    };
    let pt_phys = unsafe {
        match alloc_fn(&mut phys_to_table(pd_phys).entries[pd_idx]) {
            Some(p) => p, None => return false,
        }
    };
    unsafe { phys_to_table(pt_phys).entries[pt_idx] = (phys & PTE_ADDR_MASK) | (flags & !PTE_HUGE_PAGE); }
    true
}

/// Identity-map a physical region using 2 MB huge pages (PD-level).
///
/// Both `phys_base` and `size` are rounded to 2 MB boundaries before mapping.
/// Each PD entry uses the PS (page-size) bit, so no PT allocations are needed:
/// a 4 GB RAM system requires at most ~20 KB of page-table space.
///
/// `flags` must include `PTE_PRESENT`; add `PTE_WRITABLE`, `PTE_USER`, or
/// `PTE_NO_EXECUTE` as needed.  `PTE_HUGE_PAGE` is set automatically.
pub fn identity_map_region(
    pml4:      &mut PageTable,
    phys_base: u64,
    size:      u64,
    flags:     u64,
    alloc:     &mut PhysicalAllocator,
) {
    const HUGE: u64 = 2 * 1024 * 1024; // 2 MB
    let start = phys_base & !(HUGE - 1);                                // round down
    let end   = (phys_base.saturating_add(size) + HUGE - 1) & !(HUGE - 1); // round up

    let mut addr = start;
    while addr < end {
        let pml4_idx = ((addr >> 39) & 0x1FF) as usize;
        let pdpt_idx = ((addr >> 30) & 0x1FF) as usize;
        let pd_idx   = ((addr >> 21) & 0x1FF) as usize;

        let pdpt_phys = match ensure_table(&mut pml4.entries[pml4_idx], alloc) {
            Some(p) => p, None => return,
        };
        let pd_phys = unsafe {
            match ensure_table(&mut phys_to_table(pdpt_phys).entries[pdpt_idx], alloc) {
                Some(p) => p, None => return,
            }
        };

        // Set the 2 MB huge-page PD entry only if not already mapped.
        let entry = &mut unsafe { phys_to_table(pd_phys) }.entries[pd_idx];
        if *entry & PTE_PRESENT == 0 {
            *entry = (addr & PTE_HUGE_ADDR_MASK) | flags | PTE_HUGE_PAGE;
        }

        addr = addr.wrapping_add(HUGE);
    }
}

/// Split the 2 MB huge-page PD entry covering `virt` into 512 individually
/// mapped 4 KB PT entries, using the global bump allocator for the new PT.
///
/// After the split every sub-page has the same flags as the original huge page.
/// A subsequent `unmap_page()` call can then remove any individual 4 KB slot.
///
/// Returns `false` if:
/// - the PD entry covering `virt` is not a present huge page (no-op, returns `true`
///   if it already points to a PT);
/// - the global allocator is exhausted.
pub fn split_huge_page_global(pml4: &mut PageTable, virt: u64) -> bool {
    use super::physical::global_alloc_4k;
    use super::pool::pool_alloc_pt;

    let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
    let pd_idx   = ((virt >> 21) & 0x1FF) as usize;

    unsafe {
        if pml4.entries[pml4_idx] & PTE_PRESENT == 0 { return false; }
        let pdpt = phys_to_table(pml4.entries[pml4_idx] & PTE_ADDR_MASK);

        if pdpt.entries[pdpt_idx] & PTE_PRESENT == 0 { return false; }
        let pd = phys_to_table(pdpt.entries[pdpt_idx] & PTE_ADDR_MASK);

        let pd_entry = pd.entries[pd_idx];
        if pd_entry & PTE_PRESENT == 0 { return false; }
        if pd_entry & PTE_HUGE_PAGE == 0 {
            // Already points to a PT — nothing to split.
            return true;
        }

        // Allocate and zero a new PT — pool first, bitmap fallback.
        let pt_phys = match pool_alloc_pt().or_else(global_alloc_4k) {
            Some(p) => p,
            None => return false,
        };
        core::ptr::write_bytes(pt_phys as *mut u8, 0, 4096);

        // Flags from the huge-page entry, minus the PTE_HUGE_PAGE (PS) bit.
        let huge_base = pd_entry & PTE_HUGE_ADDR_MASK;
        let flags     = pd_entry & !PTE_HUGE_ADDR_MASK & !PTE_HUGE_PAGE;

        let pt = phys_to_table(pt_phys);
        for i in 0..512usize {
            pt.entries[i] = (huge_base + (i as u64 * 4096)) | flags;
        }

        // Replace the PD entry: huge page → PT pointer.
        pd.entries[pd_idx] = (pt_phys & PTE_ADDR_MASK) | PTE_PRESENT | PTE_WRITABLE;
        true
    }
}

/// Clear the PT entry for a single 4 KB page (mark not-present / guard page).
///
/// Requires the PD entry for `virt` to already point to a PT (not a huge page).
/// Call `split_huge_page_global()` first if the region was huge-mapped.
///
/// The caller must execute `invlpg(virt)` after this call (or rely on a
/// subsequent CR3 reload) to flush the stale TLB entry.
///
/// Returns `false` if the page is not mapped at 4 KB granularity.
pub fn unmap_page(pml4: &mut PageTable, virt: u64) -> bool {
    let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
    let pd_idx   = ((virt >> 21) & 0x1FF) as usize;
    let pt_idx   = ((virt >> 12) & 0x1FF) as usize;

    unsafe {
        if pml4.entries[pml4_idx] & PTE_PRESENT == 0 { return false; }
        let pdpt = phys_to_table(pml4.entries[pml4_idx] & PTE_ADDR_MASK);

        if pdpt.entries[pdpt_idx] & PTE_PRESENT == 0 { return false; }
        let pd = phys_to_table(pdpt.entries[pdpt_idx] & PTE_ADDR_MASK);

        let pd_entry = pd.entries[pd_idx];
        if pd_entry & PTE_PRESENT == 0 { return false; }
        if pd_entry & PTE_HUGE_PAGE != 0 {
            // Still a huge page — must call split_huge_page_global() first.
            return false;
        }

        let pt = phys_to_table(pd_entry & PTE_ADDR_MASK);
        pt.entries[pt_idx] = 0; // not present
        true
    }
}

/// Update the PTE flags for an already-mapped 4 KB page at `virt`.
///
/// The physical address encoded in the existing PTE is preserved; only the
/// flag bits are replaced with `new_flags`.  `PTE_HUGE_PAGE` in `new_flags`
/// is stripped (PT-level entries must not have the PS bit set).
///
/// **TLB responsibility**: this function does NOT execute `invlpg`.  The
/// caller must invalidate the TLB entry after this call:
/// - If the modification happens before the CR3 containing this page table is
///   first loaded, the subsequent CR3 write flushes the entire TLB automatically.
/// - If the page table is already active, call `arch_x86_64::cpu::invlpg(virt)`.
///
/// Returns `false` if:
/// - any intermediate page-table level is not present;
/// - the PD entry covering `virt` is still a 2 MB huge page (`split_huge_page_global`
///   must be called before this function);
/// - the PT entry for `virt` is not present.
pub fn remap_page_flags(pml4: &mut PageTable, virt: u64, new_flags: u64) -> bool {
    let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
    let pd_idx   = ((virt >> 21) & 0x1FF) as usize;
    let pt_idx   = ((virt >> 12) & 0x1FF) as usize;

    unsafe {
        if pml4.entries[pml4_idx] & PTE_PRESENT == 0 { return false; }
        let pdpt = phys_to_table(pml4.entries[pml4_idx] & PTE_ADDR_MASK);

        if pdpt.entries[pdpt_idx] & PTE_PRESENT == 0 { return false; }
        let pd = phys_to_table(pdpt.entries[pdpt_idx] & PTE_ADDR_MASK);

        let pd_entry = pd.entries[pd_idx];
        if pd_entry & PTE_PRESENT == 0 { return false; }
        if pd_entry & PTE_HUGE_PAGE != 0 {
            // Still a 2 MB huge page — call split_huge_page_global() first.
            return false;
        }

        let pt = phys_to_table(pd_entry & PTE_ADDR_MASK);
        let old = pt.entries[pt_idx];
        if old & PTE_PRESENT == 0 { return false; }

        // Keep physical address; replace all flag bits.
        pt.entries[pt_idx] = (old & PTE_ADDR_MASK) | (new_flags & !PTE_HUGE_PAGE);
        true
    }
}

/// Walk `pml4` to translate `virt` into its physical address.
///
/// Returns `None` if any level is not present.
pub fn translate_address(pml4: &PageTable, virt: u64) -> Option<u64> {
    let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
    let pd_idx   = ((virt >> 21) & 0x1FF) as usize;
    let pt_idx   = ((virt >> 12) & 0x1FF) as usize;

    if pml4.entries[pml4_idx] & PTE_PRESENT == 0 { return None; }
    let pdpt = unsafe { phys_to_table(pml4.entries[pml4_idx] & PTE_ADDR_MASK) };

    if pdpt.entries[pdpt_idx] & PTE_PRESENT == 0 { return None; }
    let pd = unsafe { phys_to_table(pdpt.entries[pdpt_idx] & PTE_ADDR_MASK) };

    if pd.entries[pd_idx] & PTE_PRESENT == 0 { return None; }
    // Check for 2 MB huge page at PD level (PS bit set).
    // Without this check the code would misinterpret the huge-page PD entry as
    // a pointer to a PT, dereferencing an invalid address — a functional bug.
    if pd.entries[pd_idx] & PTE_HUGE_PAGE != 0 {
        let phys_base = pd.entries[pd_idx] & PTE_HUGE_ADDR_MASK;
        let offset    = virt & 0x1F_FFFF; // 21-bit intra-page offset
        return Some(phys_base | offset);
    }
    let pt = unsafe { phys_to_table(pd.entries[pd_idx] & PTE_ADDR_MASK) };

    if pt.entries[pt_idx] & PTE_PRESENT == 0 { return None; }
    Some((pt.entries[pt_idx] & PTE_ADDR_MASK) | (virt & 0xFFF))
}

/// Copy kernel page-table entries into a user-process PML4.
///
/// Needed so ring-3 processes have kernel mappings in their PML4 — SYSCALL
/// handlers run with the user CR3 still active, so kernel code/data must be
/// accessible from the user PML4.
///
/// Called from both `SYS_CREATE_VAS` (fresh empty PML4) and `spawn_elf`
/// (after ELF segments are mapped).  When called on a fresh PML4,
/// `user_pml4[0]` is zero; the function allocates a new PDPT and PD so
/// that kernel PD entries (PD[1..512]) can always be merged, regardless
/// of whether user-space mappings have been installed yet.
///
/// # Safety
/// `user_pml4_phys` must be a valid, 4 KB-aligned frame.
/// Identity mapping must hold (phys == virt) at the call site.
pub unsafe fn merge_kernel_into_user_pml4(user_pml4_phys: u64) {
    let kernel_pml4_phys = crate::scheduler::KERNEL_PML4_PHYS
        .load(core::sync::atomic::Ordering::Relaxed);
    if kernel_pml4_phys == 0 { return; }

    let kern_pml4 = &*(kernel_pml4_phys as *const PageTable);
    let user_pml4 = &mut *(user_pml4_phys as *mut PageTable);

    // ── Step 1: PML4[1..512] (canonical kernel-half entries) ─────────────────
    for i in 1..512usize {
        if kern_pml4.entries[i] & PTE_PRESENT != 0 && user_pml4.entries[i] == 0 {
            user_pml4.entries[i] = kern_pml4.entries[i];
        }
    }

    // ── Step 2: PDPT[1..512] inside PML4[0] ──────────────────────────────────
    if kern_pml4.entries[0] & PTE_PRESENT == 0 { return; }
    let kern_pdpt = &*((kern_pml4.entries[0] & PTE_ADDR_MASK) as *const PageTable);

    // Allocate a fresh PDPT for user_pml4[0] if one doesn't exist yet.
    // This happens when merge is called on a blank PML4 (SYS_CREATE_VAS path)
    // before any ELF segments have been mapped into the address space.
    if user_pml4.entries[0] & PTE_PRESENT == 0 {
        let p = match super::physical::global_alloc_4k() {
            Some(addr) => addr,
            None       => return, // OOM — leave without kernel PDPT entries
        };
        core::ptr::write_bytes(p as *mut u8, 0, 4096);
        user_pml4.entries[0] = p | PTE_PRESENT | PTE_WRITABLE | PTE_USER;
    }
    let user_pdpt = &mut *((user_pml4.entries[0] & PTE_ADDR_MASK) as *mut PageTable);

    for i in 1..512usize {
        if kern_pdpt.entries[i] & PTE_PRESENT != 0 && user_pdpt.entries[i] == 0 {
            user_pdpt.entries[i] = kern_pdpt.entries[i];
        }
    }

    // ── Step 3: PD[1..512] inside PDPT[0] ────────────────────────────────────
    if kern_pdpt.entries[0] & PTE_PRESENT == 0 { return; }
    let kern_pd = &*((kern_pdpt.entries[0] & PTE_ADDR_MASK) as *const PageTable);

    // Allocate a fresh PD for user_pdpt[0] if one doesn't exist yet.
    if user_pdpt.entries[0] & PTE_PRESENT == 0 {
        let p = match super::physical::global_alloc_4k() {
            Some(addr) => addr,
            None       => return, // OOM
        };
        core::ptr::write_bytes(p as *mut u8, 0, 4096);
        user_pdpt.entries[0] = p | PTE_PRESENT | PTE_WRITABLE | PTE_USER;
    }
    let user_pd = &mut *((user_pdpt.entries[0] & PTE_ADDR_MASK) as *mut PageTable);

    // Skip PD[0] (0–2 MB); the crash-log page at physical 0x4000 is mapped
    // via a dedicated map_page_global call after this merge returns.
    for i in 1..512usize {
        if kern_pd.entries[i] & PTE_PRESENT != 0 && user_pd.entries[i] == 0 {
            user_pd.entries[i] = kern_pd.entries[i];
        }
    }
}

/// Map the crash-log physical page into a user PML4 so exception handlers can
/// write to it when CR3 = user PML4.  The page is supervisor-only (no PTE_USER).
///
/// Must be called after `merge_kernel_into_user_pml4` has ensured that PDPT[0]
/// and PD[0] intermediate tables exist in the user PML4.
pub fn map_crash_log_page(user_pml4: &mut PageTable) {
    // Physical and virtual address of the crash log (identity mapped).
    let addr = crate::crash_log::CRASH_LOG_PHYS;
    map_page_global(user_pml4, addr, addr, PTE_PRESENT | PTE_WRITABLE);
}

// ── Unit tests ────────────────────────────────────────────────────────────────
//
// Tests run on the host (x86_64-apple-darwin / x86_64-unknown-linux-gnu).
// On the host, the "physical" addresses returned by PhysicalAllocator are
// ordinary virtual addresses from the heap — they can be directly dereferenced
// because phys_to_table() does exactly that.  As long as the backing buffer is
// 4096-byte aligned, the PageTable casts are valid.
//
// Run with: cargo test -p core-kernel --target x86_64-apple-darwin

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::physical::PhysicalAllocator;
    use std::alloc::{alloc_zeroed, dealloc, Layout};

    // ── Test helpers ─────────────────────────────────────────────────────────

    /// RAII 4096-byte-aligned memory slab for intermediate page-table storage.
    ///
    /// On the host, heap addresses are valid pointers, so the "physical"
    /// addresses returned by the allocator can be directly dereferenced via
    /// `phys_to_table`.
    struct AlignedSlab {
        ptr:    *mut u8,
        layout: Layout,
    }

    impl AlignedSlab {
        fn new(pages: usize) -> Self {
            let layout = Layout::from_size_align(pages * 4096, 4096).unwrap();
            let ptr = unsafe { alloc_zeroed(layout) };
            assert!(!ptr.is_null(), "AlignedSlab allocation failed");
            AlignedSlab { ptr, layout }
        }

        fn allocator(&mut self, pages: usize) -> PhysicalAllocator {
            assert!(pages * 4096 <= self.layout.size());
            PhysicalAllocator::new(self.ptr as usize, pages * 4096)
        }
    }

    impl Drop for AlignedSlab {
        fn drop(&mut self) {
            unsafe { dealloc(self.ptr, self.layout); }
        }
    }

    // ── map_page + translate_address ──────────────────────────────────────────

    /// Mapping a page and translating it returns the correct physical address.
    #[test]
    fn test_map_and_translate_basic() {
        let mut slab  = AlignedSlab::new(16);  // 16 pages for intermediate tables
        let mut alloc = slab.allocator(16);
        let mut pml4  = Box::new(PageTable::new());

        let virt: u64 = 0x0000_1000_0000_0000; // arbitrary canonical user VA
        let phys: u64 = 0x0000_0000_0020_0000; // physical target (2 MB, well-known)

        let mapped = map_page(&mut pml4, virt, phys, PTE_PRESENT | PTE_WRITABLE, &mut alloc);
        assert!(mapped, "map_page should succeed");

        let result = translate_address(&pml4, virt);
        assert_eq!(result, Some(phys), "translate_address should return exact phys");
    }

    /// Page offset bits are preserved through map → translate.
    #[test]
    fn test_map_and_translate_with_offset() {
        let mut slab  = AlignedSlab::new(16);
        let mut alloc = slab.allocator(16);
        let mut pml4  = Box::new(PageTable::new());

        let virt: u64 = 0x0000_1234_5678_0ABC; // page-aligned base + 0xABC offset
        let page_base = virt & !0xFFF;
        let offset    = virt & 0xFFF;
        let phys: u64 = 0x0000_0000_0030_0000;

        assert!(map_page(&mut pml4, page_base, phys, PTE_PRESENT, &mut alloc));
        let result = translate_address(&pml4, page_base | offset);
        assert_eq!(result, Some(phys | offset),
                   "page offset must be OR-ed into the physical address");
    }

    /// Translating an address whose PML4 entry is absent returns None.
    #[test]
    fn test_translate_absent_pml4() {
        let pml4 = Box::new(PageTable::new()); // all entries zero
        assert_eq!(translate_address(&pml4, 0x0000_1000_0000_0000), None);
    }

    /// Translating with a present PML4 but absent PDPT returns None.
    #[test]
    fn test_translate_absent_pdpt() {
        let mut slab  = AlignedSlab::new(8);
        let mut alloc = slab.allocator(8);
        let mut pml4  = Box::new(PageTable::new());

        // Map address A in one PDPT slot.
        let va_a: u64 = 0x0000_0000_0010_0000; // uses PML4[0] PDPT[0] PD[0] PT[16]
        assert!(map_page(&mut pml4, va_a, 0x1000, PTE_PRESENT, &mut alloc));

        // Address B is in a different PDPT slot (same PML4 entry, different bit[38:30]).
        // Shift by 1 GB so pdpt_idx differs.
        let va_b: u64 = va_a + (1u64 << 30); // pdpt_idx = 1 → absent
        assert_eq!(translate_address(&pml4, va_b), None,
                   "PDPT entry for va_b should be absent");
    }

    /// Translating with a present PDPT but absent PD returns None.
    #[test]
    fn test_translate_absent_pd() {
        let mut slab  = AlignedSlab::new(8);
        let mut alloc = slab.allocator(8);
        let mut pml4  = Box::new(PageTable::new());

        let va_a: u64 = 0x0000_0000_0010_0000; // PML4[0], PDPT[0], PD[0], PT[16]
        assert!(map_page(&mut pml4, va_a, 0x1000, PTE_PRESENT, &mut alloc));

        // Same PML4 + PDPT, different PD slot (shift by 2 MB).
        let va_b: u64 = va_a + (1u64 << 21); // pd_idx = 1 → absent
        assert_eq!(translate_address(&pml4, va_b), None,
                   "PD entry for va_b should be absent");
    }

    /// Translating with a present PT but zero PT entry returns None.
    #[test]
    fn test_translate_absent_pt_entry() {
        let mut slab  = AlignedSlab::new(8);
        let mut alloc = slab.allocator(8);
        let mut pml4  = Box::new(PageTable::new());

        let va_a: u64 = 0x0000_0000_0010_0000; // PT[16]
        assert!(map_page(&mut pml4, va_a, 0x1000, PTE_PRESENT, &mut alloc));

        // Same PT, adjacent slot (shift by 4 KB → pt_idx = 17) — never mapped.
        let va_b: u64 = va_a + 0x1000;
        assert_eq!(translate_address(&pml4, va_b), None,
                   "adjacent PT entry was never mapped");
    }

    /// PTE_WRITABLE and PTE_USER flags survive round-trip through map_page.
    #[test]
    fn test_pte_flags_preserved() {
        let mut slab  = AlignedSlab::new(16);
        let mut alloc = slab.allocator(16);
        let mut pml4  = Box::new(PageTable::new());

        let virt: u64 = 0x0000_0000_0040_0000;
        let phys: u64 = 0x0000_0000_0050_0000;
        let flags = PTE_PRESENT | PTE_WRITABLE | PTE_USER;

        assert!(map_page(&mut pml4, virt, phys, flags, &mut alloc));

        // Translate to confirm the page is mapped (flags are in the PT entry
        // but translate_address strips them via PTE_ADDR_MASK).
        let result = translate_address(&pml4, virt);
        assert_eq!(result, Some(phys));

        // Inspect the PT entry directly to verify flags.
        // Walk manually: PML4[0]→PDPT[0]→PD[2]→PT[0] for virt=0x40_0000.
        let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
        let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
        let pd_idx   = ((virt >> 21) & 0x1FF) as usize;
        let pt_idx   = ((virt >> 12) & 0x1FF) as usize;

        let pdpt_phys = pml4.entries[pml4_idx] & PTE_ADDR_MASK;
        let pd_phys   = unsafe { phys_to_table(pdpt_phys) }.entries[pdpt_idx] & PTE_ADDR_MASK;
        let pt_phys   = unsafe { phys_to_table(pd_phys) }.entries[pd_idx] & PTE_ADDR_MASK;
        let pt_entry  = unsafe { phys_to_table(pt_phys) }.entries[pt_idx];

        assert!(pt_entry & PTE_PRESENT  != 0, "PRESENT must be set");
        assert!(pt_entry & PTE_WRITABLE != 0, "WRITABLE must be set");
        assert!(pt_entry & PTE_USER     != 0, "USER must be set");
    }

    /// map_page returns false (not true) when the allocator is exhausted.
    #[test]
    fn test_map_page_alloc_failure() {
        // Give only 1 page — PML4→PDPT alone needs 1 table; with 1 page
        // PDPT is allocated but PD allocation fails.
        let mut slab  = AlignedSlab::new(1);
        let mut alloc = slab.allocator(1);
        let mut pml4  = Box::new(PageTable::new());

        let result = map_page(&mut pml4, 0x1000, 0x2000, PTE_PRESENT, &mut alloc);
        assert!(!result, "map_page must return false when allocator is exhausted");
    }

    /// Mapping two addresses that share intermediate tables reuses them.
    #[test]
    fn test_shared_intermediate_tables() {
        let mut slab  = AlignedSlab::new(16);
        let mut alloc = slab.allocator(16);
        let mut pml4  = Box::new(PageTable::new());

        // Two adjacent 4 KB pages share PML4 + PDPT + PD + PT.
        let va0: u64 = 0x0000_0000_0010_0000;
        let va1: u64 = 0x0000_0000_0011_0000;
        let phys0: u64 = 0xAAAA_0000;
        let phys1: u64 = 0xBBBB_0000;

        assert!(map_page(&mut pml4, va0, phys0, PTE_PRESENT, &mut alloc));
        let pages_used_first = 3; // PDPT + PD + PT
        let base_after_first = alloc.current_base();

        assert!(map_page(&mut pml4, va1, phys1, PTE_PRESENT, &mut alloc));
        // The second map_page must NOT allocate any new intermediate tables
        // (all three levels are already present from the first mapping).
        assert_eq!(alloc.current_base(), base_after_first,
                   "second map_page must reuse all {} intermediate tables", pages_used_first);

        assert_eq!(translate_address(&pml4, va0), Some(phys0));
        assert_eq!(translate_address(&pml4, va1), Some(phys1));
    }

    // ── unmap_page ────────────────────────────────────────────────────────────

    /// unmap_page clears the PT entry; translate_address returns None afterward.
    #[test]
    fn test_unmap_clears_entry() {
        let mut slab  = AlignedSlab::new(16);
        let mut alloc = slab.allocator(16);
        let mut pml4  = Box::new(PageTable::new());

        let virt: u64 = 0x0000_0000_0010_0000;
        let phys: u64 = 0x0000_0000_0020_0000;

        assert!(map_page(&mut pml4, virt, phys, PTE_PRESENT | PTE_WRITABLE, &mut alloc));
        assert_eq!(translate_address(&pml4, virt), Some(phys));

        let unmapped = unmap_page(&mut pml4, virt);
        assert!(unmapped, "unmap_page must return true for a mapped 4 KB page");
        assert_eq!(translate_address(&pml4, virt), None,
                   "translate_address must return None after unmap");
    }

    /// unmap_page returns false for an address that was never mapped.
    #[test]
    fn test_unmap_absent_returns_false() {
        let pml4 = Box::new(PageTable::new());
        // This would require a mut ref; create one with Box::into_raw trick.
        let mut pml4 = pml4;
        let result = unmap_page(&mut pml4, 0x0000_0000_0010_0000);
        assert!(!result, "unmap_page must return false for unmapped address");
    }

    /// unmap_page returns false when the PD entry still points to a huge page.
    #[test]
    fn test_unmap_rejects_huge_page() {
        // Build a PD entry that looks like a present huge page (PTE_HUGE_PAGE set).
        let mut pml4 = Box::new(PageTable::new());
        // For virt = 0x200000 (2 MB), pml4[0].pdpt[0].pd[1] is the PD entry.
        // We manually construct the chain without splitting.
        let mut slab  = AlignedSlab::new(4);
        let mut alloc = slab.allocator(4);

        // Allocate PDPT and PD tables manually.
        let pdpt_phys = alloc.allocate(4096).unwrap() as u64;
        let pd_phys   = alloc.allocate(4096).unwrap() as u64;

        // Wire PML4[0] → PDPT, PDPT[0] → PD.
        pml4.entries[0] = (pdpt_phys & PTE_ADDR_MASK) | PTE_PRESENT | PTE_WRITABLE;
        unsafe {
            phys_to_table(pdpt_phys).entries[0] =
                (pd_phys & PTE_ADDR_MASK) | PTE_PRESENT | PTE_WRITABLE;
            // Set PD[0] as a 2 MB huge page covering phys 0x0.
            phys_to_table(pd_phys).entries[0] =
                PTE_PRESENT | PTE_WRITABLE | PTE_HUGE_PAGE;
        }

        // virt 0x0 → pml4[0], pdpt[0], pd[0] which is a huge page.
        let result = unmap_page(&mut pml4, 0x0);
        assert!(!result, "unmap_page must return false for a huge-page PD entry");
    }

    // ── split_huge_page_global is exercised in kernel integration (boot) ──────
    // It uses the global allocator (init_global_allocator) which would race
    // with other tests; integration coverage comes from the QEMU boot test.

    // ── remap_page_flags ─────────────────────────────────────────────────────

    /// remap_page_flags replaces the flag bits while preserving the physical address.
    #[test]
    fn test_remap_flags_preserves_phys() {
        let mut slab  = AlignedSlab::new(16);
        let mut alloc = slab.allocator(16);
        let mut pml4  = Box::new(PageTable::new());

        let virt: u64 = 0x0000_0000_0010_0000;
        let phys: u64 = 0x0000_0000_0020_0000;

        assert!(map_page(&mut pml4, virt, phys, PTE_PRESENT | PTE_WRITABLE, &mut alloc));

        // Remap as read-only + NX: WRITABLE cleared, NO_EXECUTE set.
        let ok = remap_page_flags(&mut pml4, virt, PTE_PRESENT | PTE_NO_EXECUTE);
        assert!(ok, "remap_page_flags must return true for a mapped 4 KB page");

        // Physical address unchanged.
        assert_eq!(translate_address(&pml4, virt), Some(phys),
                   "physical address must be preserved after flag change");

        // Inspect the PT entry directly.
        let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
        let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
        let pd_idx   = ((virt >> 21) & 0x1FF) as usize;
        let pt_idx   = ((virt >> 12) & 0x1FF) as usize;

        let pdpt_phys = pml4.entries[pml4_idx] & PTE_ADDR_MASK;
        let pd_phys   = unsafe { phys_to_table(pdpt_phys) }.entries[pdpt_idx] & PTE_ADDR_MASK;
        let pt_phys   = unsafe { phys_to_table(pd_phys) }.entries[pd_idx] & PTE_ADDR_MASK;
        let entry     = unsafe { phys_to_table(pt_phys) }.entries[pt_idx];

        assert!(entry & PTE_PRESENT    != 0, "PRESENT must remain set");
        assert!(entry & PTE_WRITABLE   == 0, "WRITABLE must be cleared");
        assert!(entry & PTE_NO_EXECUTE != 0, "NO_EXECUTE must be set");
        assert_eq!(entry & PTE_ADDR_MASK, phys & PTE_ADDR_MASK,
                   "physical frame address must be unchanged");
    }

    /// remap_page_flags returns false for an address whose PML4 entry is absent.
    #[test]
    fn test_remap_flags_absent_pml4() {
        let mut pml4 = Box::new(PageTable::new());
        let ok = remap_page_flags(&mut pml4, 0x0000_1000_0000_0000, PTE_PRESENT);
        assert!(!ok, "must return false when PML4 entry is absent");
    }

    /// remap_page_flags returns false when the PT entry is not present.
    #[test]
    fn test_remap_flags_absent_pt_entry() {
        let mut slab  = AlignedSlab::new(8);
        let mut alloc = slab.allocator(8);
        let mut pml4  = Box::new(PageTable::new());

        // Map one page, then try to remap the adjacent (unmapped) page.
        let va_a: u64 = 0x0000_0000_0010_0000;
        assert!(map_page(&mut pml4, va_a, 0x1000, PTE_PRESENT, &mut alloc));

        let va_b: u64 = va_a + 0x1000; // adjacent, never mapped
        let ok = remap_page_flags(&mut pml4, va_b, PTE_PRESENT);
        assert!(!ok, "must return false for an unmapped PT slot");
    }

    /// remap_page_flags returns false when the PD entry is a huge page.
    #[test]
    fn test_remap_flags_rejects_huge_page() {
        let mut pml4 = Box::new(PageTable::new());
        let mut slab  = AlignedSlab::new(4);
        let mut alloc = slab.allocator(4);

        let pdpt_phys = alloc.allocate(4096).unwrap() as u64;
        let pd_phys   = alloc.allocate(4096).unwrap() as u64;

        pml4.entries[0] = (pdpt_phys & PTE_ADDR_MASK) | PTE_PRESENT | PTE_WRITABLE;
        unsafe {
            phys_to_table(pdpt_phys).entries[0] =
                (pd_phys & PTE_ADDR_MASK) | PTE_PRESENT | PTE_WRITABLE;
            phys_to_table(pd_phys).entries[0] =
                PTE_PRESENT | PTE_WRITABLE | PTE_HUGE_PAGE;
        }

        let ok = remap_page_flags(&mut pml4, 0x0, PTE_PRESENT);
        assert!(!ok, "must return false when PD entry is still a huge page");
    }
}
