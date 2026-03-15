/// Voluntary context switch between two kernel-mode tasks.
///
/// # Protocol
///
/// When a new process is created (`ProcessControlBlock::new`), the entry point is
/// written as a fake return address at the very top of its kernel stack, and
/// `ctx.rsp` points to that slot.  On the first switch, `ret` pops the entry
/// point and jumps there.
///
/// On every subsequent switch, the process's saved `rsp` points to the return
/// address that was pushed by the `call switch_context` instruction.  Restoring
/// `rsp` and executing `ret` resumes the process at the instruction after the
/// original `call`.
///
/// # TaskContext field offsets (must match `core_kernel::process::pcb::TaskContext`)
/// ```text
///  rbx  =  0    rbp  =  8    r12 = 16    r13 = 24    r14 = 32    r15 = 40
///  rax  = 48    rcx  = 56    rdx = 64    rsi = 72    rdi = 80
///  r8   = 88    r9   = 96    r10 =104    r11 =112
///  rsp  =120    rip  =128    rflags=136
/// ```
use core_kernel::process::pcb::TaskContext;

/// Switch from the task whose context is at `*old` to the task at `*new`.
///
/// Callee-saved registers and `rsp` are saved into `*old`; they are restored
/// from `*new`, then execution resumes at the return address on the new stack.
///
/// If `new_pml4 != 0` the PML4 table is loaded into CR3 after the stack switch,
/// flushing the TLB and activating the new address space.  Pass `0` when both
/// tasks share the same page table (e.g. while all processes are kernel-mode).
///
/// Interrupts are disabled for the duration of the switch and re-enabled by
/// the `sti` executed just before `ret`.
///
/// # Safety
/// Both context pointers must be valid, non-null, and point to correctly
/// initialised `TaskContext` structs.  The stacks they reference must be valid
/// kernel stacks.  If `new_pml4 != 0` it must be a 4 KB-aligned physical
/// address of a PML4 that identity-maps at least the currently executing code.
#[unsafe(naked)]
pub unsafe extern "C" fn switch_context(
    old:      *mut TaskContext,
    new:      *const TaskContext,
    new_pml4: u64,
) {
    // System V AMD64 ABI: rdi = old, rsi = new, rdx = new_pml4
    core::arch::naked_asm!(
        "cli",                        // no interrupts during switch

        // ── Save callee-saved registers and rsp into old context ─────────────
        "mov  [rdi +   0], rbx",
        "mov  [rdi +   8], rbp",
        "mov  [rdi +  16], r12",
        "mov  [rdi +  24], r13",
        "mov  [rdi +  32], r14",
        "mov  [rdi +  40], r15",
        // rsp: the return address of this call is already at [rsp]
        "mov  [rdi + 120], rsp",

        // ── Restore callee-saved registers and rsp from new context ──────────
        // (rdx still holds new_pml4 — the restore does not touch it)
        "mov  rbx, [rsi +   0]",
        "mov  rbp, [rsi +   8]",
        "mov  r12, [rsi +  16]",
        "mov  r13, [rsi +  24]",
        "mov  r14, [rsi +  32]",
        "mov  r15, [rsi +  40]",
        "mov  rsp, [rsi + 120]",      // switch to new stack

        // ── Conditionally switch address space ───────────────────────────────
        // Skip CR3 write when new_pml4 == 0 (same address space).
        "test rdx, rdx",
        "jz   2f",
        "mov  cr3, rdx",              // flush TLB + load new PML4
        "2:",

        "sti",                        // re-enable interrupts
        "ret",                        // pop return address from new stack → jump
    );
}
