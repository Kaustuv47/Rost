use core::mem;

/// Interrupt Descriptor Table entry (16 bytes).
#[repr(C)]
#[derive(Copy, Clone)]
pub struct IdtEntry {
    pub offset_low:  u16,
    pub selector:    u16,
    /// Bits[2:0] = IST index (0 = disabled, 1–7 = IST stack).
    pub ist:         u8,
    pub type_attr:   u8,
    pub offset_mid:  u16,
    pub offset_high: u32,
    pub reserved:    u32,
}

impl IdtEntry {
    pub const fn null() -> Self {
        IdtEntry { offset_low: 0, selector: 0, ist: 0, type_attr: 0,
                   offset_mid: 0, offset_high: 0, reserved: 0 }
    }

    /// Standard interrupt gate (ring 0, no IST).
    pub fn interrupt_gate(handler: u64, selector: u16) -> Self {
        IdtEntry {
            offset_low:  (handler & 0xFFFF) as u16,
            selector,
            ist:         0,
            type_attr:   0x8E, // P=1 DPL=0 type=0xE (64-bit interrupt gate)
            offset_mid:  ((handler >> 16) & 0xFFFF) as u16,
            offset_high: ((handler >> 32) & 0xFFFF_FFFF) as u32,
            reserved:    0,
        }
    }

    /// Interrupt gate that unconditionally switches to IST stack `ist` (1–7).
    /// Use for #DF (IST=1), #NMI (IST=2), #MC (IST=3) — they must never reuse
    /// the interrupted task's stack even if it is corrupt.
    pub fn interrupt_gate_ist(handler: u64, selector: u16, ist: u8) -> Self {
        let mut entry = Self::interrupt_gate(handler, selector);
        entry.ist = ist & 0x7;
        entry
    }
}

#[repr(C, packed)]
pub struct IdtDescriptor {
    pub size:   u16,
    pub offset: u64,
}

pub struct InterruptDescriptorTable {
    entries: [IdtEntry; 256],
}

impl InterruptDescriptorTable {
    pub const fn new() -> Self {
        InterruptDescriptorTable { entries: [IdtEntry::null(); 256] }
    }

    pub fn set_entry(&mut self, index: u8, entry: IdtEntry) {
        self.entries[index as usize] = entry;
    }

    /// Set an entry using `usize` index (for filling all 256 slots in a loop).
    pub fn set_entry_usize(&mut self, index: usize, entry: IdtEntry) {
        self.entries[index] = entry;
    }

    pub fn load(&self) {
        let desc = IdtDescriptor {
            size:   (mem::size_of::<Self>() - 1) as u16,
            offset: self as *const Self as u64,
        };
        unsafe {
            core::arch::asm!(
                "lidt [{}]",
                in(reg) &desc,
                options(nostack, preserves_flags)
            );
        }
    }
}
