/// ACPI MADT (Multiple APIC Description Table) parser.
///
/// Extracts Local APIC and I/O APIC addresses plus IRQ source overrides
/// from the MADT.  Results are stored in fixed-size arrays; no heap is needed.
///
/// Byte-offset reads are used throughout to avoid E0793 (reference to field of
/// packed struct is unaligned).  All offsets match ACPI 6.5 §5.2.12.

// ── Limits ────────────────────────────────────────────────────────────────────

/// Maximum number of Local APIC entries to record.
pub const MAX_LOCAL_APICS:   usize = 32;
/// Maximum number of I/O APIC entries to record.
pub const MAX_IO_APICS:      usize = 8;
/// Maximum number of interrupt source override entries to record.
pub const MAX_IRQ_OVERRIDES: usize = 16;

// ── MADT entry type codes (ACPI 6.5 §5.2.12.x) ───────────────────────────────

const TYPE_LOCAL_APIC:   u8 = 0;
const TYPE_IO_APIC:      u8 = 1;
const TYPE_IRQ_OVERRIDE: u8 = 2;

// ── Field offsets ─────────────────────────────────────────────────────────────
//
// MADT header (44 bytes total = 36-byte SDT header + 8 MADT-specific bytes)
const MADT_OFF_LENGTH:         usize = 4;  // u32 — total table byte count
const MADT_OFF_LOCAL_APIC_ADDR:usize = 36; // u32 — default LAPIC MMIO address
const MADT_OFF_FLAGS:          usize = 40; // u32 — bit 0 = dual-8259 present
const MADT_HEADER_LEN:         usize = 44;

// MADT interrupt controller structure entry header (at start of each entry)
const ENTRY_OFF_TYPE:   usize = 0; // u8
const ENTRY_OFF_LENGTH: usize = 1; // u8

// Type-0 (Local APIC) body offsets (relative to body start = entry base + 2)
const LA_OFF_ACPI_ID: usize = 0; // u8
const LA_OFF_APIC_ID: usize = 1; // u8
const LA_OFF_FLAGS:   usize = 2; // u32

// Type-1 (I/O APIC) body offsets
const IOA_OFF_ID:       usize = 0; // u8
//  IOA_OFF_RESERVED:         1    // u8 (skip)
const IOA_OFF_ADDRESS:  usize = 2; // u32
const IOA_OFF_GSI_BASE: usize = 6; // u32

// Type-2 (IRQ Source Override) body offsets
//  ISO_OFF_BUS:              0    // u8 (always 0 = ISA)
const ISO_OFF_IRQ:   usize = 1; // u8
const ISO_OFF_GSI:   usize = 2; // u32
const ISO_OFF_FLAGS: usize = 6; // u16

// ── Inline read helpers (same as in acpi/mod.rs — reproduced to avoid dep) ───

#[inline]
unsafe fn read_u8(ptr: *const u8) -> u8 { *ptr }

#[inline]
unsafe fn read_u16_le(ptr: *const u8) -> u16 {
    u16::from_le(core::ptr::read_unaligned(ptr as *const u16))
}

#[inline]
unsafe fn read_u32_le(ptr: *const u8) -> u32 {
    u32::from_le(core::ptr::read_unaligned(ptr as *const u32))
}

// ── Public data types ─────────────────────────────────────────────────────────

/// A processor Local APIC found in the MADT.
#[derive(Copy, Clone, Debug)]
pub struct LocalApic {
    /// ACPI processor UID.
    pub acpi_id: u8,
    /// Local APIC hardware ID — used to address IPIs.
    pub apic_id: u8,
    /// True when bit 0 of the flags field is set (processor is enabled).
    pub enabled: bool,
}

/// An I/O APIC found in the MADT.
#[derive(Copy, Clone, Debug)]
pub struct IoApic {
    /// I/O APIC ID.
    pub id: u8,
    /// Physical address of the I/O APIC MMIO registers.
    pub address: u32,
    /// Global System Interrupt base — first GSI handled by this I/O APIC.
    pub gsi_base: u32,
}

