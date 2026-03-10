mod handlers;

use crate::cpu::{InterruptDescriptorTable, IdtEntry};
use handlers::*;

/// Monotonically increasing tick counter — incremented by the timer ISR at 100 Hz.
/// Readable from any crate via `arch_x86_64::interrupts::TICK_COUNT`.
pub static TICK_COUNT: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Wire all interrupt/exception handlers into the IDT.
pub fn init(idt: &mut InterruptDescriptorTable) {
    idt.set_entry( 0, IdtEntry::interrupt_gate(divide_by_zero_handler           as *const () as u64, 0x8));
    idt.set_entry(13, IdtEntry::interrupt_gate(general_protection_fault_handler  as *const () as u64, 0x8));
    idt.set_entry(14, IdtEntry::interrupt_gate(page_fault_handler                as *const () as u64, 0x8));
    idt.set_entry(32, IdtEntry::interrupt_gate(timer_interrupt_handler           as *const () as u64, 0x8));
}
