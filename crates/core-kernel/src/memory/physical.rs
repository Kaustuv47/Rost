// ── Global physical allocator (single-core, bitmap free-list) ─────────────────
//
// Replaces the original bump allocator with a bitmap allocator so that
// freed frames (process stacks, page tables) can be re-used.
//
// IEC 61508 §7.4.5: safety-critical software must reclaim resources from
// terminated processes.  A pure bump allocator can never satisfy this
// requirement in long-running deployments.
//
// Design:
//   FREE_BITMAP — 1024 × u64 = 65 536 bits, one per 4 KB frame, covering
//   the same 256 MB range as FRAME_TAGS.  Bit = 1 means the frame is free.
//
//   global_alloc_4k()   — scan FREE_BITMAP for the first set bit (trailing
//                         zeros on each u64 word — O(N/64) with hardware CTZ).
//   global_free_4k(phys) — set the corresponding bit and retag as Free.
//
// Thread / interrupt safety: single-core; interrupts are masked on the syscall
// path (SFMASK clears IF) and in timer ISRs (IDT interrupt-gate).  No spinlock
// needed.  Same model as the previous bump allocator.

static mut GLOBAL_ALLOC_BASE: usize = 0;
static mut GLOBAL_ALLOC_NEXT: usize = 0; // kept for diagnostic / compat
static mut GLOBAL_ALLOC_END:  usize = 0;

// ── Free-frame bitmap ─────────────────────────────────────────────────────────

/// Free-frame bitmap: 1024 × u64 = 65 536 bits, one per 4 KB frame.
/// Bit = 1 → frame is free and available for allocation.
/// Zero-initialised in BSS — all frames start as "not free" until
/// `init_global_allocator` sets bits for the usable range.
static mut FREE_BITMAP: [u64; MAX_TRACKED_FRAMES / 64] =
    [0u64; MAX_TRACKED_FRAMES / 64];

/// Mark bit `idx` as free (= 1) in FREE_BITMAP.
#[inline]
fn bitmap_set(idx: usize) {
    debug_assert!(idx < MAX_TRACKED_FRAMES);
    unsafe { FREE_BITMAP[idx / 64] |= 1u64 << (idx % 64); }
}

/// Mark bit `idx` as allocated (= 0) in FREE_BITMAP.
#[inline]
fn bitmap_clear(idx: usize) {
    debug_assert!(idx < MAX_TRACKED_FRAMES);
    unsafe { FREE_BITMAP[idx / 64] &= !(1u64 << (idx % 64)); }
}

/// Find the index of the first free frame (bit = 1) in FREE_BITMAP.
/// Returns `None` if no free frame exists in the entire bitmap.
#[inline]
fn bitmap_find_free() -> Option<usize> {
    // Safety: single-core, IF=0 on hot path.
    for (wi, &word) in unsafe { (core::ptr::addr_of!(FREE_BITMAP) as *const [u64; 1024]).as_ref().unwrap().iter().enumerate() } {
        if word != 0 {
            return Some(wi * 64 + word.trailing_zeros() as usize);
        }
    }
    None
}

// ── Physical frame tracker ─────────────────────────────────────────────────────
//
// Each 4 KB frame in the allocatable region is tagged with a FrameKind.
// Provides spatial auditing required by IEC 61508 §7.4.5 (resource partitioning)
// and is a prerequisite for the future free-list allocator.
//
// Coverage: MAX_TRACKED_FRAMES × 4 KB = 256 MB of allocatable space.
// Storage:  65 536 × 1 byte = 64 KB BSS — fixed, bounded, no heap required.

/// The kind of a physical 4 KB frame.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Free       = 0, // unallocated
    KernelData = 1, // kernel page tables, stacks, and other kernel allocations
    UserOwned  = 2, // allocated to a user-space process
    Guard      = 3, // guard page — deliberately unmapped (PTE cleared)
    Mmio       = 4, // MMIO / reserved region — do not allocate
}

/// Maximum number of simultaneously tracked 4 KB frames (256 MB).
pub const MAX_TRACKED_FRAMES: usize = 65536;

static mut FRAME_TAGS: [u8; MAX_TRACKED_FRAMES] = [0u8; MAX_TRACKED_FRAMES];

/// Compute the index of a physical address in FRAME_TAGS.
///
/// Returns `None` if `phys` is outside the tracked range (below base or
/// beyond `MAX_TRACKED_FRAMES` frames above base).
#[inline]
fn frame_index(phys: u64) -> Option<usize> {
    let base = unsafe { GLOBAL_ALLOC_BASE } as u64;
    if phys < base { return None; }
    let idx = ((phys - base) / 4096) as usize;
    if idx >= MAX_TRACKED_FRAMES { None } else { Some(idx) }
}

