/// ISR stubs, exception handlers, and preemptive timer context switch.
///
/// # Stack layout pushed by each exception stub (low → high, rsp = &frame):
/// ```text
///  r15 r14 r13 r12 r11 r10 r9 r8  rdi rsi rdx rcx rbx rbp rax
///  error_code   ← 0 for exceptions without a hardware error code
///  rip cs rflags [rsp ss]  ← CPU-pushed (ss/rsp only on privilege change)
/// ```
///
/// # User-vs-kernel fault detection
/// If `ExceptionFrame.cs & 3 == 3` the fault occurred in ring 3 (user mode).
/// User faults should terminate the process and notify the health monitor
/// (PID 1) rather than halting the system.
use core::sync::atomic::Ordering;
use super::{TICK_COUNT, LAPIC_EOI_ADDR};
use core_kernel::crash_log;

// ── Saved register frame ──────────────────────────────────────────────────────

/// All registers saved by an exception ISR stub.
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
    pub error_code: u64,
    pub rip:        u64,
    pub cs:         u64,
    pub rflags:     u64,
    // rsp / ss only present on ring-3 → ring-0 transition (not yet used)
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

// ── Helper: is the frame from ring-3? ────────────────────────────────────────

#[inline(always)]
fn from_user(f: &ExceptionFrame) -> bool {
    f.cs & 3 == 3
}

// ── Crash record helper ───────────────────────────────────────────────────────

/// Write a crash record to the persistent log and flush over serial.
///
/// Called from every fatal (ring-0 / unrecoverable) exception handler before
/// halting.  Uses the current TICK_COUNT and CURRENT_PID for correlation.
fn record_crash(frame: &ExceptionFrame, vector: u64, cr2: u64) {
    let tick = TICK_COUNT.load(Ordering::Relaxed);
    let pid  = core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed);
    crash_log::write(crash_log::ErrorRecord {
        magic:  0,  // crash_log::write() stamps the real magic
        vector,
        tick,
        pid:    pid as u64,
        rip:    frame.rip,
        rsp:    0,  // ExceptionFrame doesn't expose interrupted RSP (ring-0 same-priv)
        rflags: frame.rflags,
        cr2,
    });
}

// ── Inner Rust exception handlers ────────────────────────────────────────────

#[cold]
extern "C" fn handle_divide_by_zero(frame: &ExceptionFrame) {
    hal::uart::print_str("\n[EXCEPTION #DE — Division by Zero]\n");
    hal::uart::print_str(if from_user(frame) { "  origin: user-mode\n" } else { "  origin: kernel\n" });
    dump_registers(frame);
    if from_user(frame) {
        terminate_faulting_process(0xDE);
    }
    hal::uart::print_str("System halted.\n");
    loop { unsafe { core::arch::asm!("cli", "hlt", options(nostack, nomem)); } }
}

#[cold]
extern "C" fn handle_general_protection(frame: &ExceptionFrame) {
    hal::uart::print_str("\n[EXCEPTION #GP — General Protection Fault]\n");
    hal::uart::print_str("  frame@");
    hal::uart::print_hex(frame as *const ExceptionFrame as u64);
    hal::uart::print_str("\n");
    hal::uart::print_str("  err=");
    hal::uart::print_hex(frame.error_code);
    hal::uart::print_str("\n");
    hal::uart::print_str("  rip=");
    hal::uart::print_hex(frame.rip);
    hal::uart::print_str("\n");
    hal::uart::print_str("  cs=");
    hal::uart::print_hex(frame.cs);
    hal::uart::print_str("\n");
    hal::uart::print_str(if from_user(frame) { "  origin: user-mode\n" } else { "  origin: kernel\n" });
    dump_registers(frame);
    if from_user(frame) {
        terminate_faulting_process(0x0D);
    }
    record_crash(frame, 0x0D, 0);
    hal::uart::print_str("System halted.\n");
    loop { unsafe { core::arch::asm!("cli", "hlt", options(nostack, nomem)); } }
}

#[cold]
extern "C" fn handle_page_fault(frame: &ExceptionFrame) {
    let cr2 = crate::cpu::read_cr2();
    hal::uart::print_str("\n[EXCEPTION #PF — Page Fault]\n");
    hal::uart::print_str("  frame@");
    hal::uart::print_hex(frame as *const ExceptionFrame as u64);
    hal::uart::print_str("\n");
    hal::uart::print_str("  fault addr="); hal::uart::print_hex(cr2);
    hal::uart::print_str("\n");
    hal::uart::print_str("  err=");  hal::uart::print_hex(frame.error_code);
    hal::uart::print_str("  rip=");  hal::uart::print_hex(frame.rip);
    hal::uart::print_str("  cs=");   hal::uart::print_hex(frame.cs);
    hal::uart::print_str("  rfl=");  hal::uart::print_hex(frame.rflags);
    hal::uart::print_str("\n");
    hal::uart::print_str(if from_user(frame) { "  origin: user-mode\n" } else { "  origin: kernel\n" });
    dump_registers(frame);
    if from_user(frame) {
        terminate_faulting_process(0x0E);
    }
    record_crash(frame, 0x0E, cr2);
    hal::uart::print_str("System halted.\n");
    loop { unsafe { core::arch::asm!("cli", "hlt", options(nostack, nomem)); } }
}

