use core::mem;

/// Global Descriptor Table entry.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct GdtEntry {
    pub limit_low:           u16,
    pub base_low:            u16,
    pub base_mid:            u8,
    pub access:              u8,
    pub limit_high_and_flags: u8,
    pub base_high:           u8,
}

impl GdtEntry {
    pub const fn null() -> Self {
        GdtEntry {
            limit_low: 0, base_low: 0, base_mid: 0,
            access: 0, limit_high_and_flags: 0, base_high: 0,
        }
    }

    /// 64-bit ring-0 code segment (selector 0x08).
    pub const fn code_ring0() -> Self {
        GdtEntry {
            limit_low: 0xFFFF,
            base_low: 0, base_mid: 0,
            access: 0x9A,                // P=1 DPL=0 S=1 E=1 DC=0 RW=1 A=0
            limit_high_and_flags: 0xAF,  // G=1 L=1 (64-bit) D=0 limit_high=0xF
            base_high: 0,
        }
    }

    /// 64-bit ring-0 data segment (selector 0x10).
    pub const fn data_ring0() -> Self {
        GdtEntry {
            limit_low: 0xFFFF,
            base_low: 0, base_mid: 0,
            access: 0x92,                // P=1 DPL=0 S=1 E=0 DC=0 RW=1 A=0
            limit_high_and_flags: 0xCF,  // G=1 B=1 limit_high=0xF
            base_high: 0,
        }
    }

    /// 64-bit ring-3 data segment (selector 0x18 | RPL=3 = 0x1B).
    /// SYSRET sets SS = STAR[63:48] + 8 = 0x10 + 8 = 0x18.
    pub const fn data_ring3() -> Self {
        GdtEntry {
            limit_low: 0xFFFF,
            base_low: 0, base_mid: 0,
            access: 0xF2,                // P=1 DPL=3 S=1 E=0 DC=0 RW=1 A=0
            limit_high_and_flags: 0xCF,
            base_high: 0,
        }
    }

    /// 64-bit ring-3 code segment (selector 0x20 | RPL=3 = 0x23).
    /// SYSRET sets CS = STAR[63:48] + 16 = 0x10 + 16 = 0x20.
    pub const fn code_ring3() -> Self {
        GdtEntry {
            limit_low: 0xFFFF,
            base_low: 0, base_mid: 0,
            access: 0xFA,                // P=1 DPL=3 S=1 E=1 DC=0 RW=1 A=0
            limit_high_and_flags: 0xAF,  // G=1 L=1 (64-bit) D=0
            base_high: 0,
        }
    }

    // Kept for compatibility with existing callers.
    #[inline] pub const fn code() -> Self { Self::code_ring0() }
    #[inline] pub const fn data() -> Self { Self::data_ring0() }
}

#[repr(C, packed)]
pub struct GdtDescriptor {
    pub size:   u16,
    pub offset: u64,
}

/// Five-entry GDT:
/// ```text
/// 0x00  null
/// 0x08  ring-0 code   (SYSCALL CS)
/// 0x10  ring-0 data   (SYSCALL SS; also SYSRET base — CS+16=0x20, SS+8=0x18)
/// 0x18  ring-3 data   (SYSRET SS)
/// 0x20  ring-3 code   (SYSRET CS)
/// ```
pub struct GlobalDescriptorTable {
    #[allow(dead_code)] // read by CPU via LGDT pointer, not by Rust
    entries: [GdtEntry; 5],
}

impl GlobalDescriptorTable {
    pub const fn new() -> Self {
        GlobalDescriptorTable {
            entries: [
                GdtEntry::null(),
                GdtEntry::code_ring0(),
                GdtEntry::data_ring0(),
                GdtEntry::data_ring3(),
                GdtEntry::code_ring3(),
            ],
        }
    }

    pub fn load(&self) {
        let gdt_descriptor = GdtDescriptor {
            size:   (mem::size_of::<Self>() - 1) as u16,
            offset: self as *const Self as u64,
        };

        unsafe {
            core::arch::asm!(
                "lgdt [{}]",
                in(reg) &gdt_descriptor,
                options(nostack, preserves_flags)
            );
            // Far return to reload CS with ring-0 code selector (0x08).
            core::arch::asm!(
                "push 0x8",
                "lea rax, [rip + 2f]",
                "push rax",
                "retfq",
                "2:",
                options(nostack)
            );
            // Reload data segment registers with ring-0 data selector (0x10).
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
