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
/// | 8  | sys_spawn   | Create ring-0 process (rdi=entry, rsi=pml4, rdx=priority) |
/// | 9  | sys_map     | Map 4 KB virtual page (rdi=vaddr, rsi=paddr, rdx=flags) |
/// | 10 | sys_register| Register service name for current PID |
/// | 11 | sys_lookup  | Lookup PID for service name |
/// | 12 | sys_uart_write | Write byte to COM1 |
/// | 13 | sys_uart_read  | Non-blocking read from COM1 |
/// | 14 | sys_clock   | Monotonic clock: nanoseconds since boot (100 Hz resolution) |
/// | 15 | sys_setprio | Set process priority (rdi=pid 0=self, rsi=0–255) |
/// | 16 | sys_setrt   | Set real-time period (rdi=pid 0=self, rsi=period_ticks 0=disable) |
/// | 17 | sys_call      | Synchronous call+reply: send msg, block until reply |
/// | 18 | sys_cap_grant | Grant capability from caller's table to another process |
/// | 19 | sys_setquota  | Set resource quotas (memory pages, CPU budget, IPC rate) |
/// | 20 | sys_chan_bind  | Create a Channel capability pointing to a target PID |
/// | 21 | sys_send_cap  | Send 72-byte message via a channel capability |
/// | 22 | sys_map_share  | Allocate shared 4 KB frame, map, return Memory cap slot |
/// | 23 | sys_map_cap    | Map a shared frame via a Memory capability |
/// | 24 | sys_lookup_cap  | Lookup service name → Channel capability slot (unforgeable) |
/// | 25 | sys_inject_fault | (fault-injection feature only) Trigger CPU exception for testing |
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
const SYS_UART_WRITE: u64 = 12; // write one byte to COM1 (driver use only)
const SYS_UART_READ:  u64 = 13; // non-blocking read from COM1; u64::MAX if empty
const SYS_CLOCK:      u64 = 14; // monotonic nanoseconds since boot (100 Hz resolution)
const SYS_SETPRIO:    u64 = 15; // set process scheduling priority (0=self, 0–255)
const SYS_SETRT:      u64 = 16; // set real-time period (0=self, period_ticks; 0=disable)
const SYS_CALL:       u64 = 17; // synchronous call: send msg, block until reply
const SYS_CAP_GRANT:  u64 = 18; // grant capability slot to another process
const SYS_SETQUOTA:   u64 = 19; // set resource quotas for a process
const SYS_CHAN_BIND:   u64 = 20; // create a Channel capability pointing to a target PID
const SYS_SEND_CAP:    u64 = 21; // send 72-byte message via channel capability
const SYS_MAP_SHARE:   u64 = 22; // allocate shared 4 KB frame + map + return Memory cap slot
const SYS_MAP_CAP:     u64 = 23; // map a shared frame into caller's VAS via Memory capability
const SYS_LOOKUP_CAP:  u64 = 24; // lookup service name → Channel capability slot index
#[cfg(feature = "fault-injection")]
const SYS_INJECT_FAULT: u64 = 25; // trigger CPU exception for handler-path testing

// Error codes
const ENOSYS:    u64 = u64::MAX;      // -1: function not implemented
const EINVAL:    u64 = u64::MAX - 1;  // -2: invalid argument
pub const EPERM: u64 = u64::MAX - 2;  // -3: operation not permitted
const ENOMEM:    u64 = u64::MAX - 3;  // -4: out of physical memory
const EAGAIN:    u64 = u64::MAX - 4;  // -5: resource temporarily unavailable
const ETIMEDOUT: u64 = u64::MAX - 5;  // -6: operation timed out
const ENOENT:    u64 = u64::MAX - 6;  // -7: no such entry (service not registered)

// ── Syscall argument validation ───────────────────────────────────────────────

/// First virtual address above the canonical user-space region on x86-64
/// with 4-level paging (48-bit virtual address space, lower half).
///
/// Addresses in `[0, USER_VA_END)` are valid user-space virtual addresses.
/// Addresses at or above this value are non-canonical or kernel-space.
/// The kernel is identity-mapped at physical addresses ~16–48 MB which are
/// far below this boundary; this constant is the correct security demarcation.
const USER_VA_END: u64 = 0x0000_8000_0000_0000;

