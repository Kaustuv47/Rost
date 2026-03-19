/// ACPI FADT (Fixed ACPI Description Table — signature "FACP") parser.
///
/// Extracts the PM timer I/O port and the ACPI reset register from the FADT.
/// These are the two FADT fields the Rost kernel currently uses:
///
///   - `pm_timer_port`  — 32-bit I/O port for the ACPI PM timer (3.579545 MHz).
///     Useful for TSC calibration and as a fallback monotonic clock.
///   - `reset_reg` / `reset_value` — Generic Address Structure specifying the
///     register to write to perform a software-initiated system reset.
///
/// All field accesses use byte-offset constants to avoid E0793 (unaligned
/// reference into packed struct).  Offsets match ACPI 6.5 §5.2.9.
///
/// IEC 61508 §7.4.9: a software-accessible reset path allows the safety
/// monitor to force a transition to the safe state (power-off / warm reset)
/// without relying on the hardware watchdog alone.

use super::{checksum_valid, read_u8, read_u16, read_u32, read_u64};

// ── FADT field offsets (ACPI 6.5 §5.2.9) ─────────────────────────────────────

const FADT_OFF_SIGNATURE:  usize = 0;   // [u8; 4] = "FACP"
const FADT_OFF_LENGTH:     usize = 4;   // u32 — total table byte count
const FADT_OFF_REVISION:   usize = 8;   // u8  — 1 = ACPI 1.0, 2+ = ACPI 2.0+
// (checksum at byte 9 — validated via checksum_valid over entire table)

// v1 fields (present in all revisions)
const FADT_OFF_PM_TMR_BLK: usize = 76;  // u32 — ACPI PM timer I/O port
const FADT_OFF_PM_TMR_LEN: usize = 91;  // u8  — must be 4 for a 32-bit PM timer
const FADT_OFF_FLAGS:      usize = 112; // u32 — feature flags

// FLAGS bits
const FLAG_RESET_REG_SUP:  u32 = 1 << 10; // RESET_REG field is valid (ACPI ≥ 2.0)

// RESET_REG — Generic Address Structure (GAS, 12 bytes) at offset 116
const FADT_OFF_RESET_GAS_SPACE:  usize = 116; // u8  — address space id
const FADT_OFF_RESET_GAS_BW:     usize = 117; // u8  — register bit width
const FADT_OFF_RESET_GAS_BOFF:   usize = 118; // u8  — register bit offset
const FADT_OFF_RESET_GAS_SIZE:   usize = 119; // u8  — access size
const FADT_OFF_RESET_GAS_ADDR:   usize = 120; // u64 — address (I/O port or MMIO)
const FADT_OFF_RESET_VALUE:      usize = 128; // u8  — value to write to RESET_REG

// Minimum table lengths (to check before reading extended fields)
const FADT_MIN_LEN_V1:     usize = 116; // through PM_TMR_LEN (byte 91) plus FLAGS
const FADT_MIN_LEN_RESET:  usize = 129; // through RESET_VALUE (byte 128)

// ── Address-space IDs (ACPI Generic Address Structure §5.2.3.2) ───────────────

/// System memory (MMIO) address space.
pub const GAS_SPACE_MEMORY: u8 = 0;
/// System I/O address space (port I/O).
pub const GAS_SPACE_IO:     u8 = 1;

// ── Public types ──────────────────────────────────────────────────────────────

/// Parsed fields from the ACPI FADT that are relevant to the Rost kernel.
///
/// Only fields actually used by the kernel are extracted; the rest of the
/// 276-byte (ACPI 6.5) FADT is intentionally ignored to keep the parser
/// minimal and auditable.
#[derive(Debug, Clone, Copy)]
pub struct FadtInfo {
    /// I/O port of the ACPI PM timer block (3.579545 MHz, 24- or 32-bit counter).
    /// Zero if not reported by firmware.
    pub pm_timer_port: u32,

