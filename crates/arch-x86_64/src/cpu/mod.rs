pub mod gdt;
pub mod idt;
pub mod syscall;

pub use gdt::GlobalDescriptorTable;
pub use idt::{IdtEntry, InterruptDescriptorTable};

// ── Basic CPU control ──────────────────────────────────────────────────────────

pub fn enable_interrupts() {
    unsafe { core::arch::asm!("sti", options(nostack, preserves_flags)); }
}

pub fn disable_interrupts() {
    unsafe { core::arch::asm!("cli", options(nostack, preserves_flags)); }
}

pub fn halt() {
    unsafe { core::arch::asm!("hlt", options(nostack, preserves_flags)); }
}

// ── MSR access ────────────────────────────────────────────────────────────────

/// Read a Model-Specific Register.
pub fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") lo,
            out("edx") hi,
            options(nostack, nomem),
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

/// Write a Model-Specific Register.
pub fn wrmsr(msr: u32, value: u64) {
    let lo = value as u32;
    let hi = (value >> 32) as u32;
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") lo,
            in("edx") hi,
            options(nostack, nomem),
        );
    }
}

// ── Control registers ─────────────────────────────────────────────────────────

/// Read CR2 (faulting virtual address set by page-fault handler).
pub fn read_cr2() -> u64 {
    let val: u64;
    unsafe {
        core::arch::asm!(
            "mov {}, cr2",
            out(reg) val,
            options(nostack, nomem),
        );
    }
    val
}

/// Load a PML4 physical address into CR3, activating the page table.
///
/// # Safety
/// `pml4_phys` must point to a correctly-initialised, 4 KB-aligned PML4 table
/// that identity-maps at least the currently executing code.  Loading an
/// invalid CR3 causes an immediate page fault.
pub unsafe fn activate_page_table(pml4_phys: u64) {
    core::arch::asm!(
        "mov cr3, {}",
        in(reg) pml4_phys,
        options(nostack, nomem),
    );
}
