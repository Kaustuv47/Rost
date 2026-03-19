/// I/O APIC initialisation and IRQ routing.
///
/// The I/O APIC is accessed through two 32-bit MMIO registers:
/// ```text
///   base+0x00  IOREGSEL — write the index of the I/O APIC register to access
///   base+0x10  IOWIN    — read/write the selected register
/// ```
///
/// Indirect register map:
/// ```text
///   0x00  IOAPICID  — I/O APIC ID
///   0x01  IOAPICVER — bits[23:16] = maximum redirect entry (MRE), bits[7:0] = version
///   0x10 + 2*n  Redirection table entry n, low  32-bit word
///   0x11 + 2*n  Redirection table entry n, high 32-bit word
/// ```
///
/// Redirect entry (64-bit):
/// ```text
///   bits[7:0]   IDT vector
///   bits[10:8]  Delivery mode (000 = Fixed)
///   bit[11]     Destination mode (0 = physical)
///   bit[12]     Delivery status (RO)
///   bit[13]     Pin polarity   (0 = active high)
///   bit[14]     Remote IRR     (RO)
///   bit[15]     Trigger mode   (0 = edge)
///   bit[16]     Mask           (1 = masked)
///   bits[63:56] Destination (physical: Local APIC ID)
/// ```

// ── MMIO helpers ─────────────────────────────────────────────────────────────

/// Write to the I/O APIC indirect register at `reg`.
///
/// # Safety
/// `base` must be a valid, mapped I/O APIC MMIO base address.
#[inline]
unsafe fn write(base: u64, reg: u8, val: u32) {
    let ioregsel = base as *mut u32;
    let iowin    = (base + 0x10) as *mut u32;
    ioregsel.write_volatile(reg as u32);
    iowin.write_volatile(val);
}

/// Read from the I/O APIC indirect register at `reg`.
///
/// # Safety
/// `base` must be a valid, mapped I/O APIC MMIO base address.
#[inline]
unsafe fn read(base: u64, reg: u8) -> u32 {
    let ioregsel = base as *mut u32;
    let iowin    = (base + 0x10) as *const u32;
    ioregsel.write_volatile(reg as u32);
    iowin.read_volatile()
}

// ── Redirection-entry encoding (pure, unit-testable) ─────────────────────────

/// Encode a 64-bit redirection table entry.
///
/// The entry is unmasked, edge-triggered, fixed delivery to `apic_id`,
/// delivering `vector`.
#[inline]
pub fn redirect_entry(vector: u8, apic_id: u8) -> u64 {
    let low  = (vector as u64) & 0xFF; // fixed delivery + edge + active-high + unmasked
    let high = (apic_id as u64) << 56;
    high | low
}

/// Encode a masked 64-bit redirection table entry (bit 16 set).
#[inline]
pub fn masked_entry() -> u64 {
    1u64 << 16
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the I/O APIC at `ioapic_base`.
///
/// Reads the maximum redirect entry count from IOAPICVER and masks every
/// redirection table entry.  With the LAPIC timer serving as the schedule
/// clock and no other hardware needing IRQs (UART is polled), masking all
/// entries is the correct safe initial state.
///
/// Individual entries can be unmasked later with `route_irq()`.
/// Safe to call with `ioapic_base == 0`; in that case init is skipped.
pub fn init(ioapic_base: u64) {
    if ioapic_base == 0 {
        hal::uart::print_str("      [WARN] I/O APIC base unknown — skipping\n");
        return;
    }

    let (version, max_entry) = unsafe {
        let ver = read(ioapic_base, 0x01);
        let max = ((ver >> 16) & 0xFF) as u8; // bits[23:16] = MRE
        let v   = ver & 0xFF;                 // bits[7:0]   = version byte
        (v, max)
    };

    // Mask all redirect entries.
    unsafe {
        for n in 0..=(max_entry as u32) {
            let reg_lo = 0x10 + 2 * n;
            let reg_hi = reg_lo + 1;
            write(ioapic_base, reg_lo as u8, 0x0001_0000); // mask bit
            write(ioapic_base, reg_hi as u8, 0);
        }
    }

    hal::uart::print_str("      ├─ I/O APIC:        ver=0x");
    hal::uart::print_hex(version as u64);
    hal::uart::print_str("  entries=");
    hal::uart::print_dec(max_entry as u64 + 1);
    hal::uart::print_str("  all masked\n");
}

/// Route `gsi` (global system interrupt) to `vector` on CPU with `apic_id`.
///
/// - Edge-triggered, active-high, fixed delivery, unmasked.
/// - `gsi` is the I/O APIC input pin number (= IRQ + `gsi_base` from MADT).
/// - Call after `init()` to expose specific hardware interrupts.
///
/// # Safety
/// `ioapic_base` must be a valid I/O APIC MMIO address; `gsi` must be
/// within the range reported by IOAPICVER (0 through `max_entry`).
pub unsafe fn route_irq(ioapic_base: u64, gsi: u8, vector: u8, apic_id: u8) {
    if ioapic_base == 0 { return; }
    let entry = redirect_entry(vector, apic_id);
    let reg_lo = 0x10 + 2 * (gsi as u32);
    let reg_hi = reg_lo + 1;
    write(ioapic_base, reg_hi as u8, (entry >> 32) as u32);
    write(ioapic_base, reg_lo as u8, (entry & 0xFFFF_FFFF) as u32);
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// redirect_entry places the vector in bits[7:0].
    #[test]
    fn test_redirect_entry_vector() {
        let e = redirect_entry(32, 0);
        assert_eq!(e & 0xFF, 32, "vector in bits[7:0]");
    }

    /// redirect_entry places the destination APIC ID in bits[63:56].
    #[test]
    fn test_redirect_entry_apic_id() {
        let e = redirect_entry(32, 5);
        assert_eq!((e >> 56) & 0xFF, 5, "dest apic_id in bits[63:56]");
    }

    /// A fresh redirect_entry has the mask bit clear (bit 16 = 0).
    #[test]
    fn test_redirect_entry_unmasked() {
        let e = redirect_entry(32, 0);
        assert_eq!(e & (1 << 16), 0, "new entry must be unmasked");
    }

    /// masked_entry has bit 16 set and vector/destination zero.
    #[test]
    fn test_masked_entry() {
        let e = masked_entry();
        assert_ne!(e & (1 << 16), 0, "masked entry must have mask bit set");
        assert_eq!(e & 0xFF, 0, "vector field must be zero");
    }
}
