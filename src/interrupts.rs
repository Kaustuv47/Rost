use crate::cpu::{InterruptDescriptorTable, IdtEntry};

/// Exception frame pushed by CPU during interrupt
#[repr(C)]
pub struct ExceptionFrame {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

/// Initialize interrupt handlers
pub fn init(idt: &mut InterruptDescriptorTable) {
    // Division by zero handler
    idt.set_entry(0, IdtEntry::interrupt_gate(
        divide_by_zero_handler as *const () as u64,
        0x8,
    ));

    // Page fault handler
    idt.set_entry(14, IdtEntry::interrupt_gate(
        page_fault_handler as *const () as u64,
        0x8,
    ));

    // General protection fault handler
    idt.set_entry(13, IdtEntry::interrupt_gate(
        general_protection_fault_handler as *const () as u64,
        0x8,
    ));

    // Timer interrupt handler (IRQ0, maps to INT32)
    idt.set_entry(32, IdtEntry::interrupt_gate(
        timer_interrupt_handler as *const () as u64,
        0x8,
    ));
}

/// Division by zero exception handler
extern "C" fn divide_by_zero_handler(frame: &ExceptionFrame) {
    crate::console::print_str("Division by zero exception!\n");
    print_exception_frame(frame);
    crate::cpu::halt();
}

/// Page fault exception handler
extern "C" fn page_fault_handler(frame: &ExceptionFrame) {
    let cr2: u64;
    unsafe {
        core::arch::asm!("mov {}, cr2", out(reg) cr2, options(nostack, preserves_flags));
    }
    crate::console::print_str("Page fault at address: ");
    crate::console::print_hex(cr2);
    crate::console::print_str("\n");
    print_exception_frame(frame);
    crate::cpu::halt();
}

/// General protection fault handler
extern "C" fn general_protection_fault_handler(frame: &ExceptionFrame) {
    crate::console::print_str("General protection fault!\n");
    print_exception_frame(frame);
    crate::cpu::halt();
}

/// Timer interrupt handler (will be used by scheduler)
extern "C" fn timer_interrupt_handler(_frame: &ExceptionFrame) {
    // Send End of Interrupt (EOI) to PIC
    unsafe {
        core::arch::asm!(
        "mov al, 0x20",
        "out 0x20, al",
        options(nostack, preserves_flags)
        );
    }
    // Scheduler will handle context switching here
}

fn print_exception_frame(frame: &ExceptionFrame) {
    crate::console::print_str("RAX: ");
    crate::console::print_hex(frame.rax);
    crate::console::print_str(" RIP: ");
    crate::console::print_hex(frame.rip);
    crate::console::print_str("\n");
}
