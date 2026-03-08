/// Initialize system timer for scheduling interrupts
pub fn init() {
    // Configure PIT (Programmable Interval Timer) for 100 Hz (10ms ticks)
    let frequency = 100;
    let divisor = 1193180 / frequency;

    unsafe {
        // Set command byte for channel 0: binary, 16-bit, rate generator
        core::arch::asm!(
        "mov al, 0x34",
        "out 0x43, al",
        options(nostack, preserves_flags)
        );

        // Set divisor low byte
        core::arch::asm!(
        "mov al, {}",
        "out 0x40, al",
        in(reg_byte) (divisor & 0xFF) as u8,
        options(nostack, preserves_flags)
        );

        // Set divisor high byte
        core::arch::asm!(
        "mov al, {}",
        "out 0x40, al",
        in(reg_byte) ((divisor >> 8) & 0xFF) as u8,
        options(nostack, preserves_flags)
        );
    }

    // Enable PIC interrupts for timer (IRQ0)
    configure_pic();
}

/// Configure PIC (Programmable Interrupt Controller)
fn configure_pic() {
    unsafe {
        // ICW1: Start initialization (8086 mode)
        core::arch::asm!(
        "mov al, 0x11",
        "out 0x20, al",     // Master PIC
        "out 0xA0, al",     // Slave PIC
        options(nostack, preserves_flags)
        );

        // ICW2: Set interrupt vector offset
        core::arch::asm!(
        "mov al, 0x20",     // Master IRQs start at 32
        "out 0x21, al",
        "mov al, 0x28",     // Slave IRQs start at 40
        "out 0xA1, al",
        options(nostack, preserves_flags)
        );

        // ICW3: Set master/slave configuration
        core::arch::asm!(
        "mov al, 0x04",     // Slave on IRQ2
        "out 0x21, al",
        "mov al, 0x02",     // Slave identifier
        "out 0xA1, al",
        options(nostack, preserves_flags)
        );

        // ICW4: 8086 mode
        core::arch::asm!(
        "mov al, 0x01",
        "out 0x21, al",
        "out 0xA1, al",
        options(nostack, preserves_flags)
        );

        // OCW1: Unmask all interrupts
        core::arch::asm!(
        "mov al, 0x00",
        "out 0x21, al",
        "out 0xA1, al",
        options(nostack, preserves_flags)
        );
    }
}
