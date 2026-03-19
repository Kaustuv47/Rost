/// TSC (Time Stamp Counter) calibration against the HPET main counter.
///
/// The TSC is the lowest-latency clock source on x86 but its frequency is
/// not defined in any standard — it must be measured at boot.  This module
/// calibrates it by measuring how many TSC ticks elapse over a known HPET
/// window (10 ms at the HPET's stated femtosecond period).
///
/// # Calibration algorithm
/// 1. Read the HPET main counter and the TSC simultaneously.
/// 2. Spin-wait until the HPET counter advances by `10^13 / period_fs` ticks
///    (= 10 ms exactly, given the HPET-stated period).
/// 3. Read the TSC again; compute `tsc_delta / 10` → TSC kHz.
///
/// The result is stored in `TSC_KHZ` and exposed via `khz()`.  All subsequent
/// ns-resolution timestamps use `tsc_to_ns()` without further calibration.
///
/// # Precision note
/// The calibration loop is a busy-wait — it must not be interrupted by other
/// code.  Call this function early in Stage 4, before enabling the scheduler.
///
/// IEC 61508 §7.4.1: clock source quality must be verified at startup.

use core::sync::atomic::{AtomicU64, Ordering};

/// TSC frequency in kHz, populated by `calibrate()`.
///
/// Zero until `calibrate()` has been called successfully.
static TSC_KHZ: AtomicU64 = AtomicU64::new(0);

// ── HPET MMIO helper (identical to core-kernel/src/hpet.rs, inlined here
//    to avoid a cross-crate dependency on a no_std MMIO call) ─────────────────

/// Read the HPET main counter at `hpet_base + 0x0F0`.
///
/// # Safety
/// `hpet_base` must be a valid, identity-mapped HPET MMIO base address.
#[inline]
unsafe fn read_hpet_counter(hpet_base: u64) -> u64 {
    core::ptr::read_volatile((hpet_base + 0x0F0) as *const u64)
}

/// Execute the `rdtsc` instruction and return the 64-bit TSC value.
///
/// # Safety
/// The calling context must not be preempted between the HPET and TSC reads
/// (interrupts should be disabled or the measurement window should be short
/// enough that preemption does not introduce visible error).
#[inline]
unsafe fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    core::arch::asm!(
        "rdtsc",
        out("eax") lo,
        out("edx") hi,
        options(nomem, nostack, preserves_flags),
    );
    ((hi as u64) << 32) | lo as u64
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Calibrate the TSC frequency against the HPET main counter.
///
/// Spins for exactly 10 ms (measured by the HPET) and stores
/// `tsc_delta / 10` as `TSC_KHZ`.
///
/// No-ops if:
/// - `hpet_base` is zero (HPET not present)
/// - `period_fs` is zero (invalid HPET info)
/// - The computed `target_ticks` is zero (period_fs > 10^13 — impossible for
///   a valid HPET but guarded for robustness)
///
/// # Safety
/// `hpet_base` must be a valid, identity-mapped HPET MMIO base.
pub fn calibrate(hpet_base: u64, period_fs: u64) {
    if hpet_base == 0 || period_fs == 0 { return; }

    // Number of HPET ticks in 10 ms.
    // 10 ms = 10^13 fs; target_ticks = 10^13 / period_fs.
    let target_ticks: u64 = 10_000_000_000_000u64 / period_fs;
    if target_ticks == 0 { return; }

    let start_hpet = unsafe { read_hpet_counter(hpet_base) };
    let start_tsc  = unsafe { rdtsc() };

    // Busy-wait for 10 ms.
    // Timeout after 2 × target_ticks iterations; if the HPET counter appears
    // stuck (e.g. MMIO unresponsive) we return without storing TSC_KHZ so
    // callers receive 0 from khz() and skip TSC-based timing gracefully.
    // IEC 61508 §7.4.1: clock-source verification must be liveness-bounded.
    let max_iters: u64 = target_ticks.saturating_mul(2).max(1_000_000);
    let mut iters: u64 = 0;
    loop {
        let cur = unsafe { read_hpet_counter(hpet_base) };
        if cur.wrapping_sub(start_hpet) >= target_ticks { break; }
        iters += 1;
        if iters >= max_iters { return; } // HPET stuck — skip calibration
    }

    let end_tsc = unsafe { rdtsc() };
    let tsc_delta = end_tsc.wrapping_sub(start_tsc);

    // tsc_delta ticks elapsed in 10 ms → kHz = tsc_delta / 10.
    TSC_KHZ.store(tsc_delta / 10, Ordering::Relaxed);
}

