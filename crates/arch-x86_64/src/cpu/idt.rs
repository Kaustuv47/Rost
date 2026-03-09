use core::mem;

/// Interrupt Descriptor Table entry
#[repr(C)]
#[derive(Copy, Clone)]
pub struct IdtEntry {
    pub offset_low: u16,
    pub selector: u16,
    pub ist: u8,
    pub type_attr: u8,
    pub offset_mid: u16,
    pub offset_high: u32,
    pub reserved: u32,
}

impl IdtEntry {
    pub const fn null() -> Self {
        IdtEntry {
            offset_low: 0, selector: 0, ist: 0, type_attr: 0,
            offset_mid: 0, offset_high: 0, reserved: 0,
        }
    }

    pub fn interrupt_gate(handler: u64, selector: u16) -> Self {
        IdtEntry {
            offset_low:  (handler & 0xFFFF) as u16,
            selector,
            ist: 0,
            type_attr: 0x8E, // Present, Ring 0, Interrupt gate
            offset_mid:  ((handler >> 16) & 0xFFFF) as u16,
            offset_high: ((handler >> 32) & 0xFFFF_FFFF) as u32,
            reserved: 0,
        }
    }
}

#[repr(C, packed)]
pub struct IdtDescriptor {
    pub size: u16,
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

    pub fn load(&self) {
        let idt_descriptor = IdtDescriptor {
            size: (mem::size_of::<Self>() - 1) as u16,
            offset: self as *const Self as u64,
        };

        unsafe {
            core::arch::asm!(
                "lidt [{}]",
                in(reg) &idt_descriptor,
                options(nostack, preserves_flags)
            );
        }
    }
}
