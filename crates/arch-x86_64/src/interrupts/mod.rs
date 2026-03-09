mod handlers;

use crate::cpu::{InterruptDescriptorTable, IdtEntry};
use handlers::*;

/// Wire all interrupt handlers into the IDT
pub fn init(idt: &mut InterruptDescriptorTable) {
    idt.set_entry(0,  IdtEntry::interrupt_gate(divide_by_zero_handler as *const () as u64, 0x8));
    idt.set_entry(13, IdtEntry::interrupt_gate(general_protection_fault_handler as *const () as u64, 0x8));
    idt.set_entry(14, IdtEntry::interrupt_gate(page_fault_handler as *const () as u64, 0x8));
    idt.set_entry(32, IdtEntry::interrupt_gate(timer_interrupt_handler as *const () as u64, 0x8));
}