    /// Length of the PM timer register in bytes.  Must be 4 for a 32-bit timer.
    pub pm_timer_len: u8,

    /// FADT FLAGS word (see ACPI §5.2.9 Table 5-35).
    /// The kernel checks `FLAG_RESET_REG_SUP` (bit 10) before using `reset_addr`.
    pub flags: u32,

    /// Address-space identifier for the reset register (0=MMIO, 1=I/O port).
    pub reset_reg_space: u8,

    /// Physical address (or I/O port number) of the reset register.
    /// Only valid when `flags & FLAG_RESET_REG_SUP != 0`.
    pub reset_reg_addr: u64,

    /// Value to write to `reset_reg_addr` to trigger a system reset.
    pub reset_value: u8,
}

impl FadtInfo {
    /// Returns `true` if the PM timer port is reported and has the expected 4-byte width.
    #[inline]
    pub fn pm_timer_valid(&self) -> bool {
        self.pm_timer_port != 0 && self.pm_timer_len == 4
    }

    /// Returns `true` if the ACPI reset register is present and usable.
    #[inline]
    pub fn reset_supported(&self) -> bool {
        self.flags & FLAG_RESET_REG_SUP != 0 && self.reset_reg_addr != 0
    }
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse the FADT at `fadt_phys` and return a [`FadtInfo`].
///
/// Returns `None` if:
/// - `fadt_phys` is zero
/// - The 4-byte signature is not `"FACP"`
/// - The full-table ACPI checksum fails
/// - The table is too short to contain the minimum required fields
///
/// The reset register fields are only populated when the table is long enough
/// (≥ 129 bytes) and the `RESET_REG_SUP` flag is set, matching ACPI 2.0+
/// firmware behaviour.
///
/// # Safety
/// `fadt_phys` must be a valid physical address of an ACPI table that is
/// identity-mapped into the kernel's virtual address space for its lifetime.
pub fn parse_fadt(fadt_phys: u64) -> Option<FadtInfo> {
    if fadt_phys == 0 { return None; }

    let base = fadt_phys as *const u8;

    // Validate signature "FACP".
    let sig = unsafe { core::slice::from_raw_parts(base.add(FADT_OFF_SIGNATURE), 4) };
    if sig != b"FACP" { return None; }

    // Read declared length and validate full-table checksum.
    let len = unsafe { read_u32(base.add(FADT_OFF_LENGTH)) } as usize;
    if len < FADT_MIN_LEN_V1 { return None; }
    if !checksum_valid(base, len) { return None; }

    let revision     = unsafe { read_u8(base.add(FADT_OFF_REVISION)) };
    let pm_timer_port= unsafe { read_u32(base.add(FADT_OFF_PM_TMR_BLK)) };
    let pm_timer_len = unsafe { read_u8(base.add(FADT_OFF_PM_TMR_LEN)) };
    let flags        = unsafe { read_u32(base.add(FADT_OFF_FLAGS)) };

    // Reset register is only defined for ACPI ≥ 2.0 (revision ≥ 2) and only when
    // the table is long enough to contain all RESET_* fields.
    let (reset_reg_space, reset_reg_addr, reset_value) =
        if revision >= 2 && len >= FADT_MIN_LEN_RESET && flags & FLAG_RESET_REG_SUP != 0 {
            let space = unsafe { read_u8(base.add(FADT_OFF_RESET_GAS_SPACE)) };
            let addr  = unsafe { read_u64(base.add(FADT_OFF_RESET_GAS_ADDR)) };
            let val   = unsafe { read_u8(base.add(FADT_OFF_RESET_VALUE)) };
            (space, addr, val)
        } else {
            (GAS_SPACE_IO, 0u64, 0u8)
        };

    // Suppress unused-variable warning for revision in release builds where
    // no further revision-gated fields are decoded.
    let _ = revision;

    Some(FadtInfo {
        pm_timer_port,
        pm_timer_len,
        flags,
        reset_reg_space,
        reset_reg_addr,
        reset_value,
    })
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{alloc_zeroed, dealloc, Layout};

    // ── Minimal FADT helper ───────────────────────────────────────────────────

    struct FadtBuf {
        data: Vec<u8>,
    }

    impl FadtBuf {
        /// Allocate a zeroed FADT buffer of `size` bytes, stamp the signature and
        /// length, then recompute the checksum so it is valid.
        fn new(size: usize) -> Self {
            assert!(size >= FADT_MIN_LEN_V1, "buffer too small");
            let mut data = vec![0u8; size];

            // Signature
            data[0] = b'F'; data[1] = b'A'; data[2] = b'C'; data[3] = b'P';
            // Length (little-endian u32)
            let len = size as u32;
            data[4] = (len & 0xFF) as u8;
            data[5] = ((len >> 8) & 0xFF) as u8;
            data[6] = ((len >> 16) & 0xFF) as u8;
            data[7] = ((len >> 24) & 0xFF) as u8;
            // Revision
            data[8] = 2; // ACPI 2.0

            Self { data }
        }

        /// Write a little-endian u32 at `offset`.
        fn set_u32(&mut self, offset: usize, val: u32) {
            self.data[offset]     = (val & 0xFF) as u8;
            self.data[offset + 1] = ((val >> 8) & 0xFF) as u8;
            self.data[offset + 2] = ((val >> 16) & 0xFF) as u8;
            self.data[offset + 3] = ((val >> 24) & 0xFF) as u8;
        }

        /// Write a little-endian u64 at `offset`.
        fn set_u64(&mut self, offset: usize, val: u64) {
            for i in 0..8 {
                self.data[offset + i] = ((val >> (i * 8)) & 0xFF) as u8;
            }
        }

        /// Fix the checksum byte (offset 9) so the whole table checksums to zero.
        fn fix_checksum(&mut self) {
            self.data[9] = 0; // clear existing checksum
            let sum: u8 = self.data.iter().fold(0u8, |a, &b| a.wrapping_add(b));
            self.data[9] = 0u8.wrapping_sub(sum);
        }

        /// Return the physical (= host virtual) address of the buffer.
        fn phys(&self) -> u64 {
            self.data.as_ptr() as u64
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_parse_fadt_null_returns_none() {
        assert!(parse_fadt(0).is_none());
    }

    #[test]
    fn test_parse_fadt_bad_signature() {
        let mut buf = FadtBuf::new(FADT_MIN_LEN_RESET);
        buf.data[0] = b'X'; // corrupt signature
        buf.fix_checksum();
        assert!(parse_fadt(buf.phys()).is_none());
    }

    #[test]
    fn test_parse_fadt_bad_checksum() {
        let mut buf = FadtBuf::new(FADT_MIN_LEN_RESET);
        buf.fix_checksum();
        buf.data[10] ^= 0xFF; // flip bits after checksum fix → invalid
        assert!(parse_fadt(buf.phys()).is_none());
    }

    #[test]
    fn test_parse_fadt_too_short() {
        // A buffer shorter than FADT_MIN_LEN_V1 must be rejected.
        let mut data = vec![0u8; FADT_MIN_LEN_V1 - 1];
        data[0] = b'F'; data[1] = b'A'; data[2] = b'C'; data[3] = b'P';
        let len = data.len() as u32;
        data[4] = (len & 0xFF) as u8;
        data[5] = ((len >> 8) & 0xFF) as u8;
        // No checksum fix — length check fires before checksum.
        assert!(parse_fadt(data.as_ptr() as u64).is_none());
    }

    #[test]
    fn test_parse_fadt_pm_timer() {
        let mut buf = FadtBuf::new(FADT_MIN_LEN_RESET);
        // PM timer port = 0x408, len = 4
        buf.set_u32(FADT_OFF_PM_TMR_BLK, 0x0408);
        buf.data[FADT_OFF_PM_TMR_LEN] = 4;
        buf.fix_checksum();

        let info = parse_fadt(buf.phys()).expect("should parse");
        assert_eq!(info.pm_timer_port, 0x0408);
        assert_eq!(info.pm_timer_len, 4);
        assert!(info.pm_timer_valid());
    }

    #[test]
    fn test_pm_timer_invalid_when_len_not_4() {
        let mut buf = FadtBuf::new(FADT_MIN_LEN_RESET);
        buf.set_u32(FADT_OFF_PM_TMR_BLK, 0x0408);
        buf.data[FADT_OFF_PM_TMR_LEN] = 2; // unusual: 2-byte timer
        buf.fix_checksum();

        let info = parse_fadt(buf.phys()).expect("should parse");
        assert!(!info.pm_timer_valid(), "pm_timer_valid must be false when len != 4");
    }

    #[test]
    fn test_parse_fadt_reset_register() {
        let mut buf = FadtBuf::new(FADT_MIN_LEN_RESET);
        // Flags: RESET_REG_SUP set
        buf.set_u32(FADT_OFF_FLAGS, FLAG_RESET_REG_SUP);
        // GAS: I/O space, address = 0xCF9
        buf.data[FADT_OFF_RESET_GAS_SPACE] = GAS_SPACE_IO;
        buf.set_u64(FADT_OFF_RESET_GAS_ADDR, 0x0CF9);
        buf.data[FADT_OFF_RESET_VALUE] = 0x06; // full reset command
        buf.fix_checksum();

        let info = parse_fadt(buf.phys()).expect("should parse");
        assert!(info.reset_supported());
        assert_eq!(info.reset_reg_space, GAS_SPACE_IO);
        assert_eq!(info.reset_reg_addr, 0x0CF9);
        assert_eq!(info.reset_value, 0x06);
    }

    #[test]
    fn test_reset_not_supported_when_flag_absent() {
        let mut buf = FadtBuf::new(FADT_MIN_LEN_RESET);
        // Flags: RESET_REG_SUP NOT set
        buf.set_u32(FADT_OFF_FLAGS, 0);
        buf.set_u64(FADT_OFF_RESET_GAS_ADDR, 0x0CF9);
        buf.data[FADT_OFF_RESET_VALUE] = 0x06;
        buf.fix_checksum();

        let info = parse_fadt(buf.phys()).expect("should parse");
        assert!(!info.reset_supported());
    }

    #[test]
    fn test_reset_absent_for_v1_table() {
        // A v1 FADT (revision < 2) must not expose reset register fields
        // even if the table is large enough and the flag byte happens to be set.
        let mut buf = FadtBuf::new(FADT_MIN_LEN_RESET);
        buf.data[FADT_OFF_REVISION] = 1; // override to v1
        buf.set_u32(FADT_OFF_FLAGS, FLAG_RESET_REG_SUP);
        buf.set_u64(FADT_OFF_RESET_GAS_ADDR, 0x0CF9);
        buf.data[FADT_OFF_RESET_VALUE] = 0x06;
        buf.fix_checksum();

        let info = parse_fadt(buf.phys()).expect("should parse");
        // reset_reg_addr should be zero since v1 tables do not define it.
        assert_eq!(info.reset_reg_addr, 0, "v1 FADT must not expose reset addr");
        assert!(!info.reset_supported());
    }

    #[test]
    fn test_parse_fadt_zero_pm_port_invalid() {
        let mut buf = FadtBuf::new(FADT_MIN_LEN_RESET);
        // PM_TMR_BLK left at zero (not present)
        buf.data[FADT_OFF_PM_TMR_LEN] = 4;
        buf.fix_checksum();

        let info = parse_fadt(buf.phys()).expect("should parse");
        assert!(!info.pm_timer_valid(), "zero port must be invalid");
    }
}