#[cold]
extern "C" fn handle_double_fault(frame: &ExceptionFrame) {
    // #DF uses IST1 — always runs on a fresh stack regardless of caller state.
    hal::uart::print_str("\n[EXCEPTION #DF — Double Fault]\n");
    hal::uart::print_str("  FATAL: kernel exception handler faulted.\n");
    dump_registers(frame);
    record_crash(frame, 0x08, 0);
    hal::uart::print_str("  System halted (unrecoverable).\n");
    loop { unsafe { core::arch::asm!("cli", "hlt", options(nostack, nomem)); } }
}

#[cold]
extern "C" fn handle_nmi() {
    // NMI cannot be masked and must complete quickly.
    // In a real system: check ECC status, hardware watchdog, then IRET.
    hal::uart::print_str("\n[NMI — Non-Maskable Interrupt]\n");
    hal::uart::print_str("  (hardware watchdog / ECC signal — logged)\n");
    // Do NOT halt: NMI is typically recoverable (corrected ECC, watchdog ping).
}

#[cold]
extern "C" fn handle_machine_check(frame: &ExceptionFrame) {
    hal::uart::print_str("\n[EXCEPTION #MC — Machine Check]\n");
    hal::uart::print_str("  FATAL: uncorrectable hardware error.\n");
    dump_registers(frame);
    record_crash(frame, 0x12, 0);
    hal::uart::print_str("  System halted.\n");
    loop { unsafe { core::arch::asm!("cli", "hlt", options(nostack, nomem)); } }
}

// ── Ring-3 process fault handling ─────────────────────────────────────────────

/// Scratch area used by `terminate_faulting_process` as the "old" context save
/// target when switching away from a dead ring-3 process.
///
/// `switch_context_noints` requires an `old` context pointer to save the
/// current callee-saved registers into.  For a terminated process we do not
/// care about those registers, so we divert them here and never read them back.
///
/// Safe: single-CPU system; exception handlers are non-reentrant on this path.
static mut EXCEPTION_DEAD_CTX: core_kernel::process::pcb::TaskContext =
    core_kernel::process::pcb::TaskContext::zero();

/// Terminate the current ring-3 process and switch to the next ready process.
///
/// Must NOT call `tick_scheduler_isr()` after `terminate_process()`:
/// `terminate_process` sets `current_process = None`, which causes
/// `timer_tick()` to keep `preempt = false` and return `None` — so no context
/// switch would occur and the exception handler's `iretq` would return to the
/// now-dead process, causing an infinite fault loop and eventually a triple
/// fault / watchdog reset.
///
/// Instead, `force_schedule_next()` picks the next ready process unconditionally
/// and we call `switch_context_noints` directly, saving the dead process's
/// stale registers into `EXCEPTION_DEAD_CTX` (which is never loaded again).
#[cold]
fn terminate_faulting_process(fault_code: u64) -> ! {
    use core_kernel::scheduler::{get_global, CURRENT_PID};

    let pid_raw = CURRENT_PID.load(Ordering::Relaxed);
    hal::uart::print_str("  → terminating ring-3 process PID=");
    hal::uart::print_hex(pid_raw as u64);
    hal::uart::print_str(", notifying init\n");

    if let Some(sched) = get_global() {
        let pid  = core_kernel::process::ProcessId::new(pid_raw);
        let init = core_kernel::process::ProcessId::new(1);

        // Notify init (PID 1) with fault_code in data[0] and faulting PID in data[1].
        let mut msg = core_kernel::ipc::Message::new(pid);
        msg.set_data(0, fault_code);
        msg.set_data(1, pid_raw as u64);
        sched.send_message(pid, init, msg);

        // Remove the dead process.  After this, current_process == None.
        sched.terminate_process(pid);

        // Pick the next ready process directly — bypasses the preempt flag that
        // timer_tick() would never set when current_process is None.
        if let Some((new_ctx, new_pml4, kernel_rsp)) = sched.force_schedule_next() {
            if let Some(next_pid) = sched.current_process() {
                CURRENT_PID.store(next_pid.as_u32(), Ordering::Relaxed);
            }
            // Re-arm the one-shot LAPIC timer so preemption keeps firing after
            // we switch away (this path never returns to tick_scheduler_isr).
            crate::apic::lapic::arm_oneshot(1);
            unsafe {
                crate::cpu::tss::set_rsp0(kernel_rsp);
                crate::context::switch_context_noints(
                    core::ptr::addr_of_mut!(EXCEPTION_DEAD_CTX),
                    new_ctx,
                    new_pml4,
                );
                // switch_context_noints does not return: `ret` inside it jumps
                // to the new process's kernel context.
                core::hint::unreachable_unchecked();
            }
        }
    }

    // No ready process (all terminated or blocked) — halt until a timer tick
    // unblocks something.  Interrupts are already off (exception gate); re-enable
    // them so the timer ISR can fire.
    unsafe { core::arch::asm!("sti", options(nostack, preserves_flags)); }
    loop { unsafe { core::arch::asm!("hlt", options(nostack, nomem)); } }
}

