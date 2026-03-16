mod round_robin;

pub use round_robin::{Scheduler, AuditEntry, AuditKind};

/// Physical address of the kernel PML4 table.
///
/// Set once during stage 1 memory management; read by SYS_SPAWN when the
/// caller does not provide an explicit page table base.
pub static KERNEL_PML4_PHYS: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Record the kernel PML4 physical address for use by SYS_SPAWN.
pub fn set_kernel_pml4(addr: u64) {
    KERNEL_PML4_PHYS.store(addr, core::sync::atomic::Ordering::Relaxed);
}

// ── Global scheduler instance ─────────────────────────────────────────────────
//
// Initialised once in `kernel::main` before interrupts are enabled.
// Single-core kernel — no lock needed; access is always from ring-0 with
// interrupts either disabled (syscall entry) or from our single timer ISR path.

/// The global kernel scheduler — `None` until `init_global()` is called.
pub static mut GLOBAL_SCHEDULER: Option<Scheduler> = None;

/// PID of the process currently occupying the CPU.
/// Updated by `tick_scheduler()` on every context switch.
/// Read by `SYS_GETPID` in syscall context.
pub static CURRENT_PID: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

/// Install the global scheduler.  Must be called exactly once before the first
/// timer interrupt or syscall can fire.
pub fn init_global(s: Scheduler) {
    unsafe { GLOBAL_SCHEDULER = Some(s); }
}

/// Borrow the global scheduler (shared reference).
/// Returns `None` if `init_global` has not been called yet.
#[inline]
pub fn get_global() -> Option<&'static Scheduler> {
    unsafe { core::ptr::addr_of!(GLOBAL_SCHEDULER).as_ref().and_then(|o| o.as_ref()) }
}