/// An interrupt source override — remaps a legacy ISA IRQ to a GSI.
#[derive(Copy, Clone, Debug)]
pub struct IrqOverride {
    /// Source IRQ number (ISA bus, 0–15).
    pub irq: u8,
    /// Target Global System Interrupt.
    pub gsi: u32,
    /// MPS INTI flags: polarity in bits[1:0], trigger mode in bits[3:2].
    pub flags: u16,
}

/// All APIC information extracted from the MADT.
#[derive(Debug)]
pub struct MadtInfo {
    /// Physical address of the Local APIC MMIO registers (default).
    pub local_apic_addr:    u32,
    /// Bit 0: dual-8259 PICs present — must be masked before APIC activation.
    pub flags:              u32,

    pub local_apics:        [Option<LocalApic>;   MAX_LOCAL_APICS],
    pub local_apic_count:   usize,

    pub io_apics:           [Option<IoApic>;      MAX_IO_APICS],
    pub io_apic_count:      usize,

    pub irq_overrides:      [Option<IrqOverride>; MAX_IRQ_OVERRIDES],
    pub irq_override_count: usize,
}

impl MadtInfo {
    pub const fn empty() -> Self {
        MadtInfo {
            local_apic_addr:    0,
            flags:              0,
            local_apics:        [None; MAX_LOCAL_APICS],
            local_apic_count:   0,
            io_apics:           [None; MAX_IO_APICS],
            io_apic_count:      0,
            irq_overrides:      [None; MAX_IRQ_OVERRIDES],
            irq_override_count: 0,
        }
    }

    /// Return the first enabled Local APIC (usually the BSP's APIC).
    pub fn bsp_local_apic(&self) -> Option<LocalApic> {
        self.local_apics[..self.local_apic_count]
            .iter()
            .filter_map(|o| *o)
            .find(|la| la.enabled)
    }

    /// Return the first I/O APIC, if any.
    pub fn primary_io_apic(&self) -> Option<IoApic> {
        self.io_apics[..self.io_apic_count]
            .iter()
            .filter_map(|o| *o)
            .next()
    }