/// Catch-all for any unexpected interrupt/exception vector.
#[cold]
extern "C" fn handle_unexpected(vector: u64, frame: &ExceptionFrame) {
    hal::uart::print_str("\n[UNEXPECTED INTERRUPT vector=0x");
    hal::uart::print_hex(vector);
    hal::uart::print_str("]\n");
    dump_registers(frame);
    // Do not halt — unhandled IRQs should be EOI'd and ignored.
    // For now send EOI to both PIC masters so we don't dead-lock.
    unsafe {
        core::arch::asm!(
            "mov al, 0x20", "out 0x20, al", "out 0xA0, al",
            options(nostack, preserves_flags)
        );
    }
}

// ── Naked ISR stubs ───────────────────────────────────────────────────────────
//
// Alignment note:
//   CPU pushes 24 bytes (rflags/cs/rip) on same-privilege entry.
//   Dummy error code + 15 GPRs = 16 pushes × 8 = 128 bytes.
//   Total = 152 bytes.  152 % 16 = 8.  `sub rsp,8` before `call` → 16-byte aligned.
//
//   For exceptions that push a real error code (13, 14, 8):
//   CPU pushes 32 bytes (error_code + rflags/cs/rip).
//   15 GPRs = 15 × 8 = 120 bytes.  Total = 152.  Same alignment.

macro_rules! exception_stub_no_code {
    ($name:ident, $inner:expr) => {
        #[unsafe(naked)]
        pub unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                "push 0",           // dummy error_code
                "push rax", "push rbp", "push rbx", "push rcx",
                "push rdx", "push rsi", "push rdi",
                "push r8",  "push r9",  "push r10", "push r11",
                "push r12", "push r13", "push r14", "push r15",
                "mov  rcx, rsp",    // &ExceptionFrame — Windows ABI: arg1 in rcx
                "sub  rsp, 8",
                "call {inner}",
                "add  rsp, 8",
                "pop  r15", "pop  r14", "pop  r13", "pop  r12",
                "pop  r11", "pop  r10", "pop  r9",  "pop  r8",
                "pop  rdi", "pop  rsi", "pop  rdx", "pop  rcx",
                "pop  rbx", "pop  rbp", "pop  rax",
                "add  rsp, 8",      // skip error_code
                "iretq",
                inner = sym $inner,
            );
        }
    };
}

macro_rules! exception_stub_with_code {
    ($name:ident, $inner:expr) => {
        #[unsafe(naked)]
        pub unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                // error_code already on stack (pushed by CPU)
                "push rax", "push rbp", "push rbx", "push rcx",
                "push rdx", "push rsi", "push rdi",
                "push r8",  "push r9",  "push r10", "push r11",
                "push r12", "push r13", "push r14", "push r15",
                "mov  rcx, rsp",    // &ExceptionFrame — Windows ABI: arg1 in rcx
                "sub  rsp, 8",
                "call {inner}",
                "add  rsp, 8",
                "pop  r15", "pop  r14", "pop  r13", "pop  r12",
                "pop  r11", "pop  r10", "pop  r9",  "pop  r8",
                "pop  rdi", "pop  rsi", "pop  rdx", "pop  rcx",
                "pop  rbx", "pop  rbp", "pop  rax",
                "add  rsp, 8",
                "iretq",
                inner = sym $inner,
            );
        }
    };
}

exception_stub_no_code!(divide_by_zero_handler,        handle_divide_by_zero);
exception_stub_with_code!(general_protection_fault_handler, handle_general_protection);
exception_stub_with_code!(page_fault_handler,          handle_page_fault);
// #DF has an error code (always 0) and uses IST1.
exception_stub_with_code!(double_fault_handler,        handle_double_fault);

