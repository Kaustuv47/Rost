/// Physical memory allocator (simplified bump allocator)
pub struct PhysicalAllocator {
    heap_start: usize,
    heap_remaining: usize,
}

impl PhysicalAllocator {
    /// Initialize the physical allocator with a memory region
    pub fn new(start: usize, size: usize) -> Self {
        PhysicalAllocator {
            heap_start: start,
            heap_remaining: size,
        }
    }

    /// Allocate physical memory (simplified - returns sequential blocks)
    pub fn allocate(&mut self, size: usize) -> Option<usize> {
        // Simplified allocator — in production use a proper buddy system
        let aligned_size = ((size + 4095) / 4096) * 4096; // Align to 4KB pages
        if aligned_size > self.heap_remaining {
            return None;
        }
        let addr = self.heap_start;
        self.heap_start += aligned_size;
        self.heap_remaining -= aligned_size;
        Some(addr)
    }

    /// Deallocate physical memory
    pub fn deallocate(&mut self, _addr: usize, _size: usize) {
        // Simplified - real implementation needs proper tracking
    }
}

/// Page table entry for x86_64
#[repr(transparent)]
#[derive(Copy, Clone)]
pub struct PageTableEntry(u64);

impl PageTableEntry {
    /// Create a page table entry
    pub fn new(physical_addr: u64, present: bool, writable: bool) -> Self {
        let mut entry = physical_addr & 0x000FFFFF_FFFFF000; // Physical address
        if present {
            entry |= 0x1; // Present flag
        }
        if writable {
            entry |= 0x2; // Read/Write flag
        }
        PageTableEntry(entry)
    }

    /// Get the physical address from the entry
    pub fn address(&self) -> u64 {
        self.0 & 0x000FFFFF_FFFFF000
    }

    /// Check if entry is present
    pub fn present(&self) -> bool {
        (self.0 & 0x1) != 0
    }
}

/// Virtual page table (4-level paging for x86_64)
#[repr(align(4096))]
pub struct PageTable {
    entries: [PageTableEntry; 512],
}

impl PageTable {
    /// Create a new page table
    pub fn new() -> Self {
        PageTable {
            entries: [PageTableEntry(0); 512],
        }
    }

    /// Set a page table entry
    pub fn set_entry(&mut self, index: usize, entry: PageTableEntry) {
        if index < 512 {
            self.entries[index] = entry;
        }
    }

    /// Get a page table entry
    pub fn get_entry(&self, index: usize) -> PageTableEntry {
        if index < 512 {
            self.entries[index]
        } else {
            PageTableEntry(0)
        }
    }
}

/// Map a virtual address to a physical address
pub fn map_page(
    page_table: &mut PageTable,
    virtual_addr: u64,
    physical_addr: u64,
    writable: bool,
) {
    let index = ((virtual_addr >> 12) & 0x1FF) as usize;
    let entry = PageTableEntry::new(physical_addr, true, writable);
    page_table.set_entry(index, entry);
}

/// Get physical address from virtual address
pub fn translate_address(page_table: &PageTable, virtual_addr: u64) -> Option<u64> {
    let index = ((virtual_addr >> 12) & 0x1FF) as usize;
    let entry = page_table.get_entry(index);

    if entry.present() {
        let page_offset = virtual_addr & 0xFFF;
        Some(entry.address() + page_offset)
    } else {
        None
    }
}
