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
/// | 3 | sys_send     | IPC send 2 words (rdi=to_pid, rsi=w0, rdx=w1) |
/// | 4 | sys_recv     | IPC blocking receive word0 (rdi=timeout_ticks) |
/// | 5 | sys_notify   | Send notification word (rdi=to_pid, rsi=word) |
/// | 6 | sys_recv_msg | Receive full Message → user buffer (rdi=timeout, rsi=ptr) |
/// | 7 | sys_send_msg | Send full Message from user buffer (rdi=to_pid, rsi=ptr) |
use super::{rdmsr, wrmsr};

// MSR addresses
const MSR_EFER:   u32 = 0xC000_0080;
const MSR_STAR:   u32 = 0xC000_0081;
const MSR_LSTAR:  u32 = 0xC000_0082;
const MSR_SFMASK: u32 = 0xC000_0084;

// Syscall numbers
const SYS_YIELD:      u64 =  0;
const SYS_EXIT:       u64 =  1;
const SYS_GETPID:     u64 =  2;
const SYS_SEND:       u64 =  3;
const SYS_RECV:       u64 =  4;
const SYS_NOTIFY:     u64 =  5;
const SYS_RECV_MSG:   u64 =  6;
const SYS_SEND_MSG:   u64 =  7;
const SYS_SPAWN:      u64 =  8; // spawn a new ring-0 process
const SYS_MAP:        u64 =  9; // map virtual page in current process
const SYS_REGISTER:   u64 = 10; // register service name → current PID
const SYS_LOOKUP:     u64 = 11; // look up PID for service name