// NMI (no error code, uses IST2).
#[unsafe(naked)]
pub unsafe extern "C" fn nmi_handler() {
    core::arch::naked_asm!(
        "push 0",
        "push rax", "push rbp", "push rbx", "push rcx",
        "push rdx", "push rsi", "push rdi",
        "push r8",  "push r9",  "push r10", "push r11",
        "push r12", "push r13", "push r14", "push r15",
        "sub  rsp, 8",
        "call {inner}",
        "add  rsp, 8",
        "pop  r15", "pop  r14", "pop  r13", "pop  r12",
        "pop  r11", "pop  r10", "pop  r9",  "pop  r8",
        "pop  rdi", "pop  rsi", "pop  rdx", "pop  rcx",
        "pop  rbx", "pop  rbp", "pop  rax",
        "add  rsp, 8",
        "iretq",
        inner = sym handle_nmi,
    );
}

// #MC (no error code, uses IST3).
exception_stub_no_code!(machine_check_handler, handle_machine_check);

// Spurious interrupt (vector 0xFF from LAPIC; just IRET, no EOI).
#[unsafe(naked)]
pub unsafe extern "C" fn spurious_handler() {
    core::arch::naked_asm!("iretq");
}

// ── Catch-all for vectors 0–255 that have no specific handler ────────────────
//
// Each stub pushes its vector number and jumps to the common dispatcher.
// We generate 256 stubs using a macro; unused slots are tiny (≤ 16 bytes each).

#[unsafe(naked)]
pub unsafe extern "C" fn unexpected_interrupt_common() {
    core::arch::naked_asm!(
        // On entry: stack = ... | rflags | cs | rip
        // (CPU did NOT push an error code for most vectors)
        // rcx = vector number (set by the per-vector stub before jmp here)
        // Windows x64 ABI: arg1=rcx (vector), arg2=rdx (frame ptr)
        "push 0",           // dummy error_code
        "push rax", "push rbp", "push rbx", "push rcx",
        "push rdx", "push rsi", "push rdi",
        "push r8",  "push r9",  "push r10", "push r11",
        "push r12", "push r13", "push r14", "push r15",
        // rcx still holds the vector number (push saved it to the frame but
        // did not clear the register).
        "mov  rdx, rsp",    // rdx = &ExceptionFrame — Windows ABI: arg2 in rdx
        "sub  rsp, 8",
        "call {inner}",     // handle_unexpected(vector=rcx, frame=rdx)
        "add  rsp, 8",
        "pop  r15", "pop  r14", "pop  r13", "pop  r12",
        "pop  r11", "pop  r10", "pop  r9",  "pop  r8",
        "pop  rdi", "pop  rsi", "pop  rdx", "pop  rcx",
        "pop  rbx", "pop  rbp", "pop  rax",
        "add  rsp, 8",  // skip dummy error_code
        "iretq",
        inner = sym handle_unexpected,
    );
}

/// Generate one catch-all stub per vector (used for all unregistered slots).
/// The stub puts the vector number in rcx (Windows ABI arg1) then jumps to
/// the common handler.
macro_rules! unexpected_stub {
    ($name:ident, $vec:expr) => {
        #[unsafe(naked)]
        pub unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                concat!("mov rcx, ", $vec),
                "jmp {common}",
                common = sym unexpected_interrupt_common,
            );
        }
    };
}

