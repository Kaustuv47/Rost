pub mod gdt;
pub mod idt;
pub mod syscall;
pub mod tss;

pub use gdt::GlobalDescriptorTable;
pub use idt::{IdtEntry, InterruptDescriptorTable};
pub use tss::{set_rsp0, load_tss, init_tss};

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

/// Enable hardware spatial-isolation features.
///
/// Sets:
///   * **EFER.NXE** (MSR 0xC000_0080 bit 11) — activates the No-Execute (XD)
///     page attribute so `PTE_NO_EXECUTE` pages cannot be executed.
///   * **CR0.WP** (bit 16) — kernel cannot write to read-only pages.
///   * **CR4.SMEP** (bit 20) — supervisor cannot *execute* user-mode pages.
///   * **CR4.SMAP** (bit 21) — supervisor cannot *access* user-mode pages
///     without explicitly setting RFLAGS.AC.
///
/// Call this *before* `activate_page_table()`.  The page table must not mark
/// any kernel-code pages as user-mode (`PTE_USER`) — enabling SMEP/SMAP with
/// kernel code in user pages causes an immediate fault.
pub fn init_protection() {
    // EFER.NXE
    let efer = rdmsr(0xC000_0080);
    wrmsr(0xC000_0080, efer | (1 << 11));

    unsafe {
        // CR0.WP (bit 16)
        let cr0: u64;
        core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nostack, nomem));
        core::arch::asm!("mov cr0, {}", in(reg) cr0 | (1u64 << 16), options(nostack, nomem));

        // CR4.SMEP (bit 20) + CR4.SMAP (bit 21)
        let cr4: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nostack, nomem));
        core::arch::asm!("mov cr4, {}",
            in(reg) cr4 | (1u64 << 20) | (1u64 << 21),
            options(nostack, nomem));
    }
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

/// Advance the global scheduler by one tick and perform a context switch when
/// the running process's quantum (or cpu_budget) has expired.
///
/// Called after each `hlt` returns (i.e., after the timer ISR has incremented
/// `TICK_COUNT`).  At this call site the CPU is in the shell/idle loop, so
/// all callee-saved registers belong to the current Rust frame and are saved
/// by `switch_context` into the old `TaskContext`.
///
/// On every preemption this function:
///   1. Updates `CURRENT_PID` to the new process's PID.
///   2. Writes the new process's `kernel_rsp` into TSS.RSP0 (ring-3 safety).
///   3. Calls `switch_context(old, new, pml4)` which saves callee-saved regs,
///      restores them from the new context, and `ret`s to the new process.
pub fn tick_scheduler() {
    if let Some(sched) = core_kernel::scheduler::get_global() {
        if let Some((old, new, pml4, kernel_rsp)) = sched.timer_tick() {
            if let Some(pid) = sched.current_process() {
                core_kernel::scheduler::CURRENT_PID
                    .store(pid.as_u32(), core::sync::atomic::Ordering::Relaxed);
            }
            unsafe {
                tss::set_rsp0(kernel_rsp);
                crate::context::switch_context(old, new, pml4);
            }
        }
    }
}