/// Return the calibrated TSC frequency in kHz (0 if not yet calibrated).
#[inline]
pub fn khz() -> u64 {
    TSC_KHZ.load(Ordering::Relaxed)
}

/// Convert a TSC delta (in raw TSC ticks) to nanoseconds.
///
/// Returns 0 if `calibrate()` has not been called.
///
/// Uses `u128` arithmetic internally to avoid overflow for large deltas.
#[inline]
pub fn tsc_to_ns(tsc_delta: u64) -> u64 {
    let k = TSC_KHZ.load(Ordering::Relaxed);
    if k == 0 { return 0; }
    // tsc_delta ticks × 1_000_000 ns/kHz ÷ khz ticks/ms
    // = tsc_delta × 10^6 / k   (result in ns)
    ((tsc_delta as u128) * 1_000_000 / k as u128) as u64
}

// ── Pure helpers (testable without hardware) ──────────────────────────────────

/// Compute TSC kHz from a measured `tsc_delta` over a `duration_ms` window.
///
/// `duration_ms` must be non-zero; returns 0 otherwise.
///
/// This is the mathematical core of `calibrate()`, extracted so it can be
/// unit-tested without MMIO access.
#[inline]
pub fn compute_khz(tsc_delta: u64, duration_ms: u64) -> u64 {
    if duration_ms == 0 { return 0; }
    tsc_delta / duration_ms
}

/// Compute the number of HPET ticks that represent `duration_ms` milliseconds.
///
/// Returns 0 if `period_fs` is zero.
#[inline]
pub fn hpet_ticks_per_ms(duration_ms: u64, period_fs: u64) -> u64 {
    if period_fs == 0 { return 0; }
    // 1 ms = 10^12 fs.
    (duration_ms * 1_000_000_000_000u64) / period_fs
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_khz_basic() {
        // 3 GHz TSC measured over 10 ms → 300_000 kHz.
        let tsc_delta = 30_000_000u64; // 3 GHz × 10 ms = 30 M ticks
        assert_eq!(compute_khz(tsc_delta, 10), 3_000_000);
    }

    #[test]
    fn test_compute_khz_zero_duration() {
        assert_eq!(compute_khz(1_000_000, 0), 0);
    }

    #[test]
    fn test_compute_khz_1ghz() {
        // 1 GHz over 10 ms → 100_000 kHz.
        let tsc_delta = 10_000_000u64;
        assert_eq!(compute_khz(tsc_delta, 10), 1_000_000);
    }

    #[test]
    fn test_hpet_ticks_per_ms_10mhz() {
        // QEMU HPET: period = 100_000 fs (10 MHz).
        // 1 ms = 10^12 fs → 10^12 / 100_000 = 10_000 ticks.
        assert_eq!(hpet_ticks_per_ms(1, 100_000), 10_000);
    }

    #[test]
    fn test_hpet_ticks_per_ms_10ms_window() {
        // 10 ms at 10 MHz → 100_000 ticks.
        assert_eq!(hpet_ticks_per_ms(10, 100_000), 100_000);
    }

    #[test]
    fn test_hpet_ticks_per_ms_14mhz() {
        // 14.318 MHz: period ≈ 69_841 fs.
        // 10 ms = 10^13 fs → 10^13 / 69_841 ≈ 143_182 ticks.
        let ticks = hpet_ticks_per_ms(10, 69_841);
        // Allow ±2 for integer division rounding.
        assert!((143_180..=143_184).contains(&ticks),
            "expected ~143182, got {ticks}");
    }

    #[test]
    fn test_hpet_ticks_per_ms_zero_period() {
        assert_eq!(hpet_ticks_per_ms(10, 0), 0);
    }

    #[test]
    fn test_tsc_to_ns_zero_before_calibration() {
        TSC_KHZ.store(0, Ordering::Relaxed);
        assert_eq!(tsc_to_ns(1_000_000), 0);
    }

    #[test]
    fn test_tsc_to_ns_3ghz() {
        // TSC at 3 GHz = 3_000_000 kHz.
        // 3_000_000 ticks should be 1_000_000 ns = 1 ms.
        TSC_KHZ.store(3_000_000, Ordering::Relaxed);
        let ns = tsc_to_ns(3_000_000);
        assert_eq!(ns, 1_000_000, "3M ticks at 3GHz must be 1ms");
        // Restore to zero so other tests aren't affected.
        TSC_KHZ.store(0, Ordering::Relaxed);
    }
}