// Generate stubs for vectors we don't individually handle (2,8,15,16–31,33–254).
unexpected_stub!(unexpected_vec1,   1);
unexpected_stub!(unexpected_vec3,   3);
unexpected_stub!(unexpected_vec4,   4);
unexpected_stub!(unexpected_vec5,   5);
unexpected_stub!(unexpected_vec6,   6);
unexpected_stub!(unexpected_vec7,   7);
unexpected_stub!(unexpected_vec9,   9);
unexpected_stub!(unexpected_vec10, 10);
unexpected_stub!(unexpected_vec11, 11);
unexpected_stub!(unexpected_vec12, 12);
unexpected_stub!(unexpected_vec15, 15);
unexpected_stub!(unexpected_vec16, 16);
unexpected_stub!(unexpected_vec17, 17);
unexpected_stub!(unexpected_vec19, 19);
unexpected_stub!(unexpected_vec20, 20);
unexpected_stub!(unexpected_vec21, 21);
// vectors 22–31 (reserved)
unexpected_stub!(unexpected_vec22, 22);
unexpected_stub!(unexpected_vec23, 23);
unexpected_stub!(unexpected_vec24, 24);
unexpected_stub!(unexpected_vec25, 25);
unexpected_stub!(unexpected_vec26, 26);
unexpected_stub!(unexpected_vec27, 27);
unexpected_stub!(unexpected_vec28, 28);
unexpected_stub!(unexpected_vec29, 29);
unexpected_stub!(unexpected_vec30, 30);
unexpected_stub!(unexpected_vec31, 31);
// vectors 33–47 (IRQ1–IRQ15, slave PIC)
unexpected_stub!(unexpected_vec33, 33);
unexpected_stub!(unexpected_vec34, 34);
unexpected_stub!(unexpected_vec35, 35);
unexpected_stub!(unexpected_vec37, 37);
unexpected_stub!(unexpected_vec38, 38);
unexpected_stub!(unexpected_vec39, 39);
unexpected_stub!(unexpected_vec40, 40);
unexpected_stub!(unexpected_vec41, 41);
unexpected_stub!(unexpected_vec42, 42);
unexpected_stub!(unexpected_vec43, 43);
unexpected_stub!(unexpected_vec44, 44);
unexpected_stub!(unexpected_vec45, 45);
unexpected_stub!(unexpected_vec46, 46);
unexpected_stub!(unexpected_vec47, 47);
// vectors 48–254
unexpected_stub!(unexpected_vec48,  48);  unexpected_stub!(unexpected_vec49,  49);
unexpected_stub!(unexpected_vec50,  50);  unexpected_stub!(unexpected_vec51,  51);
unexpected_stub!(unexpected_vec52,  52);  unexpected_stub!(unexpected_vec53,  53);
unexpected_stub!(unexpected_vec54,  54);  unexpected_stub!(unexpected_vec55,  55);
unexpected_stub!(unexpected_vec56,  56);  unexpected_stub!(unexpected_vec57,  57);
unexpected_stub!(unexpected_vec58,  58);  unexpected_stub!(unexpected_vec59,  59);
unexpected_stub!(unexpected_vec60,  60);  unexpected_stub!(unexpected_vec61,  61);
unexpected_stub!(unexpected_vec62,  62);  unexpected_stub!(unexpected_vec63,  63);
unexpected_stub!(unexpected_vec64,  64);  unexpected_stub!(unexpected_vec65,  65);
unexpected_stub!(unexpected_vec66,  66);  unexpected_stub!(unexpected_vec67,  67);
unexpected_stub!(unexpected_vec68,  68);  unexpected_stub!(unexpected_vec69,  69);
unexpected_stub!(unexpected_vec70,  70);  unexpected_stub!(unexpected_vec71,  71);
unexpected_stub!(unexpected_vec72,  72);  unexpected_stub!(unexpected_vec73,  73);
unexpected_stub!(unexpected_vec74,  74);  unexpected_stub!(unexpected_vec75,  75);
unexpected_stub!(unexpected_vec76,  76);  unexpected_stub!(unexpected_vec77,  77);
unexpected_stub!(unexpected_vec78,  78);  unexpected_stub!(unexpected_vec79,  79);
unexpected_stub!(unexpected_vec80,  80);  unexpected_stub!(unexpected_vec81,  81);
unexpected_stub!(unexpected_vec82,  82);  unexpected_stub!(unexpected_vec83,  83);
unexpected_stub!(unexpected_vec84,  84);  unexpected_stub!(unexpected_vec85,  85);
unexpected_stub!(unexpected_vec86,  86);  unexpected_stub!(unexpected_vec87,  87);
unexpected_stub!(unexpected_vec88,  88);  unexpected_stub!(unexpected_vec89,  89);
unexpected_stub!(unexpected_vec90,  90);  unexpected_stub!(unexpected_vec91,  91);
unexpected_stub!(unexpected_vec92,  92);  unexpected_stub!(unexpected_vec93,  93);
unexpected_stub!(unexpected_vec94,  94);  unexpected_stub!(unexpected_vec95,  95);
unexpected_stub!(unexpected_vec96,  96);  unexpected_stub!(unexpected_vec97,  97);
unexpected_stub!(unexpected_vec98,  98);  unexpected_stub!(unexpected_vec99,  99);
unexpected_stub!(unexpected_vec100,100);  unexpected_stub!(unexpected_vec101,101);
unexpected_stub!(unexpected_vec102,102);  unexpected_stub!(unexpected_vec103,103);
unexpected_stub!(unexpected_vec104,104);  unexpected_stub!(unexpected_vec105,105);
unexpected_stub!(unexpected_vec106,106);  unexpected_stub!(unexpected_vec107,107);
unexpected_stub!(unexpected_vec108,108);  unexpected_stub!(unexpected_vec109,109);
unexpected_stub!(unexpected_vec110,110);  unexpected_stub!(unexpected_vec111,111);
unexpected_stub!(unexpected_vec112,112);  unexpected_stub!(unexpected_vec113,113);
unexpected_stub!(unexpected_vec114,114);  unexpected_stub!(unexpected_vec115,115);
unexpected_stub!(unexpected_vec116,116);  unexpected_stub!(unexpected_vec117,117);
unexpected_stub!(unexpected_vec118,118);  unexpected_stub!(unexpected_vec119,119);
unexpected_stub!(unexpected_vec120,120);  unexpected_stub!(unexpected_vec121,121);
unexpected_stub!(unexpected_vec122,122);  unexpected_stub!(unexpected_vec123,123);
unexpected_stub!(unexpected_vec124,124);  unexpected_stub!(unexpected_vec125,125);
unexpected_stub!(unexpected_vec126,126);  unexpected_stub!(unexpected_vec127,127);
unexpected_stub!(unexpected_vec128,128);  unexpected_stub!(unexpected_vec129,129);
unexpected_stub!(unexpected_vec130,130);  unexpected_stub!(unexpected_vec131,131);
unexpected_stub!(unexpected_vec132,132);  unexpected_stub!(unexpected_vec133,133);
unexpected_stub!(unexpected_vec134,134);  unexpected_stub!(unexpected_vec135,135);
unexpected_stub!(unexpected_vec136,136);  unexpected_stub!(unexpected_vec137,137);
unexpected_stub!(unexpected_vec138,138);  unexpected_stub!(unexpected_vec139,139);
unexpected_stub!(unexpected_vec140,140);  unexpected_stub!(unexpected_vec141,141);
unexpected_stub!(unexpected_vec142,142);  unexpected_stub!(unexpected_vec143,143);
unexpected_stub!(unexpected_vec144,144);  unexpected_stub!(unexpected_vec145,145);
unexpected_stub!(unexpected_vec146,146);  unexpected_stub!(unexpected_vec147,147);
unexpected_stub!(unexpected_vec148,148);  unexpected_stub!(unexpected_vec149,149);
unexpected_stub!(unexpected_vec150,150);  unexpected_stub!(unexpected_vec151,151);
unexpected_stub!(unexpected_vec152,152);  unexpected_stub!(unexpected_vec153,153);
unexpected_stub!(unexpected_vec154,154);  unexpected_stub!(unexpected_vec155,155);
unexpected_stub!(unexpected_vec156,156);  unexpected_stub!(unexpected_vec157,157);
unexpected_stub!(unexpected_vec158,158);  unexpected_stub!(unexpected_vec159,159);
unexpected_stub!(unexpected_vec160,160);  unexpected_stub!(unexpected_vec161,161);
unexpected_stub!(unexpected_vec162,162);  unexpected_stub!(unexpected_vec163,163);
unexpected_stub!(unexpected_vec164,164);  unexpected_stub!(unexpected_vec165,165);
unexpected_stub!(unexpected_vec166,166);  unexpected_stub!(unexpected_vec167,167);
unexpected_stub!(unexpected_vec168,168);  unexpected_stub!(unexpected_vec169,169);
unexpected_stub!(unexpected_vec170,170);  unexpected_stub!(unexpected_vec171,171);
unexpected_stub!(unexpected_vec172,172);  unexpected_stub!(unexpected_vec173,173);
unexpected_stub!(unexpected_vec174,174);  unexpected_stub!(unexpected_vec175,175);
unexpected_stub!(unexpected_vec176,176);  unexpected_stub!(unexpected_vec177,177);
unexpected_stub!(unexpected_vec178,178);  unexpected_stub!(unexpected_vec179,179);
unexpected_stub!(unexpected_vec180,180);  unexpected_stub!(unexpected_vec181,181);
unexpected_stub!(unexpected_vec182,182);  unexpected_stub!(unexpected_vec183,183);
unexpected_stub!(unexpected_vec184,184);  unexpected_stub!(unexpected_vec185,185);
unexpected_stub!(unexpected_vec186,186);  unexpected_stub!(unexpected_vec187,187);
unexpected_stub!(unexpected_vec188,188);  unexpected_stub!(unexpected_vec189,189);
unexpected_stub!(unexpected_vec190,190);  unexpected_stub!(unexpected_vec191,191);
unexpected_stub!(unexpected_vec192,192);  unexpected_stub!(unexpected_vec193,193);
unexpected_stub!(unexpected_vec194,194);  unexpected_stub!(unexpected_vec195,195);
unexpected_stub!(unexpected_vec196,196);  unexpected_stub!(unexpected_vec197,197);
unexpected_stub!(unexpected_vec198,198);  unexpected_stub!(unexpected_vec199,199);
unexpected_stub!(unexpected_vec200,200);  unexpected_stub!(unexpected_vec201,201);
unexpected_stub!(unexpected_vec202,202);  unexpected_stub!(unexpected_vec203,203);
unexpected_stub!(unexpected_vec204,204);  unexpected_stub!(unexpected_vec205,205);
unexpected_stub!(unexpected_vec206,206);  unexpected_stub!(unexpected_vec207,207);
unexpected_stub!(unexpected_vec208,208);  unexpected_stub!(unexpected_vec209,209);
unexpected_stub!(unexpected_vec210,210);  unexpected_stub!(unexpected_vec211,211);
unexpected_stub!(unexpected_vec212,212);  unexpected_stub!(unexpected_vec213,213);
unexpected_stub!(unexpected_vec214,214);  unexpected_stub!(unexpected_vec215,215);
unexpected_stub!(unexpected_vec216,216);  unexpected_stub!(unexpected_vec217,217);
unexpected_stub!(unexpected_vec218,218);  unexpected_stub!(unexpected_vec219,219);
unexpected_stub!(unexpected_vec220,220);  unexpected_stub!(unexpected_vec221,221);
unexpected_stub!(unexpected_vec222,222);  unexpected_stub!(unexpected_vec223,223);
unexpected_stub!(unexpected_vec224,224);  unexpected_stub!(unexpected_vec225,225);
unexpected_stub!(unexpected_vec226,226);  unexpected_stub!(unexpected_vec227,227);
unexpected_stub!(unexpected_vec228,228);  unexpected_stub!(unexpected_vec229,229);
unexpected_stub!(unexpected_vec230,230);  unexpected_stub!(unexpected_vec231,231);
unexpected_stub!(unexpected_vec232,232);  unexpected_stub!(unexpected_vec233,233);
unexpected_stub!(unexpected_vec234,234);  unexpected_stub!(unexpected_vec235,235);
unexpected_stub!(unexpected_vec236,236);  unexpected_stub!(unexpected_vec237,237);
unexpected_stub!(unexpected_vec238,238);  unexpected_stub!(unexpected_vec239,239);
unexpected_stub!(unexpected_vec240,240);  unexpected_stub!(unexpected_vec241,241);
unexpected_stub!(unexpected_vec242,242);  unexpected_stub!(unexpected_vec243,243);
unexpected_stub!(unexpected_vec244,244);  unexpected_stub!(unexpected_vec245,245);
unexpected_stub!(unexpected_vec246,246);  unexpected_stub!(unexpected_vec247,247);
unexpected_stub!(unexpected_vec248,248);  unexpected_stub!(unexpected_vec249,249);
unexpected_stub!(unexpected_vec250,250);  unexpected_stub!(unexpected_vec251,251);
unexpected_stub!(unexpected_vec252,252);  unexpected_stub!(unexpected_vec253,253);
unexpected_stub!(unexpected_vec254,254);

