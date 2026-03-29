//! Kernel-side UART IRQ support.
//!
//! Stores the PID of the "uart-drv" server so the COM1 IRQ4 handler can
//! deliver received bytes without going through the service registry (which
//! would add a name-scan in the hot ISR path).
//!
//! Set by `SYS_REGISTER` when name == "uart-drv".
//! Read by the IRQ4 inner handler in `arch-x86_64::interrupts::handlers`.

use core::sync::atomic::{AtomicU32, Ordering};

/// PID of the uart-drv server.  `u32::MAX` = not yet registered.
pub static UART_DRV_PID: AtomicU32 = AtomicU32::new(u32::MAX);

/// Called by `SYS_REGISTER` when "uart-drv" registers.
#[inline]
pub fn set_uart_drv_pid(pid: u32) {
    UART_DRV_PID.store(pid, Ordering::Relaxed);
}

/// Read the stored PID.  Returns `u32::MAX` if not yet registered.
#[inline]
pub fn get_uart_drv_pid() -> u32 {
    UART_DRV_PID.load(Ordering::Relaxed)
}
