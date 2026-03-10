/// ISR stubs and exception handlers.
///
/// Exception ISR layout on the stack when the inner handler is called
/// (low → high addresses, rsp = &frame at call site):
///
/// ```text
/// r15 r14 r13 r12 r11 r10 r9 r8 rdi rsi rdx rcx rbx rbp rax
/// error_code   ← 0 (dummy) for #DE; CPU-pushed for #GP / #PF
/// rip cs rflags   ← CPU-pushed (same-privilege, no rsp/ss)
/// ```
///
/// Push order in the naked stubs (last pushed = lowest address = first field):
///   push rax → push rbp → push rbx → push rcx → push rdx → push rsi →
///   push rdi → push r8 → push r9 → push r10 → push r11 → push r12 →
///   push r13 → push r14 → push r15
use super::TICK_COUNT;

// ── Saved register frame ──────────────────────────────────────────────────────

/// All registers saved by an exception ISR stub (matches the push order above).
#[repr(C)]
pub struct ExceptionFrame {
    pub r15:        u64,
    pub r14:        u64,
    pub r13:        u64,
    pub r12:        u64,
    pub r11:        u64,
    pub r10:        u64,
    pub r9:         u64,
    pub r8:         u64,
    pub rdi:        u64,
    pub rsi:        u64,
    pub rdx:        u64,
    pub rcx:        u64,
    pub rbx:        u64,
    pub rbp:        u64,
    pub rax:        u64,
    // CPU-pushed (or dummy 0 for #DE)
    pub error_code: u64,
    pub rip:        u64,
    pub cs:         u64,
    pub rflags:     u64,
}

// ── Serial register dump ──────────────────────────────────────────────────────

fn dump_registers(f: &ExceptionFrame) {
    hal::uart::print_str("  rax="); hal::uart::print_hex(f.rax);
    hal::uart::print_str("  rbx="); hal::uart::print_hex(f.rbx);
    hal::uart::print_str("  rcx="); hal::uart::print_hex(f.rcx);
    hal::uart::print_str("  rdx="); hal::uart::print_hex(f.rdx);
    hal::uart::print_str("\n");
    hal::uart::print_str("  rsi="); hal::uart::print_hex(f.rsi);
    hal::uart::print_str("  rdi="); hal::uart::print_hex(f.rdi);
    hal::uart::print_str("  rbp="); hal::uart::print_hex(f.rbp);
    hal::uart::print_str("  rsp="); hal::uart::print_hex(f.rip.wrapping_add(0)); // placeholder
    hal::uart::print_str("\n");
    hal::uart::print_str("  r8 ="); hal::uart::print_hex(f.r8);
    hal::uart::print_str("  r9 ="); hal::uart::print_hex(f.r9);
    hal::uart::print_str("  r10="); hal::uart::print_hex(f.r10);
    hal::uart::print_str("  r11="); hal::uart::print_hex(f.r11);
    hal::uart::print_str("\n");
    hal::uart::print_str("  r12="); hal::uart::print_hex(f.r12);
    hal::uart::print_str("  r13="); hal::uart::print_hex(f.r13);
    hal::uart::print_str("  r14="); hal::uart::print_hex(f.r14);
    hal::uart::print_str("  r15="); hal::uart::print_hex(f.r15);
    hal::uart::print_str("\n");
    hal::uart::print_str("  rip="); hal::uart::print_hex(f.rip);
    hal::uart::print_str("  rfl="); hal::uart::print_hex(f.rflags);
    hal::uart::print_str("  err="); hal::uart::print_hex(f.error_code);
    hal::uart::print_str("\n");
}

// ── Inner Rust exception handlers ────────────────────────────────────────────

#[cold]
extern "C" fn handle_divide_by_zero(frame: &ExceptionFrame) {
    hal::uart::print_str("\n[EXCEPTION #DE — Division by Zero]\n");
    dump_registers(frame);
    hal::uart::print_str("System halted.\n");
    loop { unsafe { core::arch::asm!("cli", "hlt", options(nostack, nomem)); } }
}

#[cold]
extern "C" fn handle_general_protection(frame: &ExceptionFrame) {
    hal::uart::print_str("\n[EXCEPTION #GP — General Protection Fault]\n");
    dump_registers(frame);
    hal::uart::print_str("System halted.\n");
    loop { unsafe { core::arch::asm!("cli", "hlt", options(nostack, nomem)); } }
}

