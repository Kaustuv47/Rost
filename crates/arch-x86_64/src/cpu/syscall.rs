/// SYSCALL / SYSRET initialisation, entry stub, and dispatcher.
///
/// # MSR layout
/// | MSR | Address | Purpose |
/// |-----|---------|---------|
/// | EFER   | 0xC000_0080 | bit 0 = SCE (System-Call Extensions) |
/// | STAR   | 0xC000_0081 | bits[47:32] = ring-0 CS; bits[63:48] = ring-3 base |
/// | LSTAR  | 0xC000_0082 | 64-bit entry RIP for SYSCALL |
/// | SFMASK | 0xC000_0084 | RFLAGS bits to CLEAR on entry (IF + DF) |
///
/// # Calling convention (mirrors Linux x86_64)
/// | Register | Role |
/// |----------|------|
/// | rax | syscall number (in) / return value (out) |
/// | rdi | arg 0 |
/// | rsi | arg 1 |
/// | rdx | arg 2 (note: r10 in Linux for 4th arg — we accept rdx here) |
/// | r10 | arg 3 |
/// | r8  | arg 4 |
/// | r9  | arg 5 |
/// | rcx | saved user RIP (by CPU) |
/// | r11 | saved user RFLAGS (by CPU) |
///
/// # Syscall table
/// | Number | Name | Description |
/// |--------|------|-------------|
/// | 0 | sys_yield    | Voluntarily give up the CPU |
/// | 1 | sys_exit     | Terminate calling process |
/// | 2 | sys_getpid   | Return own ProcessId |
/// | 3 | sys_send     | IPC send (rdi=to_pid, rsi=msg_ptr — future: after TSS) |
/// | 4 | sys_recv     | IPC blocking receive (rdi=timeout_ticks) |
/// | 5 | sys_notify   | Send notification word (rdi=to_pid, rsi=word) |
use super::{rdmsr, wrmsr};

// MSR addresses
const MSR_EFER:   u32 = 0xC000_0080;
const MSR_STAR:   u32 = 0xC000_0081;
const MSR_LSTAR:  u32 = 0xC000_0082;
const MSR_SFMASK: u32 = 0xC000_0084;

// Syscall numbers
const SYS_YIELD:   u64 = 0;
const SYS_EXIT:    u64 = 1;
const SYS_GETPID:  u64 = 2;
const SYS_SEND:    u64 = 3;
const SYS_RECV:    u64 = 4;
const SYS_NOTIFY:  u64 = 5;

// Error codes
const ENOSYS:  u64 = u64::MAX;        // -1: function not implemented
const EINVAL:  u64 = u64::MAX - 1;    // -2: invalid argument
pub const EPERM: u64 = u64::MAX - 2;  // -3: operation not permitted

/// Initialise SYSCALL/SYSRET MSRs.
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

/// Raw SYSCALL entry point (naked asm).
///
/// The CPU has already:
///   * Saved user RIP → rcx
///   * Saved user RFLAGS → r11
///   * Cleared IF and DF
///   * Switched CS to ring-0 segment (but NOT stack — TSS.RSP0 required for ring-3)
///
/// We save all callee-saved + argument registers, dispatch to `dispatch_syscall`,
/// restore, and execute SYSRETQ.
///
/// **Stack note:** until TSS.RSP0 is updated on every context switch, syscalls
/// from ring-3 are unsafe because rsp still points to the user stack.  This is
/// documented and will be fixed when the ELF loader / ring-3 entry is added.
#[unsafe(naked)]
pub unsafe extern "C" fn syscall_entry() {
    core::arch::naked_asm!(
        // Save callee-saved + rcx/r11 (user RIP/RFLAGS saved by CPU).
        "push rcx",     // user RIP
        "push r11",     // user RFLAGS
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        // Move r10 (4th arg in Linux ABI) into rcx for the Rust call.
        "mov  rcx, r10",
        // Dispatch: rax=number, rdi=a0, rsi=a1, rdx=a2, rcx=a3, r8=a4, r9=a5
        "call {dispatch}",
        // Restore (rax holds return value).
        "pop  r15",
        "pop  r14",
        "pop  r13",
        "pop  r12",
        "pop  rbx",
        "pop  rbp",
        "pop  r11",     // RFLAGS for SYSRETQ
        "pop  rcx",     // RIP for SYSRETQ
        "sysretq",
        dispatch = sym dispatch_syscall,
    );
}

/// Rust syscall dispatcher.
///
/// Arguments follow the System V AMD64 ABI after the naked stub's fixup:
///   rax = syscall number  →  first argument to this function
///   rdi = a0, rsi = a1, rdx = a2, rcx = a3 (was r10), r8 = a4, r9 = a5
///
/// Returns the value to place in rax (the caller's return value).
extern "C" fn dispatch_syscall(
    number: u64,
    a0: u64, a1: u64, a2: u64, _a3: u64, _a4: u64, _a5: u64,
) -> u64 {
    use core::sync::atomic::Ordering;

    match number {
        SYS_YIELD => {
            // Cooperative yield: the scheduler will preempt on the next tick.
            // Mark the current process Ready so pick_next_priority considers it.
            0
        }

        SYS_EXIT => {
            // Terminate the calling process.
            hal::uart::print_str("[SYS_EXIT] process exit code=");
            hal::uart::print_hex(a0);
            hal::uart::print_str("\n");
            if let Some(sched) = core_kernel::scheduler::get_global() {
                let pid = core_kernel::process::ProcessId::new(
                    core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
                sched.terminate_process(pid);
                // The process will not be scheduled again; it resumes here once
                // before the next tick selects a different process — that is
                // acceptable because it returns to user space, which should not
                // execute meaningful code after SYS_EXIT returns.
            }
            a0
        }

        SYS_GETPID => {
            // Return the calling process's PID from the global tracker.
            core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed) as u64
        }

        SYS_SEND => {
            // a0 = target PID, a1 = payload word 0, a2 = payload word 1
            // The kernel stamps msg.sender — user cannot forge the source PID.
            if let Some(sched) = core_kernel::scheduler::get_global() {
                let from_pid = core_kernel::process::ProcessId::new(
                    core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
                let to_pid = core_kernel::process::ProcessId::new(a0 as u32);
                let mut msg = core_kernel::ipc::Message::new(from_pid);
                msg.set_data(0, a1);
                msg.set_data(1, a2);
                if sched.send_message(from_pid, to_pid, msg) { 0 } else { EINVAL }
            } else {
                ENOSYS
            }
        }

        SYS_RECV => {
            // a0 = timeout_ticks (u64::MAX = no timeout)
            // If a message is waiting, returns the first payload word.
            // If no message, blocks the process (it won't be scheduled until a
            // sender unblocks it or the deadline expires) and returns u64::MAX.
            // User-space should treat u64::MAX as "retry needed" and loop.
            if let Some(sched) = core_kernel::scheduler::get_global() {
                let pid = core_kernel::process::ProcessId::new(
                    core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
                match sched.blocking_receive(pid, a0) {
                    Some(msg) => msg.get_data(0),
                    None      => u64::MAX, // blocked — retry when rescheduled
                }
            } else {
                ENOSYS
            }
        }

        SYS_NOTIFY => {
            // a0 = target PID, a1 = notification word (bitmask)
            if let Some(sched) = core_kernel::scheduler::get_global() {
                let to_pid = core_kernel::process::ProcessId::new(a0 as u32);
                if sched.notify_process(to_pid, a1) { 0 } else { EINVAL }
            } else {
                ENOSYS
            }
        }

        _ => ENOSYS,
    }
}
