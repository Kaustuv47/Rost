use core::mem;

/// Global Descriptor Table entry
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
    pub const fn null() -> Self {
        GdtEntry { limit_low: 0, base_low: 0, base_mid: 0, access: 0, limit_high_and_flags: 0, base_high: 0 }
    }

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

#[repr(C, packed)]
pub struct GdtDescriptor {
    pub size: u16,
    pub offset: u64,
}

pub struct GlobalDescriptorTable {
    #[allow(dead_code)] // read by CPU via LGDT pointer, not by Rust code
    entries: [GdtEntry; 3], // Null, Code, Data
}

impl GlobalDescriptorTable {
    pub const fn new() -> Self {
        GlobalDescriptorTable {
            entries: [GdtEntry::null(), GdtEntry::code(), GdtEntry::data()],
        }
    }

    pub fn load(&self) {
        let gdt_descriptor = GdtDescriptor {
            size: (mem::size_of::<Self>() - 1) as u16,
            offset: self as *const Self as u64,
        };

        unsafe {
            core::arch::asm!(
                "lgdt [{}]",
                in(reg) &gdt_descriptor,
                options(nostack, preserves_flags)
            );
            core::arch::asm!(
                "push 0x8",
                "lea rax, [rip + 2f]",
                "push rax",
                "retfq",
                "2:",
                options(nostack)
            );
            core::arch::asm!(
                "mov ds, ax",
                "mov es, ax",
                "mov ss, ax",
                in("ax") 0x10u16,
                options(nostack, preserves_flags)
            );
        }
    }
}
