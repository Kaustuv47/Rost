/// Fixed-size object pools for the kernel's hot-path allocations.
///
/// # Why pools?
///
/// The global bitmap allocator (`global_alloc_4k`) scans up to 65 536 bits
/// with `trailing_zeros()` — O(N/64) worst-case.  For allocations that occur
/// on the timer-ISR path (intermediate page-table nodes via `map_page_global`)
/// a provable Worst-Case Execution Time (WCET) requires O(1) allocation.
///
/// IEC 61508 §7.4.5: fixed-size pools provide a formal upper bound on
/// resource usage and enable static resource analysis at certification time.
///
/// # Pools provided
///
/// | Pool                | Type      | Capacity | Storage |
/// |---------------------|-----------|----------|---------|
/// | Page-table frames   | O(1) LIFO | 512 × 4 KB | 4 KB BSS (addresses) |
///
/// # Thread / interrupt safety
///
/// All pools are `static mut`, safe on a single-core system where the syscall
/// path clears IF (via SFMASK) and timer ISRs run with IF=0.

use super::physical::{global_alloc_4k, global_free_4k};

// ── Page-table frame pool ─────────────────────────────────────────────────────

/// Number of 4 KB frames pre-allocated for page-table intermediate nodes.
///
/// 512 frames × 4 KB = 2 MB.  Sufficient for 32 processes each with a full
/// 4-level PML4 (≤ 16 intermediate nodes per process at most = 512 total).
/// Must fit in `u16` if pool indices are stored as u16; we store addresses
/// directly so this can be up to `usize::MAX`.
pub const PT_POOL_CAP: usize = 512;

/// LIFO stack of free 4 KB frame addresses available for page-table use.
///
/// Invariant: `PT_POOL[i]` is a valid, non-zero physical address for every
/// `i < PT_POOL_TOP`; all entries at `i >= PT_POOL_TOP` are zero.
static mut PT_POOL:     [u64; PT_POOL_CAP] = [0u64; PT_POOL_CAP];
/// Logical top of the `PT_POOL` stack.  0 = empty, `PT_POOL_CAP` = full.
static mut PT_POOL_TOP: usize = 0;

// ── Public API — page-table pool ─────────────────────────────────────────────

/// Initialise the page-table frame pool by drawing `count` frames from the
/// global physical allocator.
///
/// Must be called exactly once, after
/// [`init_global_allocator`](super::physical::init_global_allocator), before
/// any call to [`pool_alloc_pt`].
///
/// Returns the number of frames actually placed in the pool; may be less than
/// `count` if the global allocator has fewer than `count` free frames.
pub fn pool_init(count: usize) -> usize {
    let want = count.min(PT_POOL_CAP);
    let mut filled = 0usize;
    for _ in 0..want {
        match global_alloc_4k() {
            Some(phys) => { unsafe { PT_POOL[filled] = phys; } filled += 1; }
            None => break,
        }
    }
    unsafe { PT_POOL_TOP = filled; }
    filled
}

/// Allocate one 4 KB frame from the page-table pool.
///
/// The returned frame is **not** zero-filled; callers must zero it before
/// installing it as a page table to avoid stale entries.
///
/// Returns `None` if the pool is exhausted.  Callers that need a fallback
/// should call `global_alloc_4k()` after a pool miss.
///
/// O(1) — single array read + decrement.
#[inline]
pub fn pool_alloc_pt() -> Option<u64> {
    unsafe {
        if PT_POOL_TOP == 0 { return None; }
        PT_POOL_TOP -= 1;
        let phys = PT_POOL[PT_POOL_TOP];
        PT_POOL[PT_POOL_TOP] = 0; // clear stale entry
        Some(phys)
    }
}

