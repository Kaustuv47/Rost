use core::mem;
use super::tss::TaskStateSegment;

/// A standard 8-byte segment descriptor (null / code / data).
#[repr(C)]
#[derive(Copy, Clone)]
pub struct GdtEntry {
    pub limit_low:            u16,
    pub base_low:             u16,
    pub base_mid:             u8,
    pub access:               u8,
    pub limit_high_and_flags: u8,
    pub base_high:            u8,
}

impl GdtEntry {
    pub const fn null() -> Self {
        GdtEntry { limit_low: 0, base_low: 0, base_mid: 0, access: 0,
                   limit_high_and_flags: 0, base_high: 0 }
    }

    /// 64-bit ring-0 code segment (selector 0x08).
    pub const fn code_ring0() -> Self {
        GdtEntry {
            limit_low: 0xFFFF, base_low: 0, base_mid: 0,
            access: 0x9A,               // P=1 DPL=0 S=1 E=1 DC=0 RW=1 A=0
            limit_high_and_flags: 0xAF, // G=1 L=1 (64-bit) D=0 limit_high=0xF
            base_high: 0,
        }
    }

    /// 64-bit ring-0 data segment (selector 0x10).
    pub const fn data_ring0() -> Self {
        GdtEntry {
            limit_low: 0xFFFF, base_low: 0, base_mid: 0,
            access: 0x92,               // P=1 DPL=0 S=1 E=0 DC=0 RW=1 A=0
            limit_high_and_flags: 0xCF, // G=1 B=1 limit_high=0xF
            base_high: 0,
        }
    }

    /// 64-bit ring-3 data segment (selector 0x18 | RPL=3 = 0x1B).
    pub const fn data_ring3() -> Self {
        GdtEntry {
            limit_low: 0xFFFF, base_low: 0, base_mid: 0,
            access: 0xF2,               // P=1 DPL=3 S=1 E=0 DC=0 RW=1 A=0
            limit_high_and_flags: 0xCF,
            base_high: 0,
        }
    }

    /// 64-bit ring-3 code segment (selector 0x20 | RPL=3 = 0x23).
    pub const fn code_ring3() -> Self {
        GdtEntry {
            limit_low: 0xFFFF, base_low: 0, base_mid: 0,
            access: 0xFA,               // P=1 DPL=3 S=1 E=1 DC=0 RW=1 A=0
            limit_high_and_flags: 0xAF, // G=1 L=1 (64-bit) D=0
            base_high: 0,
        }
    }

    #[inline] pub const fn code() -> Self { Self::code_ring0() }
    #[inline] pub const fn data() -> Self { Self::data_ring0() }
}

#[repr(C, packed)]
pub struct GdtDescriptor {
    pub size:   u16,
    pub offset: u64,
}

/// Seven-entry GDT:
/// ```text
/// 0x00  null
/// 0x08  ring-0 code   (SYSCALL CS)
/// 0x10  ring-0 data   (SYSCALL SS; SYSRET base — CS+16=0x20, SS+8=0x18)
/// 0x18  ring-3 data   (SYSRET SS)
/// 0x20  ring-3 code   (SYSRET CS)
/// 0x28  TSS low       (lower 8 bytes of 16-byte 64-bit system descriptor)
/// 0x30  TSS high      (upper 8 bytes)
/// ```
#[repr(C)]
pub struct GlobalDescriptorTable {
    #[allow(dead_code)]
    entries:  [GdtEntry; 5],   // selectors 0x00 – 0x20
    tss_low:  u64,             // selector 0x28 — lower half of TSS descriptor
    tss_high: u64,             // selector 0x30 — upper half
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
            tss_low:  0,
            tss_high: 0,
        }
    }

    /// Encode `tss` as a 16-byte 64-bit available-TSS descriptor into slots 5/6.
    ///
    /// Call this once before `load_tss()`.
    pub fn install_tss(&mut self, tss: *const TaskStateSegment) {
        let base  = tss as u64;
        let limit = (mem::size_of::<TaskStateSegment>() - 1) as u64;

        // Low word (8 bytes):
        //  [15: 0] limit[15:0]          [31:16] base[15:0]
        //  [39:32] base[23:16]          [47:40] type=0x89 (P=1 DPL=0 type=9=avail-TSS64)
        //  [51:48] limit[19:16]         [55:52] flags=0 (G=0 byte-granular)
        //  [63:56] base[31:24]
        self.tss_low =
              (limit        & 0xFFFF)
            | ((base        & 0x00FF_FFFF) << 16)
            | (0x89u64      << 40)
            | (((limit >> 16) & 0xF) << 48)
            | (((base  >> 24) & 0xFF) << 56);

        // High word (8 bytes):
        //  [31:0] base[63:32]   [63:32] reserved = 0
        self.tss_high = (base >> 32) & 0xFFFF_FFFF;
    }

    pub fn load(&self) {
        let desc = GdtDescriptor {
            size:   (mem::size_of::<Self>() - 1) as u16,
            offset: self as *const Self as u64,
        };

        unsafe {
            core::arch::asm!(
                "lgdt [{}]",
                in(reg) &desc,
                options(nostack, preserves_flags)
            );
            // Far return to reload CS with ring-0 code selector (0x08).
            core::arch::asm!(
                "push 0x8",
                "lea  rax, [rip + 2f]",
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
