/// Division by zero — no error code
#[unsafe(naked)]
pub unsafe extern "C" fn divide_by_zero_handler() {
    core::arch::naked_asm!("0: cli", "hlt", "jmp 0b");
}

/// General protection fault — CPU pushes error code
#[unsafe(naked)]
pub unsafe extern "C" fn general_protection_fault_handler() {
    core::arch::naked_asm!("0: cli", "hlt", "jmp 0b");
}

/// Page fault — CPU pushes error code
#[unsafe(naked)]
pub unsafe extern "C" fn page_fault_handler() {
    core::arch::naked_asm!("0: cli", "hlt", "jmp 0b");
}

/// Timer IRQ0 — send EOI to master PIC and return
#[unsafe(naked)]
pub unsafe extern "C" fn timer_interrupt_handler() {
    core::arch::naked_asm!(
        "push rax",
        "mov al, 0x20",
        "out 0x20, al",
        "pop rax",
        "iretq",
    );
}