/// Return a 4 KB frame to the page-table pool.
///
/// If the pool's free stack is already at `PT_POOL_CAP` (can happen when
/// more frames are freed than the pool originally held), the frame is
/// forwarded to `global_free_4k()` rather than dropped.
///
/// O(1) — single array write + increment.
#[inline]
pub fn pool_free_pt(phys: u64) {
    unsafe {
        if PT_POOL_TOP < PT_POOL_CAP {
            PT_POOL[PT_POOL_TOP] = phys;
            PT_POOL_TOP += 1;
        } else {
            global_free_4k(phys);
        }
    }
}

/// Return the number of frames currently available in the pool.
#[inline]
pub fn pool_available() -> usize { unsafe { PT_POOL_TOP } }

/// Return the maximum number of frames the pool can hold (`PT_POOL_CAP`).
#[inline]
pub fn pool_capacity() -> usize { PT_POOL_CAP }

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // All pool tests share PT_POOL / PT_POOL_TOP — run in a single function
    // to avoid data races with concurrent test threads.

    /// O(1) pool: LIFO order, exhaustion → None, free → realloc, overflow guard.
    #[test]
    fn test_pt_pool() {
        // ── Set up: inject 4 fake "physical" addresses directly ───────────────
        // (host addresses are valid in test mode; we never dereference them)
        const FRAMES: [u64; 4] = [0x1000, 0x2000, 0x3000, 0x4000];
        unsafe {
            PT_POOL_TOP = 0;
            for (i, &f) in FRAMES.iter().enumerate() {
                PT_POOL[i] = f;
            }
            PT_POOL_TOP = FRAMES.len();
        }

        assert_eq!(pool_available(), 4);
        assert_eq!(pool_capacity(), PT_POOL_CAP);

        // ── Part 1: allocate all 4 frames; verify LIFO order ─────────────────
        let a = pool_alloc_pt().expect("frame 0");
        let b = pool_alloc_pt().expect("frame 1");
        let c = pool_alloc_pt().expect("frame 2");
        let d = pool_alloc_pt().expect("frame 3");

        assert_eq!(a, 0x4000, "LIFO: top of stack first");
        assert_eq!(b, 0x3000);
        assert_eq!(c, 0x2000);
        assert_eq!(d, 0x1000);
        assert_eq!(pool_available(), 0);

        // ── Part 2: pool exhausted ────────────────────────────────────────────
        assert!(pool_alloc_pt().is_none(), "pool must be exhausted");

        // ── Part 3: free one → reallocate ─────────────────────────────────────
        pool_free_pt(a);
        assert_eq!(pool_available(), 1);
        let a2 = pool_alloc_pt().expect("reallocate");
        assert_eq!(a2, a, "reallocated address must equal the freed one");
        assert_eq!(pool_available(), 0);

        // ── Part 4: free all, verify available, reallocate all ────────────────
        pool_free_pt(a2);
        pool_free_pt(b);
        pool_free_pt(c);
        pool_free_pt(d);
        assert_eq!(pool_available(), 4);
        for _ in 0..4 { assert!(pool_alloc_pt().is_some()); }
        assert!(pool_alloc_pt().is_none(), "exhausted again");

        // ── Part 5: fill pool to capacity; overflow goes to global allocator ──
        // Re-seed the pool with PT_POOL_CAP frames so we can verify the overflow
        // path without calling global_alloc_4k (which races with other tests).
        // We do this by directly setting PT_POOL_TOP to its max.
        unsafe {
            // Mark pool as full (all entries are don't-care; just testing the guard).
            PT_POOL_TOP = PT_POOL_CAP;
        }
        // pool_free_pt must NOT panic; it calls global_free_4k which handles
        // out-of-range addresses silently.  Use an address outside any real
        // tracked range so global_free_4k's frame_index returns None → no-op.
        pool_free_pt(0xFFFF_FFFF_F000); // far beyond any tracked range
        // Pool top unchanged after overflow.
        assert_eq!(pool_available(), PT_POOL_CAP);

        // ── Reset pool for any subsequent tests ───────────────────────────────
        unsafe { PT_POOL_TOP = 0; }
    }
}
