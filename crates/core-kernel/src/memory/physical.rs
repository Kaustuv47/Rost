// ── Global physical allocator (single-core, bump-style) ───────────────────────
//
// Initialised once by the kernel during stage 1 memory management.  After that
// point it is written only from ring-0 code with interrupts disabled (syscall
// path uses SFMASK to clear IF; timer ISR runs with IF=0).  Safe to access as
// `static mut` on a single-core system.

static mut GLOBAL_ALLOC_BASE: usize = 0;
static mut GLOBAL_ALLOC_NEXT: usize = 0;
static mut GLOBAL_ALLOC_END:  usize = 0;

/// Initialise the kernel-wide physical page allocator.
///
/// `base` is the first usable physical address; `size` is the total byte count.
/// This must be called exactly once before any call to [`global_alloc_4k`].
pub fn init_global_allocator(base: usize, size: usize) {
    unsafe {
        GLOBAL_ALLOC_BASE = base;
        GLOBAL_ALLOC_NEXT = base;
        GLOBAL_ALLOC_END  = base + size;
    }
}

/// Allocate one 4 KB-aligned physical page from the global bump allocator.
///
/// Returns the physical address of the page, or `None` if physical memory
/// is exhausted.
pub fn global_alloc_4k() -> Option<u64> {
    unsafe {
        if GLOBAL_ALLOC_NEXT + 4096 > GLOBAL_ALLOC_END { return None; }
        let addr = GLOBAL_ALLOC_NEXT;
        GLOBAL_ALLOC_NEXT += 4096;
        Some(addr as u64)
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
}