/// Validate a user-supplied pointer for safety and security.
///
/// Returns `true` iff **all** of the following hold:
///
/// 1. `ptr` is non-zero (null pointer is never valid).
/// 2. `ptr` is `align`-byte aligned (`align` must be a power of two).
/// 3. `ptr + size` does not wrap around (no overflow).
/// 4. The entire range `[ptr, ptr + size)` lies strictly within the
///    canonical user-space virtual address region (`< USER_VA_END`),
///    preventing the kernel from being steered at its own data structures
///    via a crafted user argument.
///
/// This is a **necessary but not sufficient** precondition: the range must
/// also be present in the calling process's page tables.  Full page-table
/// walk verification is deferred; CR4.SMAP provides a hardware backstop for
/// any unmapped or kernel-owned page that slips through.
///
/// # SIL-4 rationale
/// IEC 61508 §7.4.2 requires that every external input (here: syscall
/// arguments from ring-3) is validated before use.  A single unvalidated
/// pointer passed to `core::ptr::write` can corrupt any kernel data
/// structure, violating spatial isolation — the core safety property of a
/// microkernel.  Explicit validation is required even with SMAP active
/// because SMAP is a defence-in-depth measure, not a substitute for
/// software input validation.
#[inline]
fn validate_user_ptr(ptr: u64, size: usize, align: usize) -> bool {
    debug_assert!(align.is_power_of_two(), "align must be a power of two");
    // 1. Non-null
    if ptr == 0 { return false; }
    // 2. Alignment
    if ptr & (align as u64 - 1) != 0 { return false; }
    // 3. No overflow on end address
    let end = match ptr.checked_add(size as u64) {
        Some(e) => e,
        None    => return false,
    };
    // 4. Entirely within canonical user space
    end <= USER_VA_END
}

