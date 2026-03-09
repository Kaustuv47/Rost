mod pic;

/// Initialize PIT at 100 Hz and configure PIC
pub fn init() {
    let divisor = 1193180u32 / 100; // 100 Hz → 10 ms ticks

    unsafe {
        // PIT channel 0: binary, 16-bit, rate generator
        core::arch::asm!(
            "mov al, 0x34", "out 0x43, al",
            options(nostack, preserves_flags)
        );
        core::arch::asm!(
            "mov al, {lo}", "out 0x40, al",
            lo = in(reg_byte) (divisor & 0xFF) as u8,
            options(nostack, preserves_flags)
        );
        core::arch::asm!(
            "mov al, {hi}", "out 0x40, al",
            hi = in(reg_byte) ((divisor >> 8) & 0xFF) as u8,
            options(nostack, preserves_flags)
        );
    }

    pic::configure_pic();
}
