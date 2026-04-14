# Chapter 11 — Timer Subsystem

## 11.1 Why the Timer Matters

The timer is the kernel's heartbeat.  Every 10 milliseconds (100 Hz), the timer
ISR fires, and the scheduler has a chance to:
- Preempt the current process if its quantum has expired
- Unblock processes whose IPC timeout has elapsed
- Advance the monotonic clock
- Pet the hardware watchdog

Without a reliable timer, the scheduler degenerates to cooperative multitasking —
processes only yield voluntarily, and a misbehaving process can starve all others.

## 11.2 Timer Architecture: PIT → LAPIC

Rost's timer subsystem went through two generations:

**Generation 1 (boot)**: The Intel 8253/8254 Programmable Interval Timer (PIT)
is initialized at 100 Hz and routed through the 8259 PIC.  This is the universal
PC timer — every x86 system has it.

**Generation 2 (production)**: The Local APIC timer replaces the PIT after the
LAPIC is initialized.  The LAPIC timer is per-CPU (every CPU core has its own),
higher resolution, and not shared with the OS's interrupts.

The transition happens in Stage 3:
1. PIT is initialized at 100 Hz, 8259 PIC is configured
2. LAPIC is enabled via the LAPIC spurious vector register
3. PIT channel 2 is used to calibrate the LAPIC timer (10 ms calibration window)
4. LAPIC timer is set to periodic mode at 100 Hz
5. 8259 PIC is fully masked (all IRQs blocked through the PIC)

After Stage 3, all timer interrupts come from the LAPIC.

## 11.3 PIT Initialization

```rust
pub fn init_pit_100hz() {
    // PIT channel 0, mode 2 (rate generator), binary, divisor 11931
    // 1,193,182 Hz / 11931 ≈ 99.97 Hz ≈ 100 Hz
    out_byte(0x43, 0x34);  // command: ch0, lo/hi byte, mode 2, binary
    out_byte(0x40, 0x9B);  // divisor low byte: 11931 & 0xFF = 0x9B
    out_byte(0x40, 0x2E);  // divisor high byte: 11931 >> 8 = 0x2E
}
```

## 11.4 LAPIC Timer Initialization

```rust
pub fn init_lapic(lapic_base: u64) {
    // 1. Map the LAPIC MMIO page (if not already mapped)
    map_page_global(KERNEL_PML4, lapic_base, lapic_base, PRESENT | WRITABLE);

    // 2. Enable the LAPIC via the Spurious Vector Register (SVR)
    // Bit 8 = software enable; bits [7:0] = spurious vector (255)
    lapic_write(SVR, lapic_read(SVR) | 0x100 | 0xFF);

    // 3. Set Task Priority Register to 0 (accept all interrupts)
    lapic_write(TPR, 0);

    // 4. Mask LVT entries (LINT0, LINT1, Error, PMI, Thermal)
    lapic_write(LVT_LINT0, 0x10000);   // masked
    lapic_write(LVT_LINT1, 0x10000);   // masked
    lapic_write(LVT_ERROR, 0x10000);   // masked

    // 5. Calibrate: count LAPIC ticks in a 10 ms PIT window
    let ticks_per_10ms = calibrate_lapic_via_pit();

    // 6. Set LAPIC timer to periodic mode, vector 32, every ticks_per_10ms ticks
    lapic_write(LVT_TIMER, 0x20000 | 32); // periodic mode, vector 32
    lapic_write(DIVIDE_CONFIG, 0x03);      // divide-by-16
    lapic_write(INITIAL_COUNT, ticks_per_10ms);

    // 7. Mask all 8259 PIC lines
    out_byte(0xA1, 0xFF);  // mask slave PIC
    out_byte(0x21, 0xFF);  // mask master PIC
}
```

### 11.4.1 PIT Calibration

The calibration procedure measures how many LAPIC timer ticks correspond to one
10 ms PIT interval:

```rust
fn calibrate_lapic_via_pit() -> u32 {
    // Configure PIT channel 2 for one-shot mode, 10 ms
    out_byte(0x61, (in_byte(0x61) & 0xFD) | 0x01);  // gate on, speaker off
    out_byte(0x43, 0xB2);                             // ch2, lo/hi, mode 1
    out_byte(0x42, 0x9B);                             // 11931 = 10 ms at 1.19 MHz
    out_byte(0x42, 0x2E);

    // Start counting
    let t0 = out_byte(0x61, in_byte(0x61) | 0x01);
    lapic_write(INITIAL_COUNT, 0xFFFFFFFF);  // max count, count down

    // Wait for PIT channel 2 to reach 0 (bit 5 of port 0x61)
    while in_byte(0x61) & 0x20 == 0 {}

    // Read remaining count
    let remaining = lapic_read(CURRENT_COUNT);
    0xFFFFFFFF - remaining  // ticks elapsed in 10 ms
}
```

This is one of the few places in the kernel with an unbounded spin loop.
It is acceptable because:
1. It runs exactly once at boot
2. PIT channel 2 completes in exactly 10 ms
3. The hardware guarantees completion; no software condition could prevent it

## 11.5 The LAPIC One-Shot Timer

For per-process deadline wakeups, Rost uses the LAPIC in one-shot mode:

```rust
pub fn arm_oneshot(n: u64) {
    let icr = LAPIC_ICR_PER_TICK.load(Ordering::Relaxed);
    if icr == 0 { return; }  // LAPIC not initialized
    lapic_write(INITIAL_COUNT, (icr * n) as u32);
}
```

`LAPIC_ICR_PER_TICK` is the LAPIC ticks per scheduler tick (= ticks_per_10ms,
since the periodic timer fires every 10 ms = 1 tick).