    /// Resolve a legacy ISA IRQ to its Global System Interrupt number.
    ///
    /// Returns the IRQ unchanged (identity) when no override is present.
    pub fn irq_to_gsi(&self, irq: u8) -> u32 {
        for entry in &self.irq_overrides[..self.irq_override_count] {
            if let Some(ov) = entry {
                if ov.irq == irq { return ov.gsi; }
            }
        }
        irq as u32
    }
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse the MADT at `madt_phys` (physical = identity-mapped virtual address).
///
/// Returns `None` if the table signature, checksum, or minimum length check
/// fails.  Unknown entry types are silently skipped — forward compatibility.
///
/// # Safety
/// `madt_phys` must point to readable ACPI table memory that is identity-mapped
/// into the kernel's virtual address space.
pub fn parse_madt(madt_phys: u64) -> Option<MadtInfo> {
    if madt_phys == 0 { return None; }

    let base = madt_phys as *const u8;

    // Signature check ("APIC").
    let sig = unsafe { core::slice::from_raw_parts(base, 4) };
    if sig != b"APIC" { return None; }

    let total_len = unsafe { read_u32_le(base.add(MADT_OFF_LENGTH)) } as usize;
    if total_len < MADT_HEADER_LEN { return None; }

    // Full-table checksum.
    let mut sum: u8 = 0;
    for i in 0..total_len {
        sum = sum.wrapping_add(unsafe { *base.add(i) });
    }
    if sum != 0 { return None; }

    let mut info = MadtInfo::empty();
    info.local_apic_addr = unsafe { read_u32_le(base.add(MADT_OFF_LOCAL_APIC_ADDR)) };
    info.flags           = unsafe { read_u32_le(base.add(MADT_OFF_FLAGS)) };

    // Walk the variable-length entry array following the MADT header.
    let mut offset = MADT_HEADER_LEN;
    while offset + 2 <= total_len {
        let entry_base = madt_phys as usize + offset;
        let eptr       = entry_base as *const u8;
        let entry_type = unsafe { read_u8(eptr.add(ENTRY_OFF_TYPE)) };
        let entry_len  = unsafe { read_u8(eptr.add(ENTRY_OFF_LENGTH)) } as usize;

        if entry_len < 2 { break; } // malformed — protect against infinite loop
        if offset + entry_len > total_len { break; }

        let body = unsafe { eptr.add(2) }; // body starts immediately after the 2-byte header

        match entry_type {
            TYPE_LOCAL_APIC if entry_len >= 2 + 6 => {
                let flags = unsafe { read_u32_le(body.add(LA_OFF_FLAGS)) };
                if info.local_apic_count < MAX_LOCAL_APICS {
                    info.local_apics[info.local_apic_count] = Some(LocalApic {
                        acpi_id: unsafe { read_u8(body.add(LA_OFF_ACPI_ID)) },
                        apic_id: unsafe { read_u8(body.add(LA_OFF_APIC_ID)) },
                        enabled: flags & 1 != 0,
                    });
                    info.local_apic_count += 1;
                }
            }
            TYPE_IO_APIC if entry_len >= 2 + 10 => {
                if info.io_apic_count < MAX_IO_APICS {
                    info.io_apics[info.io_apic_count] = Some(IoApic {
                        id:       unsafe { read_u8(body.add(IOA_OFF_ID)) },
                        address:  unsafe { read_u32_le(body.add(IOA_OFF_ADDRESS)) },
                        gsi_base: unsafe { read_u32_le(body.add(IOA_OFF_GSI_BASE)) },
                    });
                    info.io_apic_count += 1;
                }
            }
            TYPE_IRQ_OVERRIDE if entry_len >= 2 + 8 => {
                if info.irq_override_count < MAX_IRQ_OVERRIDES {
                    info.irq_overrides[info.irq_override_count] = Some(IrqOverride {
                        irq:   unsafe { read_u8(body.add(ISO_OFF_IRQ)) },
                        gsi:   unsafe { read_u32_le(body.add(ISO_OFF_GSI)) },
                        flags: unsafe { read_u16_le(body.add(ISO_OFF_FLAGS)) },
                    });
                    info.irq_override_count += 1;
                }
            }
            _ => {} // unknown / oversized entry: skip
        }

        offset += entry_len;
    }

    Some(info)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct a minimal, valid MADT byte buffer in host memory.
    ///
    /// Includes: 1 enabled Local APIC, 1 I/O APIC, 1 IRQ source override.
    fn build_test_madt() -> Vec<u8> {
        // Build the variable-length entry section first.
        let mut entries: Vec<u8> = Vec::new();

        // Type 0 — Local APIC: acpi_id=1, apic_id=0, flags=1 (enabled).
        entries.extend_from_slice(&[
            0, 8,           // type=0, length=8
            1, 0,           // acpi_id=1, apic_id=0
            1, 0, 0, 0,     // flags=1 (enabled), LE
        ]);

        // Type 1 — I/O APIC: id=0, address=0xFEC00000, gsi_base=0.
        entries.extend_from_slice(&[
            1, 12,                          // type=1, length=12
            0, 0,                           // id=0, reserved
            0x00, 0x00, 0xC0, 0xFE,         // address=0xFEC0_0000, LE
            0, 0, 0, 0,                     // gsi_base=0
        ]);

        // Type 2 — IRQ override: bus=0 (ISA), irq=0 → gsi=2, flags=0.
        entries.extend_from_slice(&[
            2, 10,                          // type=2, length=10
            0, 0,                           // bus=0, irq=0
            2, 0, 0, 0,                     // gsi=2, LE
            0, 0,                           // flags=0
        ]);

        let total = (MADT_HEADER_LEN + entries.len()) as u32;

        // 44-byte MADT header.
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(b"APIC");                         // offset  0: signature
        buf.extend_from_slice(&total.to_le_bytes());            // offset  4: length
        buf.push(3);                                            // offset  8: revision
        buf.push(0);                                            // offset  9: checksum (placeholder)
        buf.extend_from_slice(b"ROST  ");                       // offset 10: OEM ID (6)
        buf.extend_from_slice(b"TESTMADT");                     // offset 16: OEM table (8)
        buf.extend_from_slice(&0u32.to_le_bytes());             // offset 24: OEM rev
        buf.extend_from_slice(&0u32.to_le_bytes());             // offset 28: creator ID
        buf.extend_from_slice(&0u32.to_le_bytes());             // offset 32: creator rev
        buf.extend_from_slice(&0xFEE0_0000u32.to_le_bytes());   // offset 36: LAPIC addr
        buf.extend_from_slice(&0x0000_0001u32.to_le_bytes());   // offset 40: flags (PIC present)
        buf.extend_from_slice(&entries);

        // Fix checksum: byte sum over all bytes must be 0 mod 256.
        let sum: u8 = buf.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        buf[9] = (0u8).wrapping_sub(sum);

        buf
    }

    /// All three entry types are parsed correctly.
    #[test]
    fn test_parse_madt_all_entry_types() {
        let buf  = build_test_madt();
        let info = parse_madt(buf.as_ptr() as u64).expect("valid MADT must parse");

        // Header fields.
        assert_eq!(info.local_apic_addr, 0xFEE0_0000);
        assert_eq!(info.flags, 1, "dual-8259 flag must be set");

        // Local APIC.
        assert_eq!(info.local_apic_count, 1);
        let la = info.local_apics[0].unwrap();
        assert_eq!(la.acpi_id, 1);
        assert_eq!(la.apic_id, 0);
        assert!(la.enabled);

        // I/O APIC.
        assert_eq!(info.io_apic_count, 1);
        let ioa = info.io_apics[0].unwrap();
        assert_eq!(ioa.id, 0);
        assert_eq!(ioa.address, 0xFEC0_0000);
        assert_eq!(ioa.gsi_base, 0);

        // IRQ override: IRQ 0 → GSI 2.
        assert_eq!(info.irq_override_count, 1);
        let ov = info.irq_overrides[0].unwrap();
        assert_eq!(ov.irq, 0);
        assert_eq!(ov.gsi, 2);
        assert_eq!(ov.flags, 0);

        // Accessor helpers.
        assert!(info.bsp_local_apic().is_some());
        assert!(info.primary_io_apic().is_some());
        assert_eq!(info.irq_to_gsi(0), 2, "IRQ 0 must map to GSI 2");
        assert_eq!(info.irq_to_gsi(1), 1, "IRQ 1 must be identity (no override)");
    }

    /// parse_madt returns None for a null address.
    #[test]
    fn test_parse_madt_null() {
        assert!(parse_madt(0).is_none());
    }

    /// parse_madt returns None when the checksum byte is wrong.
    #[test]
    fn test_parse_madt_bad_checksum() {
        let mut buf = build_test_madt();
        buf[9] = buf[9].wrapping_add(1);
        assert!(parse_madt(buf.as_ptr() as u64).is_none());
    }

    /// parse_madt returns None when the 4-byte signature is wrong.
    #[test]
    fn test_parse_madt_bad_signature() {
        let mut buf = build_test_madt();
        buf[0] = b'X'; // "APIC" → "XPIC"
        // Re-compute checksum.
        buf[9] = 0;
        let sum: u8 = buf.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        buf[9] = (0u8).wrapping_sub(sum);
        assert!(parse_madt(buf.as_ptr() as u64).is_none());
    }

    /// irq_to_gsi returns identity (IRQ == GSI) when no overrides exist.
    #[test]
    fn test_irq_to_gsi_identity() {
        let info = MadtInfo::empty();
        for irq in 0..16u8 {
            assert_eq!(info.irq_to_gsi(irq), irq as u32);
        }
    }

    /// An entry whose body is shorter than the minimum is silently skipped.
    #[test]
    fn test_truncated_entry_skipped() {
        let mut buf = build_test_madt();

        // Overwrite the Type-0 entry's length with 3 (too short for a 6-byte body).
        // The entry starts at offset MADT_HEADER_LEN (44).
        buf[MADT_HEADER_LEN + 1] = 3; // length byte of first entry

        // Re-fix checksum.
        buf[9] = 0;
        let sum: u8 = buf.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        buf[9] = (0u8).wrapping_sub(sum);

        // Should still parse without panic; the truncated Local APIC is skipped.
        let info = parse_madt(buf.as_ptr() as u64).expect("should parse despite truncated entry");
        // The Local APIC entry is skipped; I/O APIC + override may still be present
        // depending on how far the walker gets.
        assert_eq!(info.local_apic_count, 0, "truncated Local APIC entry must be skipped");
    }
}
