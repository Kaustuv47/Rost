pub mod gdt;
pub mod idt;

pub use gdt::GlobalDescriptorTable;
pub use idt::{IdtEntry, InterruptDescriptorTable};

pub fn enable_interrupts() {
    unsafe { core::arch::asm!("sti", options(nostack, preserves_flags)); }
}

pub fn disable_interrupts() {
    unsafe { core::arch::asm!("cli", options(nostack, preserves_flags)); }
}

pub fn halt() {
    unsafe { core::arch::asm!("hlt", options(nostack, preserves_flags)); }
}