`arm_oneshot(n)` programs the LAPIC to fire in `n` ticks.  After `tick_scheduler_isr`
runs, it calls `arm_oneshot(ticks_until_next_event())` to program the LAPIC to fire
exactly when the soonest blocked deadline will expire:

```rust
pub fn ticks_until_next_event(&self) -> u64 {
    let tick = *self.tick.borrow();
    let earliest = self.process_table.borrow().earliest_blocked_deadline();
    if earliest == u64::MAX { return 1; }  // no blocked processes; next tick
    earliest.saturating_sub(tick).max(1)
}
```

This avoids redundant timer ISRs when no process will be unblocked for several
ticks — instead of firing at 100 Hz with no work to do, the LAPIC is programmed
to fire exactly when needed.

## 11.6 HPET — High Precision Event Timer

The HPET provides a monotonic counter running at a fixed rate (typically 10–14 MHz).
Rost uses it as a reference clock for TSC calibration.

```rust
pub fn init_hpet(base: u64) {
    // Map the HPET MMIO page
    map_page_global(KERNEL_PML4, base, base, PRESENT | WRITABLE);

    // Read the counter period (in femtoseconds)
    let cap_id = hpet_read_u64(base, 0x00);
    let period_fs = (cap_id >> 32) as u32;  // bits[63:32]

    // Enable the HPET (set ENABLE_CNF in the config register)
    let cfg = hpet_read_u64(base, 0x10);
    hpet_write_u64(base, 0x10, cfg | 1);

    HPET_BASE.store(base, Ordering::Relaxed);
    HPET_PERIOD_FS.store(period_fs as u64, Ordering::Relaxed);
}

pub fn read_counter() -> u64 {
    let base = HPET_BASE.load(Ordering::Relaxed);
    if base == 0 { return 0; }
    unsafe { *(base as *const u64).add(0x1E0 / 8) }  // Main Counter Register
}
```

## 11.7 TSC Calibration

The Time Stamp Counter (TSC) is the CPU's built-in cycle counter.  It is the
fastest way to read time (one `rdtsc` instruction, ~1 cycle), but it runs at
the CPU's nominal frequency which varies between machines.  We calibrate it
against the HPET:

```rust
pub fn calibrate(hpet_base: u64, period_fs: u64) -> u64 {
    // 10 ms calibration window in HPET ticks
    let hpet_ticks_per_10ms = 10_000_000_000_000u64 / period_fs;  // 10ms in fs / period

    let tsc0 = rdtsc();
    let hpet0 = read_hpet_counter(hpet_base);

    // Busy-wait for 10 ms
    let target = hpet0 + hpet_ticks_per_10ms;
    while read_hpet_counter(hpet_base) < target {}

    let tsc1 = rdtsc();
    let tsc_delta = tsc1 - tsc0;

    // TSC kHz = delta / 10 (delta ticks in 10 ms)
    let khz = tsc_delta / 10;
    TSC_KHZ.store(khz, Ordering::Relaxed);
    khz
}
```

After calibration, nanosecond conversion is:
```rust
pub fn tsc_to_ns(delta: u64) -> u64 {
    // delta / (TSC_KHZ / 1_000_000) = delta * 1_000_000 / TSC_KHZ
    delta.saturating_mul(1_000_000) / TSC_KHZ.load(Ordering::Relaxed).max(1)
}
```

## 11.8 The `SYS_CLOCK` Syscall

Ring-3 processes access the monotonic clock via `SYS_CLOCK` (syscall 14):

```rust
SYS_CLOCK => {
    // TICK_COUNT × 10,000,000 nanoseconds (100 Hz → 10 ms per tick)
    TICK_COUNT.load(Ordering::Relaxed) * 10_000_000
}
```

Resolution: 10 ms (100 Hz).  This is sufficient for the shell's `uptime` command,
`sleep`, and IPC timeouts.  For sub-millisecond measurements, the TSC could be
exposed via a future `SYS_TSC_READ` syscall.

## 11.9 The Hardware Watchdog

Rost uses the iB700 watchdog device:

```rust
// crates/hal/src/watchdog.rs

const WDOG_ENABLE: u16  = 0x441;  // write 0x01 to enable, 0x00 to disable
const WDOG_DISABLE: u16 = 0x443;

/// Arm the watchdog with a timeout (seconds).
pub fn init(timeout_secs: u32) {
    let idx = timeout_index(timeout_secs);
    out_byte(WDOG_ENABLE, idx as u8);
}

/// Pet (reset) the watchdog — must be called before it expires.
pub fn kick() {
    out_byte(WDOG_ENABLE, 0x01);  // writing anything to WDOG_ENABLE resets the timer
}
```

The idle process pets the watchdog every 50 ticks (500 ms):

```rust
loop {
    arch_x86_64::cpu::halt();
    tick_count += 1;
    if tick_count % 50 == 0 {
        hal::watchdog::kick();  // IEC 61508 §7.4.9
    }
}
```

QEMU is configured with `-device ib700,id=watchdog0 -watchdog-action reset`.
If the idle process stops running (scheduler bug, interrupt storm), the watchdog
fires after 10 seconds and resets the machine.

IEC 61508 §7.4.9: hardware watchdog supervision is mandatory for SIL-4 systems.

## 11.10 Summary

The timer subsystem provides:

- **100 Hz scheduler tick** — LAPIC periodic timer, preemptive scheduling
- **LAPIC one-shot optimization** — fires only when a deadline is imminent
- **HPET monotonic clock** — fixed-frequency reference for TSC calibration
- **TSC calibration** — nanosecond-resolution timestamps without syscall overhead
- **`SYS_CLOCK`** — ring-3 access to 10 ms-resolution monotonic time
- **Hardware watchdog** — 10-second timeout, reset on expiry (idle process keeps alive)
- **IEC 61508 compliance** — watchdog supervision, all busy-waits bounded