/// Tag the 4 KB frame at `phys` with `kind`.
///
/// Silently ignored for addresses outside the tracked range.
pub fn frame_tag(phys: u64, kind: FrameKind) {
    if let Some(idx) = frame_index(phys) {
        unsafe { FRAME_TAGS[idx] = kind as u8; }
    }
}

/// Return the [`FrameKind`] of the 4 KB frame at `phys`.
///
/// Frames outside the tracked range are reported as [`FrameKind::Free`].
pub fn frame_kind(phys: u64) -> FrameKind {
    match frame_index(phys) {
        Some(idx) => match unsafe { FRAME_TAGS[idx] } {
            1 => FrameKind::KernelData,
            2 => FrameKind::UserOwned,
            3 => FrameKind::Guard,
            4 => FrameKind::Mmio,
            _ => FrameKind::Free,
        },
        None => FrameKind::Free,
    }
}

/// Return per-kind frame counts for the allocatable region.
///
/// Returns `[free, kernel_data, user_owned, guard, mmio]`.
/// Useful for diagnostic output and memory accounting.
pub fn frame_stats() -> [u32; 5] {
    let total = unsafe {
        if GLOBAL_ALLOC_END > GLOBAL_ALLOC_BASE {
            ((GLOBAL_ALLOC_END - GLOBAL_ALLOC_BASE) / 4096).min(MAX_TRACKED_FRAMES)
        } else { 0 }
    };
    let mut stats = [0u32; 5];
    for i in 0..total {
        let k = unsafe { FRAME_TAGS[i] } as usize;
        if k < 5 { stats[k] += 1; }
    }
    stats
}

/// Initialise the kernel-wide physical page allocator.
///
/// `base` is the first usable physical address; `size` is the total byte count.
/// This must be called exactly once before any call to [`global_alloc_4k`] or
/// [`global_free_4k`].
///
/// All frames in `[base, base+size)` are marked free in the bitmap so they
/// are immediately available for allocation.  Frames outside that range
/// remain unavailable.
pub fn init_global_allocator(base: usize, size: usize) {
    unsafe {
        GLOBAL_ALLOC_BASE = base;
        GLOBAL_ALLOC_NEXT = base; // high-water mark for diagnostics
        GLOBAL_ALLOC_END  = base + size;
        // Clear bitmap and frame tags — all frames start as "not free" /
        // untracked until we mark the usable range below.
        FREE_BITMAP = [0u64; MAX_TRACKED_FRAMES / 64];
        FRAME_TAGS  = [0u8;  MAX_TRACKED_FRAMES];
        // Mark every frame in the usable range as free.
        let frames = (size / 4096).min(MAX_TRACKED_FRAMES);
        for i in 0..frames {
            bitmap_set(i);
            // FRAME_TAGS[i] stays 0 = FrameKind::Free (BSS default).
        }
    }
}

/// Allocate one 4 KB-aligned physical page from the global free-list allocator.
///
/// Scans the free-frame bitmap for the first available frame, marks it
/// allocated, and tags it [`FrameKind::KernelData`].  The caller may
/// reclassify with [`frame_tag`] if appropriate (e.g. [`FrameKind::UserOwned`]
/// for a user-process mapping).
///
/// Returns the physical address of the page, or `None` if no free frame
/// is available.  This is O(N/64) where N = `MAX_TRACKED_FRAMES`.
pub fn global_alloc_4k() -> Option<u64> {
    let idx = bitmap_find_free()?;
    let phys = (unsafe { GLOBAL_ALLOC_BASE } + idx * 4096) as u64;
    if phys >= unsafe { GLOBAL_ALLOC_END as u64 } { return None; }
    bitmap_clear(idx);
    frame_tag(phys, FrameKind::KernelData);
    // Advance the high-water mark for diagnostics (monotone; never decreases).
    unsafe {
        if phys as usize + 4096 > GLOBAL_ALLOC_NEXT {
            GLOBAL_ALLOC_NEXT = phys as usize + 4096;
        }
    }
    Some(phys)
}

/// Return a 4 KB frame to the global free-list allocator.
///
/// `phys` must be 4 KB-aligned and within the range initialised by
/// [`init_global_allocator`].  Calling `global_free_4k` on an address that
/// was never allocated (double-free) is silently ignored — the frame is
/// simply marked free again.  Callers must not use the frame after this call.
///
/// The frame is re-tagged as [`FrameKind::Free`] so that [`frame_stats`]
/// correctly reflects the reclaimed memory.
///
/// IEC 61508 §7.4.5: required for process termination to reclaim kernel
/// stacks and page-table frames.
pub fn global_free_4k(phys: u64) {
    if let Some(idx) = frame_index(phys) {
        bitmap_set(idx);
        frame_tag(phys, FrameKind::Free);
    }
}

// ── Per-init allocator (local use during boot) ─────────────────────────────────