// ── COM1 UART RX ISR (IRQ4, vector 36) ───────────────────────────────────────
//
// Drain all received bytes from the FIFO and deliver each one to the
// uart-drv process via IPC.  Follows the same naked-stub + inner-function
// pattern as the timer ISR so that a context switch to uart-drv can happen
// immediately (before the next 10 ms timer tick).
//
// EOI order: drain FIFO first (clears the UART interrupt), then send EOI to
// the master PIC so no further IRQ4 is lost during processing.

/// Inner handler: drains RX FIFO and enqueues one IPC message per byte.
extern "C" fn handle_uart_rx() {
    if !hal::uart::rx_irq_pending() {
        return; // Spurious; nothing to do.
    }

    let drv_pid = core_kernel::uart_irq::get_uart_drv_pid();
    if drv_pid == u32::MAX {
        // uart-drv not yet registered — discard bytes to clear the interrupt.
        hal::uart::drain_rx_fifo(|_| {});
        return;
    }

    if let Some(sched) = core_kernel::scheduler::get_global() {
        let from = core_kernel::process::ProcessId::new(0); // kernel pseudo-PID
        let to   = core_kernel::process::ProcessId::new(drv_pid);

        hal::uart::drain_rx_fifo(|byte| {
            let mut msg = core_kernel::ipc::Message::new(from);
            msg.set_data(0, 0x02); // OP_UART_RX
            msg.set_data(1, byte as u64);
            sched.send_message(from, to, msg);
        });
    }
}

