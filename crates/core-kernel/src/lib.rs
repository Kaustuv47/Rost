// In test mode the standard library is available (test runner requires std).
// In production (no_std target) we use the bare alloc crate.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod acpi;
pub mod boot_info;
pub mod crash_log;
pub mod elf_spawn;
pub mod hpet;
pub mod stack_guard;
pub mod iommu;
pub mod ipc;
pub mod memory;
pub mod process;
pub mod scheduler;
pub mod service_registry;
pub mod irq_registry;
pub mod uart_irq;
pub mod framebuf;