// Error codes
const ENOSYS:  u64 = u64::MAX;        // -1: function not implemented
const EINVAL:  u64 = u64::MAX - 1;    // -2: invalid argument
pub const EPERM: u64 = u64::MAX - 2;  // -3: operation not permitted
const ENOMEM:  u64 = u64::MAX - 3;    // -4: out of physical memory

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
    // a0=rdi, a1=rsi, a2=rdx, _a3=r10(→rcx), _a4=r8, _a5=r9
    use core::sync::atomic::Ordering;

    match number {
        SYS_YIELD => {
            // Exhaust the current quantum so the next timer tick forces a switch.
            // The actual switch is deferred to the next timer interrupt (or the
            // next cooperative tick_scheduler() call in the idle loop).
            if let Some(sched) = core_kernel::scheduler::get_global() {
                sched.yield_current();
            }
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

        // SYS_RECV_MSG — receive one full Message into a user-provided buffer.
        //
        // a0 = timeout_ticks
        // a1 = pointer to a 72-byte user buffer laid out as:
        //        offset 0: sender u32
        //        offset 4: _pad   u32
        //        offset 8: data   [u64; 8]
        //
        // Returns 0 on success, u64::MAX on timeout / no message.
        //
        // Security: the kernel overwrites msg.sender with the real sender PID
        // before writing to the buffer, so the user cannot forge sender identity.
        // Safety: pointer is trusted (ring-3 not yet isolated; identity-mapped).
        SYS_RECV_MSG => {
            if let Some(sched) = core_kernel::scheduler::get_global() {
                let pid = core_kernel::process::ProcessId::new(
                    core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
                match sched.blocking_receive(pid, a0) {
                    Some(msg) => {
                        if a1 != 0 {
                            unsafe {
                                core::ptr::write(
                                    a1 as *mut core_kernel::ipc::Message,
                                    msg,
                                );
                            }
                        }
                        0
                    }
                    None => u64::MAX,
                }
            } else {
                ENOSYS
            }
        }

        // SYS_SEND_MSG — send a full Message from a user-provided buffer.
        //
        // a0 = target PID
        // a1 = pointer to a 72-byte user Msg buffer (same layout as SYS_RECV_MSG)
        //
        // The kernel overwrites msg.sender with CURRENT_PID before enqueuing,
        // so the sender field set by user space is always ignored.
        //
        // Returns 0 on success, EINVAL if queue full or bad PID.
        SYS_SEND_MSG => {
            if let Some(sched) = core_kernel::scheduler::get_global() {
                let from_pid = core_kernel::process::ProcessId::new(
                    core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
                let to_pid = core_kernel::process::ProcessId::new(a0 as u32);
                let mut msg = if a1 != 0 {
                    unsafe { core::ptr::read(a1 as *const core_kernel::ipc::Message) }
                } else {
                    core_kernel::ipc::Message::new(from_pid)
                };
                msg.sender = from_pid; // always overwrite — user cannot forge
                if sched.send_message(from_pid, to_pid, msg) { 0 } else { EINVAL }
            } else {
                ENOSYS
            }
        }

        // SYS_SPAWN — create a new ring-0 kernel process.
        //
        // a0 = entry_point (virtual address; must be a valid ring-0 function)
        // a1 = page_table_base (physical address of PML4; 0 = inherit kernel PML4)
        // a2 = priority (0–255; 0 = use default 128)
        //
        // Returns the new PID on success, EINVAL if the process table is full.
        SYS_SPAWN => {
            if let Some(sched) = core_kernel::scheduler::get_global() {
                let pml4 = if a1 != 0 {
                    a1
                } else {
                    core_kernel::scheduler::KERNEL_PML4_PHYS
                        .load(Ordering::Relaxed)
                };
                match sched.add_process(a0, 0, pml4) {
                    Some(pid) => {
                        if a2 > 0 && a2 <= 255 {
                            sched.set_priority(pid, a2 as u8);
                        }
                        pid.as_u32() as u64
                    }
                    None => EINVAL,
                }
            } else {
                ENOSYS
            }
        }

        // SYS_MAP — map a 4 KB virtual page in the current process's address space.
        //
        // a0 = virtual address  (4 KB-aligned)
        // a1 = physical address (4 KB-aligned; 0 = allocate a new physical page)
        // a2 = flags            (bit 0 = writable, bit 1 = user-mode accessible)
        //
        // Returns 0 on success, ENOMEM if physical memory is exhausted, EINVAL
        // for a misaligned or otherwise bad address.
        SYS_MAP => {
            use core_kernel::memory::{PTE_PRESENT, PTE_WRITABLE, PTE_USER,
                                      map_page_global, global_alloc_4k};

            if a0 & 0xFFF != 0 { return EINVAL; }
            if a1 != 0 && a1 & 0xFFF != 0 { return EINVAL; }

            let phys = if a1 != 0 {
                a1
            } else {
                match global_alloc_4k() {
                    Some(p) => p,
                    None    => return ENOMEM,
                }
            };

            // Find the calling process's PML4.
            let pml4_phys = if let Some(sched) = core_kernel::scheduler::get_global() {
                let pid = core_kernel::process::ProcessId::new(
                    core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
                sched.get_process_pml4(pid)
                    .unwrap_or_else(|| core_kernel::scheduler::KERNEL_PML4_PHYS
                        .load(Ordering::Relaxed))
            } else {
                return ENOSYS;
            };

            // For identity-mapped kernel, pml4_phys == pml4_virt.
            let pml4 = unsafe { &mut *(pml4_phys as *mut core_kernel::memory::PageTable) };

            let mut flags = PTE_PRESENT;
            if a2 & 1 != 0 { flags |= PTE_WRITABLE; }
            if a2 & 2 != 0 { flags |= PTE_USER; }

            if map_page_global(pml4, a0, phys, flags) { 0 } else { ENOMEM }
        }

        // SYS_REGISTER — register the current process as a named service.
        //
        // a0 = pointer to null-terminated ASCII service name (≤ 15 bytes)
        //
        // Returns 0 on success, EINVAL if the table is full.
        SYS_REGISTER => {
            if a0 == 0 { return EINVAL; }
            let pid = core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed);
            // Read up to NAME_LEN bytes from user memory (identity-mapped → safe).
            let name = unsafe { core::slice::from_raw_parts(a0 as *const u8, 16) };
            if core_kernel::service_registry::register(name, pid) { 0 } else { EINVAL }
        }

        // SYS_LOOKUP — look up the PID for a named service.
        //
        // a0 = pointer to null-terminated ASCII service name
        //
        // Returns the PID on success, EINVAL if not found.
        SYS_LOOKUP => {
            if a0 == 0 { return EINVAL; }
            let name = unsafe { core::slice::from_raw_parts(a0 as *const u8, 16) };
            match core_kernel::service_registry::lookup(name) {
                Some(pid) => pid as u64,
                None      => EINVAL,
            }
        }

        _ => ENOSYS,
    }
}