/// Naked ISR stub for vector 36 (IRQ4 — COM1).
///
/// Saves caller-saved GPRs, drains the FIFO, sends EOI, then calls
/// `uart_rx_isr()` which may context-switch to uart-drv immediately.
#[unsafe(naked)]
pub unsafe extern "C" fn uart_rx_handler() {
    core::arch::naked_asm!(
        // ── 1. Save caller-saved registers ──────────────────────────────────
        "push rax", "push rcx", "push rdx",
        "push rdi", "push rsi",
        "push r8",  "push r9", "push r10", "push r11",

        // ── 2. Drain FIFO and deliver bytes to uart-drv via IPC ─────────────
        "sub  rsp, 8",
        "call {deliver}",
        "add  rsp, 8",

        // ── 3a. EOI to master PIC (IRQ4 is on master, no slave EOI needed) ──
        "mov  al, 0x20",
        "out  0x20, al",

        // ── 3b. EOI to LAPIC (if initialised) ────────────────────────────────
        "mov  rax, qword ptr [{eoi_addr}]",
        "test rax, rax",
        "jz   2f",
        "mov  dword ptr [rax], 0",
        "2:",

        // ── 4. Reschedule if uart-drv has higher priority than current ───────
        "sub  rsp, 8",
        "call {sched}",
        "add  rsp, 8",

        // ── 5+6. Restore and IRET (may now be a different process) ───────────
        "pop  r11", "pop  r10", "pop  r9", "pop  r8",
        "pop  rsi", "pop  rdi",
        "pop  rdx", "pop  rcx", "pop  rax",
        "iretq",

        deliver  = sym handle_uart_rx,
        eoi_addr = sym LAPIC_EOI_ADDR,
        sched    = sym crate::cpu::uart_rx_isr,
    );
}

