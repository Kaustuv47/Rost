/// iB700 hardware watchdog driver.
///
/// The iB700 board watchdog is the standard QEMU software watchdog device
/// (`-device ib700`).  It uses two legacy I/O ports:
///
/// | Port  | Direction | Function |
/// |-------|-----------|----------|
/// | 0x441 | write     | Enable + set timeout (byte value = timeout index) |
/// | 0x443 | write     | Disable (write any value; pet also resets timer) |
///
/// # Timeout encoding (Linux ib700wdt.c table)
/// | Written value | Timeout (seconds) |
/// |---------------|-------------------|
/// | 0x00          | 30 s              |
/// | 0x01          | 28 s              |
/// | …             | …                 |
/// | 0x0E          |  2 s              |
/// | 0x0F          |  0 s (disabled)   |
///
/// Formula: timeout_sec ≈ 30 − 2 × value.
///
/// # Usage
/// ```rust
/// hal::watchdog::init(10); // 10-second timeout
/// // … in idle loop …
/// hal::watchdog::kick();   // pet every ~500 ms (every 50 ticks at 100 Hz)
/// ```
///
/// # IEC 61508 §7.4.9 — Watchdog requirement
/// A hardware watchdog is mandatory for SIL-3/4 software: it guarantees that
/// a hung scheduler, ISR, or kernel lock-up is detected within one timeout
/// period and the system is reset to a defined safe state (cold boot).
///
/// # QEMU command line
/// Add `-device ib700,id=watchdog0 -watchdog-action reset` to `scripts/run.sh`.

// ── Port I/O helpers (duplicated from uart.rs to keep hal modules independent) ─

#[inline]
unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nostack));
}

// ── iB700 register addresses ──────────────────────────────────────────────────

/// Enable register: write to arm the watchdog and set the timeout index.
const WDT_ENABLE: u16 = 0x441;
/// Disable register: write any value to disarm the watchdog.
const WDT_DISABLE: u16 = 0x443;

/// Timeout index table: value → seconds.
/// Index 0 = 30 s, index 14 = 2 s, index 15 = 0 s (off).
const TIMEOUT_TABLE: [u8; 16] = [30, 28, 26, 24, 22, 20, 18, 16, 14, 12, 10, 8, 6, 4, 2, 0];

/// Convert a desired timeout in seconds to the closest iB700 index (rounds up
/// to the next longer timeout for safety — never underestimates).
fn timeout_index(desired_sec: u8) -> u8 {
    // Find the smallest index whose mapped timeout >= desired_sec.
    for (idx, &t) in TIMEOUT_TABLE.iter().enumerate() {
        if t >= desired_sec {
            return idx as u8;
        }
    }
    // desired_sec > 30 s: use maximum (index 0 = 30 s).
    0
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Arm the watchdog with the given timeout.
///
/// The system will be reset if [`kick`] is not called within `timeout_sec`
/// seconds.  A safe value for a 100 Hz kernel is 10 seconds (kick every
/// 50 ticks = 500 ms, well inside the 10-second window).
///
/// Must be called after `ExitBootServices()` (port I/O is always available in
/// ring-0 regardless of UEFI boot services state).
pub fn init(timeout_sec: u8) {
    let idx = timeout_index(timeout_sec);
    unsafe { outb(WDT_ENABLE, idx); }
}

/// Pet (reset) the watchdog countdown timer.
///
/// Call this from the idle process every N ticks where N × (1 / tick_hz)
/// is well below the armed timeout.  At 100 Hz, calling every 50 ticks
/// gives a 500 ms kick interval — safe against a 2 s or longer timeout.
///
/// **Safety:** Must only be called from ring-0; ring-3 has no I/O privilege.
#[inline]
pub fn kick() {
    // Writing to port 0x441 resets the countdown without changing the timeout.
    unsafe { outb(WDT_ENABLE, 0); }
}

/// Disarm the watchdog (development / test builds only).
///
/// In production (`--features safety-mode`) the watchdog must never be
/// disabled at runtime — the kernel must always be supervised.
pub fn disable() {
    unsafe { outb(WDT_DISABLE, 0); }
}
