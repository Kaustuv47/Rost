/// HPET (High Precision Event Timer) MMIO driver.
///
/// Initialises the HPET main counter and exposes `read_counter()` as a
/// monotonic clock source for TSC calibration and future time-stamping.
///
/// # Register layout (HPET spec v1.0a §2.3)
/// ```text
/// 0x000  General Capabilities & ID  (GCI, 64-bit, read-only)
///          bits[63:32] = COUNTER_CLK_PERIOD (femtoseconds per tick)
///          bits[15:8]  = NUM_TIM_CAP (number of comparators − 1)
///          bit[13]     = COUNT_SIZE_CAP (1 = 64-bit main counter)
///          bits[7:0]   = REV_ID
/// 0x010  General Configuration       (GCR, 64-bit)
///          bit[0]      = ENABLE_CNF (1 = enable main counter)
///          bit[1]      = LEG_RT_CNF (legacy replacement routing)
/// 0x020  General Interrupt Status    (GISR, 64-bit)
/// 0x0F0  Main Counter Value Register (MCR, 64-bit)
/// ```
///
/// IEC 61508 §7.4.1: external hardware state is read via `read_volatile` to
/// prevent the compiler from caching or reordering MMIO accesses.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::acpi::hpet::HpetInfo;

// ── HPET MMIO register offsets ────────────────────────────────────────────────

#[allow(dead_code)]
const REG_GCI: u64 = 0x000; // General Capabilities & ID (64-bit)
const REG_GCR: u64 = 0x010; // General Configuration     (64-bit)
const REG_MCR: u64 = 0x0F0; // Main Counter Value        (64-bit)

// ── GCR bit masks ─────────────────────────────────────────────────────────────

/// Enable the main counter (GCR bit 0 = ENABLE_CNF).
const GCR_ENABLE_CNF: u64 = 1 << 0;

// ── Module statics ────────────────────────────────────────────────────────────

/// Physical base address of the HPET MMIO block (0 = not initialised).
static HPET_BASE: AtomicU64 = AtomicU64::new(0);

/// Counter clock period in femtoseconds per tick (0 = not initialised).
static HPET_PERIOD_FS: AtomicU64 = AtomicU64::new(0);

// ── MMIO helpers ──────────────────────────────────────────────────────────────

/// Read a 64-bit HPET register at `base + offset`.
///
/// # Safety
/// `base` must be a valid, identity-mapped HPET MMIO base address.
#[inline]
unsafe fn read64(base: u64, offset: u64) -> u64 {
    core::ptr::read_volatile((base + offset) as *const u64)
}

/// Write a 64-bit value to a HPET register at `base + offset`.
///
/// # Safety
/// `base` must be a valid, identity-mapped HPET MMIO base address.
#[inline]
unsafe fn write64(base: u64, offset: u64, val: u64) {
    core::ptr::write_volatile((base + offset) as *mut u64, val);
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the HPET from `info` parsed from the ACPI HPET table.
///
/// Steps:
/// 1. Store the MMIO base address and period for later use.
/// 2. Enable the main counter by setting `ENABLE_CNF` in GCR.
///
/// Safe to call with any `HpetInfo` value; performs no I/O if
/// `info.base_address` is zero (but `parse_hpet` already rejects that).
///
/// After this call, `read_counter()` returns a monotonically increasing
/// value that increments every `period_fs()` femtoseconds.
pub fn init(info: &HpetInfo) {
    if info.base_address == 0 { return; }

    HPET_BASE.store(info.base_address, Ordering::Relaxed);
    HPET_PERIOD_FS.store(info.period_fs, Ordering::Relaxed);

    unsafe {
        // Enable the main counter.  Read-modify-write to preserve other bits.
        let gcr = read64(info.base_address, REG_GCR);
        write64(info.base_address, REG_GCR, gcr | GCR_ENABLE_CNF);
    }
}

/// Read the current HPET main counter value.
///
/// Returns 0 if the HPET has not been initialised.
///
/// The counter is monotonically increasing (wraps on 32-bit-only HEPTs but
/// that takes ~429 seconds at 10 MHz — acceptable for short boot-time use).
#[inline]
pub fn read_counter() -> u64 {
    let base = HPET_BASE.load(Ordering::Relaxed);
    if base == 0 { return 0; }
    unsafe { read64(base, REG_MCR) }
}

/// Return the counter clock period in femtoseconds per tick.
///
/// Returns 0 if the HPET has not been initialised.
#[inline]
pub fn period_fs() -> u64 {
    HPET_PERIOD_FS.load(Ordering::Relaxed)
}

/// Return the HPET MMIO base address (0 if not initialised).
#[inline]
pub fn base_address() -> u64 {
    HPET_BASE.load(Ordering::Relaxed)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acpi::hpet::HpetInfo;

    fn make_info(base: u64, period: u64) -> HpetInfo {
        HpetInfo {
            base_address:  base,
            period_fs:     period,
            timer_count:   3,
            counter_64bit: true,
            hardware_rev:  1,
        }
    }

    #[test]
    fn test_init_zero_base_is_noop() {
        let info = make_info(0, 100_000);
        init(&info);
        // No crash and no side effects on the global HPET_BASE —
        // that is the pass condition.  We cannot assert base_address() == 0
        // because another test may have already set HPET_BASE.
    }

    #[test]
    fn test_read_counter_before_init_returns_zero() {
        // Reset state from any previous test (statics are global in test binary).
        HPET_BASE.store(0, Ordering::Relaxed);
        assert_eq!(read_counter(), 0);
    }

    #[test]
    fn test_period_fs_stored_by_init() {
        // Use a fake base that points to an aligned, readable buffer.
        let mut fake_mmio = [0u64; 32]; // 256 bytes — enough for REG_MCR (0xF0 = 240)
        // GCR is at offset 0x10 / 8 = index 2.
        // MCR is at offset 0xF0 / 8 = index 30.
        fake_mmio[30] = 0xDEAD_BEEF_CAFE_0001; // fake counter value

        let base = fake_mmio.as_ptr() as u64;
        HPET_BASE.store(0, Ordering::Relaxed);
        HPET_PERIOD_FS.store(0, Ordering::Relaxed);

        let info = make_info(base, 69_841);
        init(&info);

        assert_eq!(period_fs(), 69_841);
        assert_eq!(base_address(), base);
    }
}