#[cold]
extern "C" fn handle_page_fault(frame: &ExceptionFrame) {
    let cr2 = crate::cpu::read_cr2();
    hal::uart::print_str("\n[EXCEPTION #PF — Page Fault]\n");
    hal::uart::print_str("  fault addr="); hal::uart::print_hex(cr2);
    hal::uart::print_str("\n");
    dump_registers(frame);
    hal::uart::print_str("System halted.\n");
    loop { unsafe { core::arch::asm!("cli", "hlt", options(nostack, nomem)); } }
}

// ── Naked ISR stubs ───────────────────────────────────────────────────────────
//
// Stack alignment before `call`:
//   CPU pushes 3 × 8 = 24 bytes (rflags, cs, rip) → rsp % 16 becomes 8.
//   We push a dummy error code (1 × 8) + 15 GPRs = 16 × 8 = 128 bytes → rsp
//   is back to …0 (128 % 16 == 0).  So `sub rsp, 8` gives rsp % 16 == 8 needed
//   before `call` (call itself pushes 8 more → rsp % 16 == 0 at function entry).
//
// For exceptions that already push an error code (#GP, #PF) the arithmetic is
// identical: CPU pushes 4 × 8 = 32 bytes, we push 15 × 8 = 120 bytes → total
// 152 bytes.  Same alignment result.

/// #DE — Division by Zero (no CPU error code; we push a dummy 0).
#[unsafe(naked)]
pub unsafe extern "C" fn divide_by_zero_handler() {
    core::arch::naked_asm!(
        "push 0",           // dummy error_code
        "push rax",
        "push rbp",
        "push rbx",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov  rdi, rsp",    // &ExceptionFrame
        "sub  rsp, 8",      // align to 16 before call
        "call {inner}",
        // never reached — handler halts
        "add  rsp, 8",
        "pop  r15", "pop  r14", "pop  r13", "pop  r12",
        "pop  r11", "pop  r10", "pop  r9",  "pop  r8",
        "pop  rdi", "pop  rsi", "pop  rdx", "pop  rcx",
        "pop  rbx", "pop  rbp", "pop  rax",
        "add  rsp, 8",      // skip error_code
        "iretq",
        inner = sym handle_divide_by_zero,
    );
}

/// #GP — General Protection Fault (CPU pushes error code).
#[unsafe(naked)]
pub unsafe extern "C" fn general_protection_fault_handler() {
    core::arch::naked_asm!(
        // error_code already on stack (pushed by CPU)
        "push rax",
        "push rbp",
        "push rbx",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov  rdi, rsp",
        "sub  rsp, 8",
        "call {inner}",
        // never reached
        "add  rsp, 8",
        "pop  r15", "pop  r14", "pop  r13", "pop  r12",
        "pop  r11", "pop  r10", "pop  r9",  "pop  r8",
        "pop  rdi", "pop  rsi", "pop  rdx", "pop  rcx",
        "pop  rbx", "pop  rbp", "pop  rax",
        "add  rsp, 8",
        "iretq",
        inner = sym handle_general_protection,
    );
}

/// #PF — Page Fault (CPU pushes error code).
#[unsafe(naked)]
pub unsafe extern "C" fn page_fault_handler() {
    core::arch::naked_asm!(
        // error_code already on stack
        "push rax",
        "push rbp",
        "push rbx",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov  rdi, rsp",
        "sub  rsp, 8",
        "call {inner}",
        // never reached
        "add  rsp, 8",
        "pop  r15", "pop  r14", "pop  r13", "pop  r12",
        "pop  r11", "pop  r10", "pop  r9",  "pop  r8",
        "pop  rdi", "pop  rsi", "pop  rdx", "pop  rcx",
        "pop  rbx", "pop  rbp", "pop  rax",
        "add  rsp, 8",
        "iretq",
        inner = sym handle_page_fault,
    );
}

/// IRQ0 — PIT timer at 100 Hz.
///
/// Atomically increments TICK_COUNT, sends EOI to the master PIC, and returns.
/// Caller-saved registers are saved/restored; no Rust function call is needed.
#[unsafe(naked)]
pub unsafe extern "C" fn timer_interrupt_handler() {
    core::arch::naked_asm!(
        // Save caller-saved regs (9 × 8 = 72 bytes; after CPU's 24 bytes the
        // total is 96, which is 16-byte aligned — no padding needed).
        "push rax",
        "push rcx",
        "push rdx",
        "push rdi",
        "push rsi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",

        // Atomically increment the 64-bit tick counter.
        "lock inc qword ptr [{tick}]",

        // Send End-Of-Interrupt to the master PIC (port 0x20).
        "mov al, 0x20",
        "out 0x20, al",

        // Restore caller-saved registers.
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rsi",
        "pop rdi",
        "pop rdx",
        "pop rcx",
        "pop rax",

        "iretq",
        tick = sym TICK_COUNT,
    );
}
