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
