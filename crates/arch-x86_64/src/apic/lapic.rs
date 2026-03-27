/// Local APIC (xAPIC mode) initialisation.
///
/// # Register map (offsets from LAPIC MMIO base, 32-bit registers)
/// ```text
/// 0x020  ID
/// 0x030  Version
/// 0x080  Task Priority Register (TPR)
/// 0x0B0  End-of-Interrupt (write 0 to acknowledge)
/// 0x0D0  Logical Destination Register
/// 0x0E0  Destination Format Register
/// 0x0F0  Spurious Interrupt Vector Register (SVR) — bit 8 = SW enable
/// 0x320  LVT Timer — bits[17:16] = timer mode (01=periodic), bits[7:0] = vector
/// 0x350  LVT LINT0
/// 0x360  LVT LINT1
/// 0x370  LVT Error
/// 0x380  Timer Initial Count (ICR)
/// 0x390  Timer Current Count (CCR) — read-only
/// 0x3E0  Timer Divide Config (DCR) — 0x3 = ÷16
/// ```

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// ── Module-level statics ──────────────────────────────────────────────────────

/// Physical base address of the Local APIC MMIO block (0 = not initialised).
///
/// Stored at LAPIC init so `arm_oneshot()` can be called from the timer ISR
/// without needing a parameter.
pub static LAPIC_BASE: AtomicU64 = AtomicU64::new(0);

/// LAPIC timer Initial Count Register (ICR) value for one 10 ms tick.
///
/// Calibrated by `calibrate_lapic_timer_10ms()` and stored at init.
/// `arm_oneshot(n)` programs the ICR to `LAPIC_ICR_PER_TICK × n`.
pub static LAPIC_ICR_PER_TICK: AtomicU32 = AtomicU32::new(0);

// ── LAPIC register offsets ────────────────────────────────────────────────────

const REG_TPR:        usize = 0x080;
const REG_EOI:        usize = 0x0B0;
const REG_SVR:        usize = 0x0F0;
const REG_LVT_TIMER:  usize = 0x320;
const REG_LVT_LINT0:  usize = 0x350;
const REG_LVT_LINT1:  usize = 0x360;
const REG_LVT_ERROR:  usize = 0x370;
const REG_TIMER_ICR:  usize = 0x380;
const REG_TIMER_CCR:  usize = 0x390;
const REG_TIMER_DCR:  usize = 0x3E0;

/// LVT mask bit — set to suppress delivery of the corresponding interrupt.
const LVT_MASK: u32 = 1 << 16;
/// LVT Timer: periodic mode (bits[17:16] = 01).
#[allow(dead_code)]
const LVT_TIMER_PERIODIC: u32 = 1 << 17;

// ── MMIO helpers ─────────────────────────────────────────────────────────────

/// Write a 32-bit value to a LAPIC register.
///
/// # Safety
/// `lapic_base` must be a valid, mapped LAPIC MMIO base address.
#[inline]
unsafe fn write_reg(lapic_base: u64, offset: usize, val: u32) {
    core::ptr::write_volatile((lapic_base as usize + offset) as *mut u32, val);
}

/// Read a 32-bit value from a LAPIC register.
///
/// # Safety
/// `lapic_base` must be a valid, mapped LAPIC MMIO base address.
#[inline]
unsafe fn read_reg(lapic_base: u64, offset: usize) -> u32 {
    core::ptr::read_volatile((lapic_base as usize + offset) as *const u32)
}

// ── Port I/O helpers (used for PIT calibration) ───────────────────────────────

#[inline]
unsafe fn outb(port: u16, val: u8) {
    core::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") val,
        options(nostack, preserves_flags)
    );
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    core::arch::asm!(
        "in al, dx",
        out("al") val,
        in("dx") port,
        options(nostack, preserves_flags)
    );
    val
}

// ── PIT channel 2 calibration ─────────────────────────────────────────────────

