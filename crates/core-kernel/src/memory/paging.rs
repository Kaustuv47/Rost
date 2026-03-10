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

pub const PTE_PRESENT:   u64 = 1 << 0;
pub const PTE_WRITABLE:  u64 = 1 << 1;
pub const PTE_USER:      u64 = 1 << 2;
pub const PTE_ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

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

/// Map `virt` → `phys` in the given PML4 table.
///
/// Intermediate tables are allocated from `alloc` as needed.
/// Returns `true` on success, `false` if allocation fails.
pub fn map_page(
    pml4:     &mut PageTable,
    virt:     u64,
    phys:     u64,
    writable: bool,
    alloc:    &mut PhysicalAllocator,
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

    let flags = PTE_PRESENT | if writable { PTE_WRITABLE } else { 0 };
    unsafe { phys_to_table(pt_phys).entries[pt_idx] = (phys & PTE_ADDR_MASK) | flags; }
    true
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
    let pt = unsafe { phys_to_table(pd.entries[pd_idx] & PTE_ADDR_MASK) };

    if pt.entries[pt_idx] & PTE_PRESENT == 0 { return None; }
    Some((pt.entries[pt_idx] & PTE_ADDR_MASK) | (virt & 0xFFF))
}
