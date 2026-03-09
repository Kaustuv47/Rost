use crate::cpu::{InterruptDescriptorTable, IdtEntry};

/// Initialize interrupt handlers
pub fn init(idt: &mut InterruptDescriptorTable) {
    idt.set_entry(0,  IdtEntry::interrupt_gate(divide_by_zero_handler as u64, 0x8));
    idt.set_entry(13, IdtEntry::interrupt_gate(general_protection_fault_handler as u64, 0x8));
    idt.set_entry(14, IdtEntry::interrupt_gate(page_fault_handler as u64, 0x8));
    idt.set_entry(32, IdtEntry::interrupt_gate(timer_interrupt_handler as u64, 0x8));
}

/// Division by zero — no error code, halt loop
#[unsafe(naked)]
unsafe extern "C" fn divide_by_zero_handler() {
    core::arch::naked_asm!(
        "0: cli",
        "hlt",
        "jmp 0b",
    );
}

/// General protection fault — CPU pushes error code, halt loop
#[unsafe(naked)]
unsafe extern "C" fn general_protection_fault_handler() {
    core::arch::naked_asm!(
        "0: cli",
        "hlt",
        "jmp 0b",
    );
}

/// Page fault — CPU pushes error code, halt loop
#[unsafe(naked)]
unsafe extern "C" fn page_fault_handler() {
    core::arch::naked_asm!(
        "0: cli",
        "hlt",
        "jmp 0b",
    );
}

/// Timer IRQ0 — send EOI to PIC and return
#[unsafe(naked)]
unsafe extern "C" fn timer_interrupt_handler() {
    core::arch::naked_asm!(
        "push rax",
        "mov al, 0x20",
        "out 0x20, al",   // EOI to master PIC
        "pop rax",
        "iretq",
    );
}