/// Validate a user virtual address for `SYS_MAP`.
///
/// The address must be:
/// - 4 KB-aligned (page boundary)
/// - Within canonical user space (not pointing into kernel virtual range)
/// - Non-zero
///
/// This prevents a process from mapping a page over the kernel's own virtual
/// address space (identity-mapped), which could enable privilege escalation.
#[inline]
fn validate_user_vaddr(vaddr: u64) -> bool {
    vaddr != 0 && vaddr & 0xFFF == 0 && vaddr < USER_VA_END
}

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
///   * Cleared IF and DF (SFMASK)
///   * Switched CS to ring-0 segment; RSP still points to the user stack
///
/// We immediately switch to the per-process kernel stack (SMAP-safe: the save
/// and load both touch non-PTE_USER kernel statics, not user pages).  All
/// callee-saved + rcx/r11 are pushed onto the kernel stack, then we dispatch
/// to `dispatch_syscall` and restore before SYSRETQ.
///
/// SMAP safety:
///   * The first two instructions access only kernel statics (no PTE_USER) so
///     SMAP/AC=0 is not a problem.
///   * All push/pop operations use the kernel stack (non-PTE_USER).
///   * `dispatch_syscall` sets RFLAGS.AC (STAC) for any user-memory accesses it
///     needs to perform (e.g. SYS_RECV_MSG buffer writes).
#[unsafe(naked)]
pub unsafe extern "C" fn syscall_entry() {
    core::arch::naked_asm!(
        // ── Switch from user stack to kernel stack ────────────────────────────
        // RSP = user RSP at this point (SYSCALL does not switch stacks).
        // Save user RSP to kernel static (non-PTE_USER: SMAP does not apply).
        "mov qword ptr [{user_rsp}], rsp",
        // Load the current process's kernel stack top (also kernel static).
        "mov rsp, qword ptr [{kern_rsp}]",

        // ── Push registers onto the kernel stack ──────────────────────────────
        "push rcx",     // user RIP  (saved by SYSCALL into rcx)
        "push r11",     // user RFLAGS (saved by SYSCALL into r11)
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        // Move r10 (4th syscall arg in Linux ABI) into rcx for the Rust call.
        // r10 is unchanged since entry (only user_rsp_save/rsp were modified).
        "mov  rcx, r10",
        // Dispatch: rax=number, rdi=a0, rsi=a1, rdx=a2, rcx=a3, r8=a4, r9=a5
        "call {dispatch}",

        // ── Restore registers from kernel stack ───────────────────────────────
        "pop  r15",
        "pop  r14",
        "pop  r13",
        "pop  r12",
        "pop  rbx",
        "pop  rbp",
        "pop  r11",     // RFLAGS for SYSRETQ
        "pop  rcx",     // RIP for SYSRETQ

        // ── Switch back to user stack and return to ring-3 ────────────────────
        "mov  rsp, qword ptr [{user_rsp}]",
        "sysretq",

        user_rsp  = sym super::tss::SYSCALL_USER_RSP_SAVE,
        kern_rsp  = sym super::tss::SYSCALL_KERN_RSP,
        dispatch  = sym dispatch_syscall,
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
    a0: u64, a1: u64, a2: u64, a3: u64, _a4: u64, _a5: u64,
) -> u64 {
    // a0=rdi, a1=rsi, a2=rdx, _a3=r10(→rcx), _a4=r8, _a5=r9
    use core::sync::atomic::Ordering;

    // Allow the kernel to access user-mode pages for the duration of this
    // syscall.  CR4.SMAP prevents supervisor access to PTE_USER pages unless
    // RFLAGS.AC is set.  STAC sets AC; the RAII guard clears it on drop.
    struct SmapGuard;
    impl Drop for SmapGuard {
        fn drop(&mut self) {
            unsafe { core::arch::asm!("clac", options(nostack, preserves_flags)); }
        }
    }
    unsafe { core::arch::asm!("stac", options(nostack, preserves_flags)); }
    let _smap_guard = SmapGuard;

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
        // a1 = pointer to a 72-byte user buffer (Message layout; 8-byte aligned)
        //
        // Returns 0 on success, u64::MAX on timeout / no message, EINVAL for
        // a null or out-of-range buffer pointer.
        //
        // Security: the kernel overwrites msg.sender with the real sender PID
        // before writing to the buffer, so the user cannot forge sender identity.
        SYS_RECV_MSG => {
            // Validate buffer pointer before touching it.
            // size_of::<Message> == 72, align_of == 8.
            const MSG_SIZE:  usize = core::mem::size_of::<core_kernel::ipc::Message>();
            const MSG_ALIGN: usize = core::mem::align_of::<core_kernel::ipc::Message>();
            if a1 == 0 || !validate_user_ptr(a1, MSG_SIZE, MSG_ALIGN) {
                return EINVAL;
            }
            if let Some(sched) = core_kernel::scheduler::get_global() {
                let pid = core_kernel::process::ProcessId::new(
                    core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
                match sched.blocking_receive(pid, a0) {
                    Some(msg) => {
                        unsafe {
                            core::ptr::write(a1 as *mut core_kernel::ipc::Message, msg);
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
        // a1 = pointer to a 72-byte user Msg buffer (8-byte aligned, non-null)
        //
        // The kernel overwrites msg.sender with CURRENT_PID before enqueuing,
        // so the sender field set by user space is always ignored.
        //
        // Returns 0 on success, EINVAL if buffer invalid, queue full, or bad PID.
        SYS_SEND_MSG => {
            // Buffer pointer is mandatory and must be valid user space.
            const MSG_SIZE:  usize = core::mem::size_of::<core_kernel::ipc::Message>();
            const MSG_ALIGN: usize = core::mem::align_of::<core_kernel::ipc::Message>();
            if !validate_user_ptr(a1, MSG_SIZE, MSG_ALIGN) {
                return EINVAL;
            }
            if let Some(sched) = core_kernel::scheduler::get_global() {
                let from_pid = core_kernel::process::ProcessId::new(
                    core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
                let to_pid = core_kernel::process::ProcessId::new(a0 as u32);
                let mut msg = unsafe { core::ptr::read(a1 as *const core_kernel::ipc::Message) };
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
        // a0 = virtual address  (4 KB-aligned, non-null, within user space)
        // a1 = physical address (4 KB-aligned; 0 = allocate a new physical page)
        // a2 = flags            (bit 0 = writable, bit 1 = user-mode accessible)
        //
        // Returns 0 on success, ENOMEM if physical memory is exhausted, EINVAL
        // for a misaligned, null, or out-of-user-space address.
        //
        // Security: a0 is validated against USER_VA_END so a process cannot map
        // a page over the kernel's own identity-mapped virtual addresses.
        SYS_MAP => {
            use core_kernel::memory::{PTE_PRESENT, PTE_WRITABLE, PTE_USER,
                                      map_page_global, global_alloc_4k,
                                      frame_tag, FrameKind};

            if !validate_user_vaddr(a0) { return EINVAL; }
            if a1 != 0 && a1 & 0xFFF != 0 { return EINVAL; }

            let phys = if a1 != 0 {
                a1
            } else {
                match global_alloc_4k() {
                    Some(p) => {
                        frame_tag(p, FrameKind::UserOwned);
                        p
                    }
                    None => return ENOMEM,
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
        // a0 = pointer to a ≤15-byte null-terminated ASCII service name
        //      (must be 16 bytes accessible, 1-byte aligned, within user space)
        //
        // Returns 0 on success, EINVAL if the pointer is invalid or table full.
        SYS_REGISTER => {
            // Service name buffer: 16 bytes, byte-aligned, user space only.
            if !validate_user_ptr(a0, 16, 1) { return EINVAL; }
            let pid = core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed);
            let name = unsafe { core::slice::from_raw_parts(a0 as *const u8, 16) };
            if core_kernel::service_registry::register(name, pid) { 0 } else { EINVAL }
        }

        // SYS_LOOKUP — look up the PID for a named service.
        //
        // a0 = pointer to a null-terminated ASCII service name
        //      (must be 16 bytes accessible, 1-byte aligned, within user space)
        //
        // Returns the PID on success, EINVAL if pointer invalid or name not found.
        SYS_LOOKUP => {
            if !validate_user_ptr(a0, 16, 1) { return EINVAL; }
            let name = unsafe { core::slice::from_raw_parts(a0 as *const u8, 16) };
            match core_kernel::service_registry::lookup(name) {
                Some(pid) => pid as u64,
                None      => EINVAL,
            }
        }

        // SYS_UART_WRITE — write one byte to COM1 on behalf of a driver process.
        //
        // a0 = byte value (low 8 bits used)
        //
        // Ring-3 processes have no IOPL, so all port I/O must go through here.
        // Returns 0.
        SYS_UART_WRITE => {
            hal::uart::put_byte(a0 as u8);
            0
        }

        // SYS_UART_READ — non-blocking read of one byte from COM1.
        //
        // Returns the byte value (0–255) if one is available in the RX buffer,
        // or u64::MAX if the FIFO is empty.
        SYS_UART_READ => {
            match hal::uart::read_byte() {
                Some(b) => b as u64,
                None    => u64::MAX,
            }
        }

        // SYS_CLOCK — monotonic nanoseconds since boot.
        //
        // Returns TICK_COUNT × 10,000,000 (100 Hz → 10 ms = 10,000,000 ns per tick).
        // Resolution is 10 ms; no argument required.
        //
        // IEC 61508 §7.4.1: temporal partitioning requires processes to know
        // elapsed time so they can detect deadline overruns.
        SYS_CLOCK => {
            let ticks = crate::interrupts::TICK_COUNT
                .load(core::sync::atomic::Ordering::Relaxed);
            ticks.saturating_mul(10_000_000)
        }

        // SYS_SETPRIO — change a process's scheduling priority at runtime.
        //
        // a0 = pid  (0 = calling process)
        // a1 = new priority (0 = highest urgency … 255 = lowest)
        //
        // Ring-3 processes may lower their own priority or change child processes
        // they spawned.  Privilege enforcement is deferred to the capability system.
        // Returns 0 on success, ENOSYS if no scheduler.
        SYS_SETPRIO => {
            let caller = core_kernel::process::ProcessId::new(
                core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
            let pid = if a0 == 0 { caller }
                      else        { core_kernel::process::ProcessId::new(a0 as u32) };
            let priority = (a1 & 0xFF) as u8;
            if let Some(sched) = core_kernel::scheduler::get_global() {
                sched.set_priority(pid, priority);
                0
            } else { ENOSYS }
        }

        // SYS_SETRT — assign or remove a real-time period (EDF scheduling).
        //
        // a0 = pid  (0 = calling process)
        // a1 = period_ticks  (0 = revert to best-effort priority scheduling)
        //
        // When period_ticks > 0 the process is promoted to the EDF tier and
        // will preempt all best-effort processes.  The initial deadline is set
        // to current_tick + period_ticks.  Returns 0 on success, ENOSYS if
        // no scheduler.
        SYS_SETRT => {
            let caller = core_kernel::process::ProcessId::new(
                core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
            let pid = if a0 == 0 { caller }
                      else        { core_kernel::process::ProcessId::new(a0 as u32) };
            if let Some(sched) = core_kernel::scheduler::get_global() {
                sched.set_realtime(pid, a1);
                0
            } else { ENOSYS }
        }

        // SYS_CALL — synchronous call/reply IPC.
        //
        // a0 = target PID
        // a1 = send buffer pointer  (72-byte Message, 8-byte aligned, user space)
        // a2 = reply buffer pointer (72-byte Message, 8-byte aligned, user space)
        // a3 = timeout_ticks  (0 = wait forever, otherwise tick deadline count)
        //
        // Atomically sends the message to `a0` and blocks the caller until either:
        //   - The target (or anyone) sends a reply to the caller's mailbox.
        //   - The optional timeout expires.
        //
        // Atomicity is guaranteed by the single-core + IF=0 invariant: no other
        // process can run (and thus reply) between the send and the block.
        //
        // Returns 0 on success with reply written to a2.
        // Returns ETIMEDOUT if the deadline elapsed without a reply.
        // Returns EAGAIN if the target's mailbox is full.
        // Returns EINVAL for invalid pointers.
        SYS_CALL => {
            const MSG_SIZE:  usize = core::mem::size_of::<core_kernel::ipc::Message>();
            const MSG_ALIGN: usize = core::mem::align_of::<core_kernel::ipc::Message>();
            if !validate_user_ptr(a1, MSG_SIZE, MSG_ALIGN) { return EINVAL; }
            if !validate_user_ptr(a2, MSG_SIZE, MSG_ALIGN) { return EINVAL; }

            let sched = match core_kernel::scheduler::get_global() {
                Some(s) => s,
                None    => return ENOSYS,
            };
            let caller = core_kernel::process::ProcessId::new(
                core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
            let to_pid = core_kernel::process::ProcessId::new(a0 as u32);

            // Read outgoing message; kernel overwrites sender.
            let mut msg = unsafe { core::ptr::read(a1 as *const core_kernel::ipc::Message) };
            msg.sender = caller;

            // Send: stamps sender PID, sets caller.waiting_for = Some(to_pid).
            if !sched.send_message(caller, to_pid, msg) {
                return EAGAIN;
            }

            // Block until a reply arrives (or timeout).
            let timeout = if a3 == 0 { u64::MAX } else { a3 };
            match sched.blocking_receive(caller, timeout) {
                Some(reply) => {
                    // SMAP is already active for this dispatch frame (see _smap_guard
                    // at the top of dispatch_syscall); the write is safe.
                    unsafe { core::ptr::write(a2 as *mut core_kernel::ipc::Message, reply); }
                    0
                }
                None => ETIMEDOUT,
            }
        }

        // SYS_CAP_GRANT — transfer a capability from the caller to another process.
        //
        // a0 = slot_idx in the calling process's capability table
        // a1 = target PID (destination process)
        //
        // The capability at slot_idx must have the CAP_G (grant) right set; the
        // kernel enforces this check inside `cap_grant()`.  The capability is
        // *copied* into the first free slot of the target's cap table; the source
        // slot is not cleared (use SYS_CAP_REVOKE to explicitly drop your own copy).
        //
        // Returns the destination slot index on success.
        // Returns EPERM  if the capability lacks the grant right.
        // Returns EINVAL if slot_idx ≥ CAP_TABLE_SIZE, the target table is full,
        //                or either PID does not exist.
        // Returns ENOSYS if the scheduler is not initialised.
        SYS_CAP_GRANT => {
            let sched = match core_kernel::scheduler::get_global() {
                Some(s) => s,
                None    => return ENOSYS,
            };
            let caller = core_kernel::process::ProcessId::new(
                core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
            let slot_idx = a0 as usize;
            let to_pid   = core_kernel::process::ProcessId::new(a1 as u32);

            // cap_grant() verifies: slot bounds, CAP_G right, and table space.
            // Returns EPERM when the capability lacks the grant right.
            // Returns EINVAL for any other failure (bad slot, full table, no PID).
            match sched.cap_grant(caller, slot_idx, to_pid) {
                Some(new_slot) => new_slot as u64,
                None => {
                    // Re-inspect the source slot to distinguish EPERM vs EINVAL.
                    if slot_idx < core_kernel::process::CAP_TABLE_SIZE {
                        if let Some(rights) = sched.cap_slot_rights(caller, slot_idx) {
                            if rights & core_kernel::process::CAP_G == 0 {
                                return EPERM;
                            }
                        }
                    }
                    EINVAL
                }
            }
        }

        // SYS_SETQUOTA — set resource quotas for a process at runtime.
        //
        // a0 = pid  (0 = calling process)
        // a1 = memory_quota_pages  (max physical pages; 0 = unlimited)
        // a2 = cpu_budget_ticks    (max ticks per scheduling frame; 0 = unlimited)
        // a3 = ipc_rate_limit      (max IPC sends per 100-tick window; 0 = unlimited)
        //
        // Quotas are enforced by the scheduler on every timer tick:
        //   - memory_quota_pages: checked in SYS_MAP before allocation
        //   - cpu_budget_ticks:   process preempted when budget is exhausted
        //   - ipc_rate_limit:     send_message() drops the message when exceeded
        //
        // IEC 61508 §7.4.1 (temporal partitioning) and §7.4.5 (resource partitioning)
        // require that safety-critical processes have bounded resource consumption.
        //
        // Returns 0 on success, ENOSYS if no scheduler.
        SYS_SETQUOTA => {
            let sched = match core_kernel::scheduler::get_global() {
                Some(s) => s,
                None    => return ENOSYS,
            };
            let caller = core_kernel::process::ProcessId::new(
                core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
            let pid = if a0 == 0 { caller }
                      else        { core_kernel::process::ProcessId::new(a0 as u32) };
            // Saturate IPC rate to u16 range.
            let ipc_rate = a3.min(u16::MAX as u64) as u16;
            sched.set_quotas(pid, a1 as u32, a2 as u32, ipc_rate);
            0
        }

        // SYS_CHAN_BIND — create a Channel capability pointing to a target PID.
        //
        // a0 = target PID (the process this channel delivers messages to)
        // a1 = rights     (bitmask CAP_R|CAP_W|CAP_G; 0 = default CAP_R|CAP_W|CAP_G)
        //
        // Allocates a new `CapKind::Channel` slot in the caller's capability table
        // with `object_id` = target_pid.  The holder of a slot with CAP_W can
        // use `SYS_SEND_CAP` to send to that process without knowing its PID
        // directly; the kernel enforces the right check at send time.
        //
        // Returns the slot index on success.
        // Returns EINVAL if the target PID is 0 or the cap table is full.
        // Returns ENOSYS if the scheduler is not initialised.
        SYS_CHAN_BIND => {
            let sched = match core_kernel::scheduler::get_global() {
                Some(s) => s,
                None    => return ENOSYS,
            };
            let target_pid_raw = a0 as u32;
            if target_pid_raw == 0 { return EINVAL; }
            let caller = core_kernel::process::ProcessId::new(
                core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
            // Default rights: R + W + G (can send, receive notifications, grant).
            let rights: u8 = if a1 == 0 {
                core_kernel::process::CAP_R | core_kernel::process::CAP_W | core_kernel::process::CAP_G
            } else {
                (a1 as u8) & (core_kernel::process::CAP_R | core_kernel::process::CAP_W |
                               core_kernel::process::CAP_G | core_kernel::process::CAP_X)
            };
            let cap = core_kernel::process::Capability::new(
                core_kernel::process::CapKind::Channel,
                rights,
                target_pid_raw,
            );
            match sched.cap_alloc(caller, cap) {
                Some(slot) => slot as u64,
                None       => EINVAL,
            }
        }

        // SYS_SEND_CAP — send a full 72-byte Message via a channel capability.
        //
        // a0 = cap_slot (index in caller's capability table; must be CapKind::Channel + CAP_W)
        // a1 = pointer to a 72-byte Message buffer (8-byte aligned, user space)
        //
        // The kernel verifies the capability's kind (must be Channel) and CAP_W
        // right, extracts the target PID from `object_id`, then forwards the
        // message to that process's mailbox.  The sender field is always
        // overwritten with the calling PID — user space cannot forge it.
        //
        // Security benefit over raw SYS_SEND_MSG: only a process that possesses
        // a Channel capability with CAP_W can send to the target.  A process that
        // knows the target PID but has no capability is rejected with EPERM.
        //
        // Returns 0 on success.
        // Returns EPERM  if the capability lacks CAP_W or is not CapKind::Channel.
        // Returns EINVAL if cap_slot is out of range, buffer invalid, or queue full.
        // Returns ENOSYS if the scheduler is not initialised.
        SYS_SEND_CAP => {
            const MSG_SIZE:  usize = core::mem::size_of::<core_kernel::ipc::Message>();
            const MSG_ALIGN: usize = core::mem::align_of::<core_kernel::ipc::Message>();
            if !validate_user_ptr(a1, MSG_SIZE, MSG_ALIGN) { return EINVAL; }

            let sched = match core_kernel::scheduler::get_global() {
                Some(s) => s,
                None    => return ENOSYS,
            };
            let caller = core_kernel::process::ProcessId::new(
                core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
            let cap_slot = a0 as usize;

            // Look up capability: must be Channel with CAP_W.
            let (target_pid_raw, ok) = match sched.cap_slot_info(caller, cap_slot) {
                Some((core_kernel::process::CapKind::Channel, rights, obj_id))
                    if rights & core_kernel::process::CAP_W != 0 => (obj_id, true),
                Some(_) => (0u32, false),
                None    => (0u32, false),
            };
            if !ok { return EPERM; }

            let mut msg = unsafe { core::ptr::read(a1 as *const core_kernel::ipc::Message) };
            msg.sender = caller; // kernel-stamped; unforgeable
            let to_pid = core_kernel::process::ProcessId::new(target_pid_raw);
            if sched.send_message(caller, to_pid, msg) { 0 } else { EINVAL }
        }

        // SYS_MAP_SHARE — allocate a 4 KB shared physical frame, map it into the
        // caller's address space, and install a Memory capability for it.
        //
        // a0 = vaddr  (4 KB-aligned virtual address in caller's user space)
        // a1 = flags  (bit 0 = writable; frame is always user-accessible)
        //
        // Allocates a new physical 4 KB frame (zeroed), maps it at `vaddr` in
        // the calling process's PML4 with PTE_USER, and allocates a
        // `CapKind::Memory` slot whose `object_id` is the page frame number
        // (physical address >> 12).  The caller can then grant this cap to
        // another process via SYS_CAP_GRANT; the receiver can call SYS_MAP_CAP
        // to map the same frame into its own address space.
        //
        // Returns the Memory cap slot index on success.
        // Returns ENOMEM if the physical allocator is exhausted.
        // Returns EINVAL if `vaddr` is invalid or the cap table is full.
        // Returns ENOSYS if the scheduler is not initialised.
        SYS_MAP_SHARE => {
            use core_kernel::memory::{PTE_PRESENT, PTE_WRITABLE, PTE_USER,
                                      map_page_global, global_alloc_4k, frame_tag, FrameKind};

            if !validate_user_vaddr(a0) { return EINVAL; }

            let sched = match core_kernel::scheduler::get_global() {
                Some(s) => s,
                None    => return ENOSYS,
            };
            let caller = core_kernel::process::ProcessId::new(
                core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));

            // Allocate and zero a fresh 4 KB frame.
            let frame_phys = match global_alloc_4k() {
                Some(p) => { frame_tag(p, FrameKind::UserOwned); p }
                None    => return ENOMEM,
            };
            unsafe { core::ptr::write_bytes(frame_phys as *mut u8, 0, 4096); }

            // Map the frame at the requested virtual address.
            let pml4_phys = sched.get_process_pml4(caller)
                .unwrap_or_else(|| core_kernel::scheduler::KERNEL_PML4_PHYS
                    .load(Ordering::Relaxed));
            let pml4 = unsafe { &mut *(pml4_phys as *mut core_kernel::memory::PageTable) };
            let mut flags = PTE_PRESENT | PTE_USER;
            if a1 & 1 != 0 { flags |= PTE_WRITABLE; }
            if !map_page_global(pml4, a0, frame_phys, flags) {
                // Mapping failed (table node alloc OOM); frame is leaked to keep invariants.
                return ENOMEM;
            }

            // Install a Memory capability: object_id = PFN.
            let pfn = (frame_phys >> 12) as u32;
            let cap = core_kernel::process::Capability::new(
                core_kernel::process::CapKind::Memory,
                core_kernel::process::CAP_R | core_kernel::process::CAP_W
                    | core_kernel::process::CAP_G,
                pfn,
            );
            match sched.cap_alloc(caller, cap) {
                Some(slot) => slot as u64,
                // Table full: the frame is mapped but no cap is issued.
                // The caller can still use the mapping; the frame is just not shareable.
                None => EINVAL,
            }
        }

        // SYS_MAP_CAP — map a shared physical frame into the caller's address space
        // using a Memory capability.
        //
        // a0 = vaddr    (4 KB-aligned virtual address in caller's user space)
        // a1 = cap_slot (index in caller's table; must be CapKind::Memory + CAP_R)
        // a2 = flags    (bit 0 = writable; only effective if cap has CAP_W)
        //
        // Reads the physical frame number from `cap.object_id`, derives the
        // physical address (PFN << 12), and maps it at `vaddr` in the calling
        // process's PML4.  No new physical frame is allocated — the cap holder
        // gains access to the frame that was already allocated by SYS_MAP_SHARE.
        //
        // Returns 0 on success.
        // Returns EPERM  if the capability lacks CAP_R or is not CapKind::Memory.
        // Returns EINVAL if `vaddr` invalid, cap_slot out of range, or map failed.
        // Returns ENOSYS if the scheduler is not initialised.
        SYS_MAP_CAP => {
            use core_kernel::memory::{PTE_PRESENT, PTE_WRITABLE, PTE_USER,
                                      map_page_global};

            if !validate_user_vaddr(a0) { return EINVAL; }

            let sched = match core_kernel::scheduler::get_global() {
                Some(s) => s,
                None    => return ENOSYS,
            };
            let caller = core_kernel::process::ProcessId::new(
                core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));
            let cap_slot = a1 as usize;

            // Look up capability: must be Memory with CAP_R.
            let (pfn, has_write) = match sched.cap_slot_info(caller, cap_slot) {
                Some((core_kernel::process::CapKind::Memory, rights, pfn))
                    if rights & core_kernel::process::CAP_R != 0 =>
                    (pfn, rights & core_kernel::process::CAP_W != 0),
                Some(_) => return EPERM,
                None    => return EPERM,
            };

            let frame_phys = (pfn as u64) << 12;
            let pml4_phys = sched.get_process_pml4(caller)
                .unwrap_or_else(|| core_kernel::scheduler::KERNEL_PML4_PHYS
                    .load(Ordering::Relaxed));
            let pml4 = unsafe { &mut *(pml4_phys as *mut core_kernel::memory::PageTable) };

            // Only allow write if both the flag is set and the cap permits it.
            let mut flags = PTE_PRESENT | PTE_USER;
            if a2 & 1 != 0 && has_write { flags |= PTE_WRITABLE; }

            if map_page_global(pml4, a0, frame_phys, flags) { 0 } else { EINVAL }
        }

        // SYS_LOOKUP_CAP — look up a named service and receive a Channel capability.
        //
        // a0 = pointer to null-terminated ASCII service name (16 bytes)
        //
        // Returns the capability slot index on success.
        // Returns ENOENT if the service name is not registered.
        // Returns ENOMEM if the caller's capability table is full (64 slots).
        // Returns EINVAL if the name pointer is invalid.
        //
        // The returned cap is CapKind::Channel with CAP_W rights, pointing to the
        // registered service PID.  The caller can then use SYS_SEND_CAP(slot, ...)
        // to send messages without knowing the raw PID — the capability is unforgeable.
        //
        // IEC 61508 §7.4.3: capabilities confine inter-process communication to
        // kernel-vetted channels; raw-PID spoofing is prevented by construction.
        SYS_LOOKUP_CAP => {
            if !validate_user_ptr(a0, 16, 1) { return EINVAL; }
            let name = unsafe { core::slice::from_raw_parts(a0 as *const u8, 16) };

            // Resolve name → PID via the service registry.
            let target_pid = match core_kernel::service_registry::lookup(name) {
                Some(p) => p,
                None    => return ENOENT,
            };

            let sched = match core_kernel::scheduler::get_global() {
                Some(s) => s,
                None    => return ENOSYS,
            };
            let caller = core_kernel::process::ProcessId::new(
                core_kernel::scheduler::CURRENT_PID.load(Ordering::Relaxed));

            // Allocate a Channel capability with send rights in the caller's table.
            let cap = core_kernel::process::pcb::Capability::new(
                core_kernel::process::CapKind::Channel,
                core_kernel::process::pcb::CAP_W,
                target_pid,
            );
            match sched.cap_alloc(caller, cap) {
                Some(slot) => slot as u64,
                None       => ENOMEM,
            }
        }

        // SYS_INJECT_FAULT — trigger a CPU exception from within the kernel.
        //
        // Only compiled in when the `fault-injection` Cargo feature is active;
        // the match arm is absent in production builds so no ring-3 code can
        // trigger it (falls through to ENOSYS below).
        //
        // rdi = vector number.  Supported vectors:
        //   3  (#BP breakpoint)     — catch-all handler; logs and irets
        //   6  (#UD invalid opcode) — catch-all handler; logs and irets
        //   13 (#GP general prot.)  — dedicated handler; halts on ring-0 origin
        //   14 (#PF page fault)     — dedicated handler; halts on ring-0 origin
        //
        // WARNING: vectors 13 and 14 triggered from the syscall handler (ring-0)
        // will print a crash log and halt the system.  This is intentional —
        // the QEMU test harness verifies the halt message appears in serial output.
        #[cfg(feature = "fault-injection")]
        SYS_INJECT_FAULT => {
            unsafe {
                match a0 {
                    3  => core::arch::asm!("int 0x03", options(nostack)),
                    6  => core::arch::asm!("int 0x06", options(nostack)),
                    13 => core::arch::asm!("int 0x0D", options(nostack)),
                    14 => core::arch::asm!("int 0x0E", options(nostack)),
                    _  => return EINVAL,
                }
            }
            0
        }

        _ => ENOSYS,
    }
}
