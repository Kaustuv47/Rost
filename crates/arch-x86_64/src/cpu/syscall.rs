/// SYSCALL / SYSRET initialisation and entry stub.
///
/// MSR layout
/// ----------
/// EFER  (0xC0000080): bit 0 = SCE (System-Call Extensions)
/// STAR  (0xC0000081): bits[47:32] = ring-0 CS for SYSCALL
///                     bits[63:48] = "ring-3 base" for SYSRET
///                     SYSRET sets CS = base+16 (0x20), SS = base+8 (0x18)
/// LSTAR (0xC0000082): 64-bit entry RIP for SYSCALL
/// SFMASK(0xC0000084): RFLAGS bits to CLEAR on entry (IF + DF)
///
/// Syscall calling convention (mirrors Linux x86_64):
///   rax = syscall number
///   rdi, rsi, rdx, r10, r8, r9 = arguments
///   rcx = saved user RIP  (by CPU)
///   r11 = saved user RFLAGS (by CPU)
///   return value in rax
use super::{rdmsr, wrmsr};

// MSR addresses
const MSR_EFER:   u32 = 0xC000_0080;
const MSR_STAR:   u32 = 0xC000_0081;
const MSR_LSTAR:  u32 = 0xC000_0082;
const MSR_SFMASK: u32 = 0xC000_0084;

/// Initialise SYSCALL/SYSRET MSRs.
///
/// After this call a `syscall` instruction executed in ring 3 jumps to
/// `syscall_entry` with interrupts disabled and on the ring-0 stack (TODO: TSS).
pub fn init() {
    // Enable System Call Extensions in EFER.
    let efer = rdmsr(MSR_EFER);
    wrmsr(MSR_EFER, efer | 1);

    // STAR: ring-0 CS = 0x08 (bits[47:32]), ring-3 base = 0x10 (bits[63:48]).
    let star: u64 = (0x0010u64 << 48) | (0x0008u64 << 32);
    wrmsr(MSR_STAR, star);

    // LSTAR: entry RIP.
    wrmsr(MSR_LSTAR, syscall_entry as *const () as u64);

    // SFMASK: clear IF (bit 9) and DF (bit 10) on entry.
    wrmsr(MSR_SFMASK, (1 << 9) | (1 << 10));
}

/// Raw SYSCALL entry point.
///
/// The CPU has already saved user RIP → rcx and user RFLAGS → r11 and
/// cleared IF and DF.  We save callee-saved registers, dispatch to the Rust
/// handler, then restore and execute SYSRETQ.
///
/// TODO: switch to the per-process kernel stack via TSS.RSP0 before touching
/// the stack — required before running real ring-3 code.
#[unsafe(naked)]
pub unsafe extern "C" fn syscall_entry() {
    core::arch::naked_asm!(
        // Save callee-saved + argument-carry registers that the ABI requires us
        // to preserve across a function call.
        "push rcx",    // user RIP (saved by CPU)
        "push r11",    // user RFLAGS (saved by CPU)
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // r10 carries the 4th syscall argument (mirrors Linux convention).
        // Move it to rcx so the Rust dispatcher receives it in the right place.
        "mov rcx, r10",

        // TODO: call actual Rust dispatcher
        // For now return ENOSYS (-1).
        "mov rax, 0xFFFFFFFFFFFFFFFF",

        // Restore callee-saved registers.
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "pop r11",     // restore RFLAGS for SYSRETQ
        "pop rcx",     // restore RIP for SYSRETQ

        // SYSRETQ: restores CS from STAR+16, SS from STAR+8, RIP from rcx,
        //          RFLAGS from r11 (re-enables IF if it was set).
        "sysretq",
    );
}