/// Measure LAPIC timer ticks per 10 ms using PIT channel 2 polling.
///
/// PIT channel 2 is gated by bit 0 of I/O port 0x61 and its output appears
/// on bit 5 of the same port.  In mode 0 (one-shot) the output starts LOW
/// and goes HIGH when the initial count expires.
///
/// With the LAPIC timer dividing the bus clock by 16, the returned value is
/// the number of LAPIC timer ticks (post-divide) elapsed during one 10 ms
/// window — suitable as the periodic ICR for a 100 Hz LAPIC timer.
///
/// Returns 0 if no LAPIC base was supplied (called with base == 0).
unsafe fn calibrate_lapic_timer_10ms(lapic_base: u64) -> u32 {
    if lapic_base == 0 { return 0; }

    const PIT_HZ:      u32 = 1_193_182;
    // Ticks for 10 ms at PIT rate (100 Hz).
    const CALIB_TICKS: u16 = (PIT_HZ / 100) as u16; // 11931

    // ── Set up PIT channel 2 for 10 ms one-shot ───────────────────────────────
    // Port 0x61: bit 0 = PIT ch2 gate, bit 1 = speaker enable, bit 5 = ch2 OUT.
    let p61 = inb(0x61);
    outb(0x61, (p61 & !0x02) | 0x01); // gate on, speaker off

    // Command: channel 2 (bits[7:6]=10), lo/hi access (bits[5:4]=11),
    //          mode 0 (bits[3:1]=000), binary (bit0=0) → 0xB0.
    outb(0x43, 0xB0);
    outb(0x42, (CALIB_TICKS & 0xFF) as u8); // low byte
    outb(0x42, (CALIB_TICKS >> 8) as u8);   // high byte

    // ── Start LAPIC timer countdown ───────────────────────────────────────────
    write_reg(lapic_base, REG_TIMER_DCR, 0x3); // ÷16
    write_reg(lapic_base, REG_TIMER_ICR, 0xFFFF_FFFF);

    // ── Wait for PIT channel 2 output to go HIGH (bit 5 of port 0x61) ────────
    // Mode 0 OUT starts LOW and transitions HIGH when the counter reaches 0.
    // Timeout after 100 K iterations (~1–2 s at ~10 µs/inb under QEMU/HVF).
    // PIT ch2 is not reliably emulated in HVF; the fallback ICR handles that.
    // (IEC 61508 §7.4.1 liveness — bound all busy-waits.)
    let mut timeout = 100_000u32;
    while inb(0x61) & 0x20 == 0 {
        timeout -= 1;
        if timeout == 0 { return 0; } // calibration failed — caller uses fallback ICR
    }

    // ── Read elapsed LAPIC ticks ──────────────────────────────────────────────
    let ccr = read_reg(lapic_base, REG_TIMER_CCR);
    0xFFFF_FFFF - ccr
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the xAPIC Local APIC at `lapic_base`.
///
/// Steps performed:
/// 1. Software-enable the LAPIC via SVR (spurious vector = 0xFF).
/// 2. Set Task Priority = 0 (accept all interrupts).
/// 3. Mask LINT0, LINT1, and Error LVT entries.
/// 4. Calibrate LAPIC timer against PIT channel 2 for 100 Hz period.
/// 5. Program LAPIC timer in periodic mode, vector 32.
/// 6. Store the LAPIC EOI register address so the timer ISR can acknowledge
///    LAPIC-delivered interrupts (see `LAPIC_EOI_ADDR` in interrupts module).
/// 7. Mask all 8259 PIC interrupt lines.
///
/// Safe to call with `lapic_base == 0`; in that case APIC init is skipped.
pub fn init(lapic_base: u64) {
    if lapic_base == 0 {
        hal::uart::print_str("      [WARN] LAPIC base unknown — keeping 8259 PIC\n");
        return;
    }

    unsafe {
        // 1. Enable LAPIC: SVR bit 8 = software enable; spurious vector = 0xFF.
        write_reg(lapic_base, REG_SVR, svr_value(0xFF));

        // 2. Accept all interrupt priorities.
        write_reg(lapic_base, REG_TPR, 0);

        // 3. Mask external interrupt lines (LINT0/LINT1) and error LVT.
        write_reg(lapic_base, REG_LVT_LINT0, LVT_MASK);
        write_reg(lapic_base, REG_LVT_LINT1, LVT_MASK);
        write_reg(lapic_base, REG_LVT_ERROR,  LVT_MASK);

        // 4. Calibrate the LAPIC timer.
        let icr = calibrate_lapic_timer_10ms(lapic_base);
        let icr = if icr != 0 { icr } else { 0x0010_0000 };

        // 5. Store base and per-tick ICR for arm_oneshot() called from ISR.
        LAPIC_BASE.store(lapic_base, Ordering::Relaxed);
        LAPIC_ICR_PER_TICK.store(icr, Ordering::Relaxed);

        // 6. Program LAPIC timer: one-shot mode, vector 32.
        //    arm_oneshot() re-programs the ICR after each interrupt so the
        //    LAPIC fires at the exact moment the next event is due.
        write_reg(lapic_base, REG_TIMER_DCR, 0x3); // ÷16
        write_reg(lapic_base, REG_LVT_TIMER, lvt_timer_value(32, false)); // one-shot
        write_reg(lapic_base, REG_TIMER_ICR, icr); // first shot: 1 normal tick

        // 7. Register EOI address so the timer ISR can acknowledge LAPIC ticks.
        let eoi_addr = lapic_base + REG_EOI as u64;
        super::super::interrupts::LAPIC_EOI_ADDR
            .store(eoi_addr, Ordering::Relaxed);

        // 8. Mask all 8259 PIC lines: write 0xFF to master (0x21) and slave (0xA1).
        pic_mask_all();
    }

    hal::uart::print_str("      ├─ LAPIC enabled:    SVR=0x1FF, LINT0/LINT1 masked\n");
    hal::uart::print_str("      ├─ LAPIC timer:      one-shot mode, vector 32\n");
    hal::uart::print_str("      ├─ 8259 PIC:         all lines masked (LAPIC active)\n");
}

/// Arm the LAPIC one-shot timer to fire after `ticks` scheduler ticks.
///
/// Each "tick" corresponds to the 10 ms window calibrated by
/// `calibrate_lapic_timer_10ms()` and stored in `LAPIC_ICR_PER_TICK`.
///
/// Callers typically pass `1` for a normal quantum expiry or a smaller value
/// when a blocked process has an imminent deadline.
///
/// No-ops if the LAPIC has not been initialised (base or ICR is zero).
///
/// Called from `tick_scheduler_isr()` after each scheduler tick so the timer
/// fires exactly when the next event (deadline or quantum end) is due.
pub fn arm_oneshot(ticks: u32) {
    let base = LAPIC_BASE.load(Ordering::Relaxed);
    if base == 0 { return; }
    let icr = LAPIC_ICR_PER_TICK.load(Ordering::Relaxed);
    if icr == 0 { return; }
    // Saturating multiply to avoid wrapping; clamp to u32::MAX on overflow.
    let count = (icr as u64).saturating_mul(ticks as u64).min(u32::MAX as u64) as u32;
    let count = count.max(1); // never write 0 — that would never fire
    unsafe { write_reg(base, REG_TIMER_ICR, count); }
}

/// Mask all 8259 PIC interrupt lines by writing 0xFF to both IMR registers.
///
/// Called after the LAPIC timer replaces the PIC as the scheduling clock.
/// PIC EOIs from the timer ISR are harmless to a fully masked PIC.
fn pic_mask_all() {
    unsafe {
        core::arch::asm!(
            "mov al, 0xFF",
            "out 0x21, al", // master PIC IMR
            "out 0xA1, al", // slave  PIC IMR
            out("al") _,
            options(nostack, preserves_flags)
        );
    }
}

// ── Register-value encoding (pure functions, unit-testable) ───────────────────

/// Encode the Spurious Interrupt Vector Register value.
///
/// Bit 8 = APIC Software Enable; bits[7:0] = spurious vector.
#[inline]
pub fn svr_value(spurious_vec: u8) -> u32 {
    (1u32 << 8) | (spurious_vec as u32)
}

/// Encode the LVT Timer register value.
///
/// - `vector`: IDT vector to deliver (must be ≥ 32).
/// - `periodic`: `true` → periodic mode (bits[17:16]=01); `false` → one-shot.
#[inline]
pub fn lvt_timer_value(vector: u8, periodic: bool) -> u32 {
    let mode: u32 = if periodic { 1 << 17 } else { 0 };
    (vector as u32) | mode
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// SVR encodes the enable bit and spurious vector correctly.
    #[test]
    fn test_svr_value() {
        let v = svr_value(0xFF);
        assert_eq!(v & (1 << 8), 1 << 8, "SW-enable bit must be set");
        assert_eq!(v & 0xFF, 0xFF, "spurious vector must be in bits[7:0]");
        assert_eq!(v, 0x1FF);
    }

    /// LVT timer periodic mode has bit 17 set; one-shot does not.
    #[test]
    fn test_lvt_timer_periodic() {
        let p = lvt_timer_value(32, true);
        assert_eq!(p & 0xFF, 32, "vector must be 32");
        assert!(p & (1 << 17) != 0, "periodic bit must be set");

        let o = lvt_timer_value(32, false);
        assert_eq!(o & 0xFF, 32, "vector must be 32");
        assert!(o & (1 << 17) == 0, "periodic bit must be clear for one-shot");
    }

    /// SVR with vector 0xFE still sets the enable bit.
    #[test]
    fn test_svr_enable_bit_independent_of_vector() {
        for v in [0x00u8, 0x20, 0xFF] {
            let svr = svr_value(v);
            assert!(svr & (1 << 8) != 0, "SW-enable must be set for any vector");
        }
    }
}