/// Bump-style physical memory allocator
pub struct PhysicalAllocator {
    heap_start: usize,
    heap_remaining: usize,
}

impl PhysicalAllocator {
    pub fn new(start: usize, size: usize) -> Self {
        PhysicalAllocator { heap_start: start, heap_remaining: size }
    }

    /// Allocate `size` bytes, aligned to 4 KB pages
    pub fn allocate(&mut self, size: usize) -> Option<usize> {
        let aligned = ((size + 4095) / 4096) * 4096;
        if aligned > self.heap_remaining {
            return None;
        }
        let addr = self.heap_start;
        self.heap_start += aligned;
        self.heap_remaining -= aligned;
        Some(addr)
    }

    pub fn deallocate(&mut self, _addr: usize, _size: usize) {
        // Bump allocator — deallocation is a no-op
    }

    /// Return the next free physical address (i.e. where the next allocation would land).
    pub fn current_base(&self) -> usize {
        self.heap_start
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────
//
// Run with: cargo test -p core-kernel --target x86_64-apple-darwin

#[cfg(test)]
mod tests {
    use super::*;

    // ── PhysicalAllocator (local bump allocator) ──────────────────────────────

    /// allocate() returns sequential 4 KB-aligned addresses.
    #[test]
    fn test_allocate_sequential() {
        let base = 0x1000_0000usize;
        let mut alloc = PhysicalAllocator::new(base, 3 * 4096);

        let a = alloc.allocate(4096).expect("first allocation");
        let b = alloc.allocate(4096).expect("second allocation");
        let c = alloc.allocate(4096).expect("third allocation");

        assert_eq!(a, base);
        assert_eq!(b, base + 4096);
        assert_eq!(c, base + 2 * 4096);
    }

    /// Requesting a sub-page size still consumes a full 4 KB (rounds up).
    #[test]
    fn test_allocate_rounds_up_to_page() {
        let base = 0x2000_0000usize;
        let mut alloc = PhysicalAllocator::new(base, 2 * 4096);

        let a = alloc.allocate(1).expect("1-byte request");
        let b = alloc.allocate(1).expect("second 1-byte request");
        // Each 1-byte request should consume a full page.
        assert_eq!(b, a + 4096);
    }

    /// allocate() returns None once capacity is exhausted.
    #[test]
    fn test_allocate_oom() {
        let mut alloc = PhysicalAllocator::new(0xDEAD_0000, 4096);

        let a = alloc.allocate(4096);
        assert!(a.is_some(), "first page must succeed");

        let b = alloc.allocate(4096);
        assert!(b.is_none(), "second page must fail (OOM)");
    }

    /// A zero-size allocator is immediately exhausted.
    #[test]
    fn test_allocate_zero_capacity() {
        let mut alloc = PhysicalAllocator::new(0x1000, 0);
        assert!(alloc.allocate(4096).is_none(), "zero-capacity allocator must be OOM");
    }

    /// current_base() advances by 4 KB per allocation.
    #[test]
    fn test_current_base_tracks_allocations() {
        let base = 0x3000_0000usize;
        let mut alloc = PhysicalAllocator::new(base, 8 * 4096);

        assert_eq!(alloc.current_base(), base);
        alloc.allocate(4096);
        assert_eq!(alloc.current_base(), base + 4096);
        alloc.allocate(4096);
        assert_eq!(alloc.current_base(), base + 2 * 4096);
    }

    // ── Global state tests (single sequential function) ────────────────────────
    //
    // GLOBAL_ALLOC_BASE/NEXT/END, FREE_BITMAP, and FRAME_TAGS are static muts
    // shared across the entire process.  All tests that touch these must live
    // in ONE test function to prevent data races with concurrent test threads.

    /// Combined test for: frame tracker, bitmap allocator, free + reallocate.
    #[test]
    fn test_frame_tracker() {
        // Use a distinct base address that is unlikely to collide with other tests.
        const BASE: usize = 0x4000_0000;
        const PAGES: usize = 8;
        unsafe {
            GLOBAL_ALLOC_BASE = BASE;
            GLOBAL_ALLOC_NEXT = BASE;
            GLOBAL_ALLOC_END  = BASE + PAGES * 4096;
            // Zero the tag array and bitmap for this range.
            for i in 0..PAGES {
                FRAME_TAGS[i] = 0;
                // Mark as "not free" in bitmap (we test tag functions directly,
                // not global_alloc_4k, so bitmap state doesn't matter here).
                FREE_BITMAP[i / 64] &= !(1u64 << (i % 64));
            }
        }

        // Fresh init: all frames are Free (tag == 0).
        for i in 0..PAGES {
            let addr = (BASE + i * 4096) as u64;
            assert_eq!(frame_kind(addr), FrameKind::Free,
                       "frame {i} should start as Free");
        }

        // Tag individual frames.
        let addr0 = BASE as u64;
        let addr1 = (BASE + 4096) as u64;
        let addr2 = (BASE + 2 * 4096) as u64;
        let addr3 = (BASE + 3 * 4096) as u64;

        frame_tag(addr0, FrameKind::KernelData);
        frame_tag(addr1, FrameKind::UserOwned);
        frame_tag(addr2, FrameKind::Guard);
        frame_tag(addr3, FrameKind::Mmio);

        assert_eq!(frame_kind(addr0), FrameKind::KernelData);
        assert_eq!(frame_kind(addr1), FrameKind::UserOwned);
        assert_eq!(frame_kind(addr2), FrameKind::Guard);
        assert_eq!(frame_kind(addr3), FrameKind::Mmio);

        // Frames 4–7 remain Free.
        for i in 4..PAGES {
            let addr = (BASE + i * 4096) as u64;
            assert_eq!(frame_kind(addr), FrameKind::Free);
        }

        // frame_stats: [free=4, kdata=1, user=1, guard=1, mmio=1].
        let stats = frame_stats();
        assert_eq!(stats[0], 4, "free frames");
        assert_eq!(stats[1], 1, "kernel-data frames");
        assert_eq!(stats[2], 1, "user-owned frames");
        assert_eq!(stats[3], 1, "guard frames");
        assert_eq!(stats[4], 1, "mmio frames");

        // Addresses outside the tracked range are reported as Free.
        let out_of_range = (BASE as u64).wrapping_sub(4096);
        assert_eq!(frame_kind(out_of_range), FrameKind::Free,
                   "address below base must be reported as Free");

        // ── Part 2: Bitmap allocator — fresh init, alloc all, OOM, free + realloc
        {
            const BBASE:  usize = 0x5000_0000;
            const BPAGES: usize = 4;

            init_global_allocator(BBASE, BPAGES * 4096);

            let a0 = global_alloc_4k().expect("page 0");
            let a1 = global_alloc_4k().expect("page 1");
            let a2 = global_alloc_4k().expect("page 2");
            let a3 = global_alloc_4k().expect("page 3");

            for &addr in &[a0, a1, a2, a3] {
                assert!(addr >= BBASE as u64 && addr < (BBASE + BPAGES * 4096) as u64,
                        "allocation {addr:#x} outside expected range");
                assert_eq!(addr % 4096, 0, "allocation must be 4 KB-aligned");
                assert_eq!(frame_kind(addr), FrameKind::KernelData);
            }
            assert_ne!(a0, a1); assert_ne!(a0, a2); assert_ne!(a0, a3);
            assert_ne!(a1, a2); assert_ne!(a1, a3); assert_ne!(a2, a3);

            // Exhausted.
            assert!(global_alloc_4k().is_none(), "must be OOM after all pages allocated");

            // Free one frame, reallocate it — must get same address back.
            global_free_4k(a1);
            assert_eq!(frame_kind(a1), FrameKind::Free, "freed frame must be tagged Free");
            let a1b = global_alloc_4k().expect("reallocate freed frame");
            assert_eq!(a1b, a1, "reallocated address must equal the freed address");
            assert_eq!(frame_kind(a1b), FrameKind::KernelData);
            assert!(global_alloc_4k().is_none());

            // Free all, verify stats, reallocate all.
            global_free_4k(a0); global_free_4k(a1b);
            global_free_4k(a2); global_free_4k(a3);
            let stats = frame_stats();
            assert_eq!(stats[0], BPAGES as u32, "all frames free");
            assert_eq!(stats[1], 0);
            for _ in 0..BPAGES {
                assert!(global_alloc_4k().is_some(), "must reallocate all pages");
            }
            assert!(global_alloc_4k().is_none(), "OOM again after reallocating all");

            // free outside tracked range is silently ignored.
            let below = (BBASE as u64).wrapping_sub(4096);
            global_free_4k(below);
            assert!(global_alloc_4k().is_none(), "OOM persists after bogus free");
        }

        // ── Part 3: free → retag → frame_stats consistency
        {
            const CBASE:  usize = 0x6000_0000;
            const CPAGES: usize = 2;

            init_global_allocator(CBASE, CPAGES * 4096);
            let p0 = global_alloc_4k().expect("p0");
            let p1 = global_alloc_4k().expect("p1");

            // Reclassify p0 as UserOwned, then free it.
            frame_tag(p0, FrameKind::UserOwned);
            assert_eq!(frame_kind(p0), FrameKind::UserOwned);
            global_free_4k(p0);
            assert_eq!(frame_kind(p0), FrameKind::Free);

            let stats = frame_stats();
            assert_eq!(stats[0], 1, "one free frame (p0 reclaimed)");
            assert_eq!(stats[1], 1, "one KernelData frame (p1)");
            assert_eq!(stats[2], 0, "no UserOwned after free");
            let _ = p1;
        }
    }
}
