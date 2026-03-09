use core::mem;

/// Global Descriptor Table (GDT) entry
#[repr(C)]
#[derive(Copy, Clone)]
pub struct GdtEntry {
    pub limit_low: u16,
    pub base_low: u16,
    pub base_mid: u8,
    pub access: u8,
    pub limit_high_and_flags: u8,
    pub base_high: u8,
}

impl GdtEntry {
    /// Create a null GDT entry
    pub const fn null() -> Self {
        GdtEntry {
            limit_low: 0,
            base_low: 0,
            base_mid: 0,
            access: 0,
            limit_high_and_flags: 0,
            base_high: 0,
        }
    }

    /// Create a 64-bit code segment GDT entry
    pub const fn code() -> Self {
        GdtEntry {
            limit_low: 0xFFFF,
            base_low: 0,
            base_mid: 0,
            access: 0x9A,               // Present, Ring 0, executable, readable
            limit_high_and_flags: 0xAF, // G=1, L=1 (64-bit), D=0, limit_high=0xF
            base_high: 0,
        }
    }

    /// Create a data segment GDT entry
    pub const fn data() -> Self {
        GdtEntry {
            limit_low: 0xFFFF,
            base_low: 0,
            base_mid: 0,
            access: 0x92,               // Present, Ring 0, writable data
            limit_high_and_flags: 0xCF, // G=1, 4KB granularity
            base_high: 0,
        }
    }
}

/// GDT descriptor for LGDT instruction
#[repr(C, packed)]
pub struct GdtDescriptor {
    pub size: u16,
    pub offset: u64,
}

/// Global Descriptor Table
pub struct GlobalDescriptorTable {
    entries: [GdtEntry; 3], // Null, Code, Data
}

impl GlobalDescriptorTable {
    /// Create a new GDT
    pub const fn new() -> Self {
        GlobalDescriptorTable {
            entries: [
                GdtEntry::null(),
                GdtEntry::code(),
                GdtEntry::data(),
            ],
        }
    }

    /// Load GDT into CPU
    pub fn load(&self) {
        let ptr = self as *const Self as u64;
        let limit = (mem::size_of::<Self>() - 1) as u16;

        let gdt_descriptor = GdtDescriptor {
            size: limit,
            offset: ptr,
        };

        unsafe {
            // Load GDT descriptor
            core::arch::asm!(
            "lgdt [{}]",
            in(reg) &gdt_descriptor,
            options(nostack, preserves_flags)
            );

            // Reload code segment using far return
            core::arch::asm!(
            "push 0x8",              // Code segment selector
            "lea rax, [rip + 2f]",  // Load return address (label 2)
            "push rax",
            "retfq",                 // Far return
            "2:",
            options(nostack)
            );

            // Reload data segment
            core::arch::asm!(
            "mov ds, ax",
            "mov es, ax",
            "mov ss, ax",
            in("ax") 0x10u16,  // Data segment selector
            options(nostack, preserves_flags)
            );
        }
    }
}

/// Interrupt Descriptor Table (IDT) entry
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
    /// Create a null IDT entry
    pub const fn null() -> Self {
        IdtEntry {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            reserved: 0,
        }
    }

    /// Create an interrupt gate entry
    pub fn interrupt_gate(handler: u64, selector: u16) -> Self {
        IdtEntry {
            offset_low: (handler & 0xFFFF) as u16,
            selector,
            ist: 0,
            type_attr: 0x8E, // Present, Ring 0, Interrupt gate
            offset_mid: ((handler >> 16) & 0xFFFF) as u16,
            offset_high: ((handler >> 32) & 0xFFFFFFFF) as u32,
            reserved: 0,
        }
    }
}

/// IDT descriptor for LIDT instruction
#[repr(C, packed)]
pub struct IdtDescriptor {
    pub size: u16,
    pub offset: u64,
}

/// Interrupt Descriptor Table
pub struct InterruptDescriptorTable {
    entries: [IdtEntry; 256], // 256 possible interrupts
}

impl InterruptDescriptorTable {
    /// Create a new IDT
    pub const fn new() -> Self {
        InterruptDescriptorTable {
            entries: [IdtEntry::null(); 256],
        }
    }

    /// Set an IDT entry
    pub fn set_entry(&mut self, index: u8, entry: IdtEntry) {
        self.entries[index as usize] = entry;
    }

    /// Load IDT into CPU
    pub fn load(&self) {
        let ptr = self as *const Self as u64;
        let limit = (mem::size_of::<Self>() - 1) as u16;

        let idt_descriptor = IdtDescriptor {
            size: limit,
            offset: ptr,
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

/// Enable interrupts
pub fn enable_interrupts() {
    unsafe {
        core::arch::asm!("sti", options(nostack, preserves_flags));
    }
}

/// Disable interrupts
pub fn disable_interrupts() {
    unsafe {
        core::arch::asm!("cli", options(nostack, preserves_flags));
    }
}

/// Halt the CPU
pub fn halt() {
    unsafe {
        core::arch::asm!("hlt", options(nostack, preserves_flags));
    }
}
