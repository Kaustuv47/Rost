/// x86_64 page table entry (single-level, simplified)
#[repr(transparent)]
#[derive(Copy, Clone)]
pub struct PageTableEntry(u64);

impl PageTableEntry {
    pub fn new(physical_addr: u64, present: bool, writable: bool) -> Self {
        let mut entry = physical_addr & 0x000FFFFF_FFFFF000;
        if present  { entry |= 0x1; }
        if writable { entry |= 0x2; }
        PageTableEntry(entry)
    }

    pub fn address(&self) -> u64 { self.0 & 0x000FFFFF_FFFFF000 }
    pub fn present(&self) -> bool { (self.0 & 0x1) != 0 }
}

/// Single-level page table (512 entries, 4 KB aligned)
#[repr(align(4096))]
pub struct PageTable {
    entries: [PageTableEntry; 512],
}

impl PageTable {
    pub fn new() -> Self {
        PageTable { entries: [PageTableEntry(0); 512] }
    }

    pub fn set_entry(&mut self, index: usize, entry: PageTableEntry) {
        if index < 512 { self.entries[index] = entry; }
    }

    pub fn get_entry(&self, index: usize) -> PageTableEntry {
        if index < 512 { self.entries[index] } else { PageTableEntry(0) }
    }
}

pub fn map_page(table: &mut PageTable, virtual_addr: u64, physical_addr: u64, writable: bool) {
    let index = ((virtual_addr >> 12) & 0x1FF) as usize;
    table.set_entry(index, PageTableEntry::new(physical_addr, true, writable));
}

pub fn translate_address(table: &PageTable, virtual_addr: u64) -> Option<u64> {
    let index = ((virtual_addr >> 12) & 0x1FF) as usize;
    let entry = table.get_entry(index);
    if entry.present() {
        Some(entry.address() + (virtual_addr & 0xFFF))
    } else {
        None
    }
}
