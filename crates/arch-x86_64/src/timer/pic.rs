/// Configure PIC 8259 master/slave and remap IRQs to vectors 32–47
pub(super) fn configure_pic() {
    unsafe {
        // ICW1: Start initialization (8086 mode)
        core::arch::asm!(
            "mov al, 0x11", "out 0x20, al", "out 0xA0, al",
            options(nostack, preserves_flags)
        );
        // ICW2: Interrupt vector offsets (master=32, slave=40)
        core::arch::asm!(
            "mov al, 0x20", "out 0x21, al",
            "mov al, 0x28", "out 0xA1, al",
            options(nostack, preserves_flags)
        );
        // ICW3: Cascade wiring
        core::arch::asm!(
            "mov al, 0x04", "out 0x21, al", // slave on IRQ2
            "mov al, 0x02", "out 0xA1, al", // slave identifier
            options(nostack, preserves_flags)
        );
        // ICW4: 8086 mode
        core::arch::asm!(
            "mov al, 0x01", "out 0x21, al", "out 0xA1, al",
            options(nostack, preserves_flags)
        );
        // OCW1: Unmask all interrupts
        core::arch::asm!(
            "mov al, 0x00", "out 0x21, al", "out 0xA1, al",
            options(nostack, preserves_flags)
        );
    }
}