// ── Timer ISR (IRQ0, vector 32) ───────────────────────────────────────────────
//
// Full preemptive context switch:
//   1. Save all caller-saved registers onto the interrupted stack.
//   2. Atomically increment TICK_COUNT.
//   3. Send EOI to master PIC.
//   4. Call tick_scheduler_isr():
//      - Advances the scheduler tick counter.
//      - If the current process's quantum has expired, calls switch_context_noints
//        (which saves callee-saved + RSP into old context, restores from new context,
//         and `ret`s into the new process's ISR frame — interrupts stay disabled).
//      - If no switch is needed, returns immediately.
//   5. Restore caller-saved registers from the current stack (which may now belong
//      to a different process if a switch occurred in step 4).
//   6. IRETQ — restores RIP/CS/RFLAGS (and RSP/SS on ring-3 entry) for the task
//      that will resume; RFLAGS restoration re-enables interrupts (IF was 1 for
//      any user-mode task, or 0 for a kernel task that HAD cleared them).
//
// Stack alignment: IRETQ frame (24 B ring-0 / 40 B ring-3) + 9 × 8 B caller-saved
// = 96 / 112 bytes below the interrupted RSP.  The `sub rsp,8` before the `call`
// aligns RSP to 16 bytes as required by the System V AMD64 ABI.
#[unsafe(naked)]
pub unsafe extern "C" fn timer_interrupt_handler() {
    core::arch::naked_asm!(
        // ── 1. Save all caller-saved registers ──────────────────────────────
        "push rax", "push rcx", "push rdx",
        "push rdi", "push rsi",
        "push r8",  "push r9", "push r10", "push r11",

        // ── 2. Atomically increment the global tick counter ─────────────────
        "lock inc qword ptr [{tick}]",

        // ── 3a. EOI to master PIC ───────────────────────────────────────────
        "mov  al, 0x20",
        "out  0x20, al",

        // ── 3b. EOI to LAPIC (if initialised) ──────────────────────────────
        // LAPIC_EOI_ADDR is 0 when running in PIC-only mode; skip the write.
        "mov  rax, qword ptr [{eoi_addr}]",
        "test rax, rax",
        "jz   2f",
        "mov  dword ptr [rax], 0",
        "2:",

        // ── 4. Run the scheduler; may switch stacks (interrupts stay off) ───
        "sub  rsp, 8",                  // 16-byte align for call
        "call {sched}",
        "add  rsp, 8",

        // ── 5 + 6. Restore registers from whoever is now running, then IRET ─
        "pop  r11", "pop  r10", "pop  r9", "pop  r8",
        "pop  rsi", "pop  rdi",
        "pop  rdx", "pop  rcx", "pop  rax",
        "iretq",

        tick     = sym TICK_COUNT,
        eoi_addr = sym LAPIC_EOI_ADDR,
        sched    = sym crate::cpu::tick_scheduler_isr,
    );
}

/// Interrupt latency measurement — max ISR-entry-to-exit ticks seen so far.
pub static MAX_ISR_LATENCY: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
