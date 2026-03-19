/// ACPI DMAR (DMA Remapping Reporting — signature "DMAR") parser.
///
/// The DMAR table is defined by the Intel VT-d Architecture Specification
/// (rev 3.4, §8.1) and is used to enumerate IOMMU (VT-d) hardware units.
/// Each DMA Remapping Hardware Unit (DRHD) entry provides the MMIO base
/// address of one IOMMU controller.
///
/// Rost uses this table to:
///   1. Detect whether any VT-d IOMMU hardware is present.
///   2. Record each IOMMU's MMIO base address for future IOMMU init (§4).
///   3. Check whether interrupt-remapping (IR) is supported by the platform.
///
/// Only DRHD (type 0) entries are decoded; all other remapping-structure
/// types (RMRR, ATSR, RHSA, ANDD, SATC) are skipped safely.
///
/// All field accesses use byte-offset constants to avoid E0793.
/// Offsets match the Intel VT-d spec §8.1 (DMAR header) and §8.3 (DRHD).
///
/// IEC 61508 §7.4.3: any DMA-capable peripheral must be confined to its
/// own physical memory region; the IOMMU is the hardware mechanism that
/// enforces this in silicon.

use super::{checksum_valid, read_u8, read_u16, read_u32, read_u64};

// ── Limits ────────────────────────────────────────────────────────────────────

/// Maximum number of DRH units (IOMMU controllers) to record.
///
/// Real platforms have at most 2–4 IOMMU controllers.  8 is a safe ceiling.
pub const MAX_DRH_UNITS: usize = 8;

// ── DMAR header offsets (VT-d spec §8.1) ─────────────────────────────────────

const DMAR_OFF_SIGNATURE:        usize = 0;  // [u8; 4] = "DMAR"
const DMAR_OFF_LENGTH:           usize = 4;  // u32 — total table byte count
// (revision at 8, checksum at 9 — validated as a block)
const DMAR_OFF_HOST_ADDR_WIDTH:  usize = 36; // u8  — HAW - 1
const DMAR_OFF_FLAGS:            usize = 37; // u8  — platform feature flags
// Reserved bytes 38–47 (10 bytes).
const DMAR_HEADER_LEN:           usize = 48; // 36-byte SDT header + 12 DMAR fields

// ── DMAR header FLAGS bits ────────────────────────────────────────────────────

/// Platform supports interrupt remapping via the IOMMU.
pub const DMAR_FLAG_INTR_REMAP:     u8 = 1 << 0;
/// Platform requests that x2APIC be disabled (legacy quirk).
pub const DMAR_FLAG_X2APIC_OPT_OUT: u8 = 1 << 1;

// ── Remapping-structure entry header offsets ──────────────────────────────────

const REM_OFF_TYPE:   usize = 0; // u16
const REM_OFF_LENGTH: usize = 2; // u16

// ── DRHD (type 0) body offsets (relative to entry base) ─────────────────────

const DRHD_OFF_FLAGS:    usize = 4; // u8
// Byte 5: Reserved.
const DRHD_OFF_SEGMENT:  usize = 6; // u16 — PCI segment number
const DRHD_OFF_REG_BASE: usize = 8; // u64 — MMIO base address

/// Minimum DRHD entry length (header + flags + reserved + segment + base).
const DRHD_MIN_LEN: usize = 16;

// ── Remapping-structure type codes ────────────────────────────────────────────

const TYPE_DRHD: u16 = 0; // DMA Remapping Hardware Unit Definition

// ── DRHD flags ────────────────────────────────────────────────────────────────

/// When set, this DRHD covers all PCI endpoints on the segment not covered
/// by any other DRHD with a Device Scope list.
pub const DRHD_FLAG_INCLUDE_PCI_ALL: u8 = 1 << 0;

// ── Public types ──────────────────────────────────────────────────────────────

/// A single DMA Remapping Hardware Unit (one VT-d IOMMU controller).
#[derive(Debug, Clone, Copy)]
pub struct DrhUnit {
    /// DRHD flags byte.  Bit 0 (`DRHD_FLAG_INCLUDE_PCI_ALL`) means this unit
    /// covers all PCI devices on `segment` not claimed by a scoped DRHD.
    pub flags: u8,
    /// PCI segment number this unit governs (0 on most single-segment systems).
    pub segment: u16,
    /// Physical base address of this IOMMU's MMIO register block.
    /// The VT-d register layout starts here (see VT-d spec §10).
    pub register_base: u64,
}

impl DrhUnit {
    /// Returns `true` if this unit covers all PCI endpoints on its segment.
    #[inline]
    pub fn covers_all_pci(&self) -> bool {
        self.flags & DRHD_FLAG_INCLUDE_PCI_ALL != 0
    }
}

/// Parsed fields from the ACPI DMAR table.
///
/// Only DRHD entries are decoded; the rest of the table is intentionally
/// ignored to keep the parser minimal and auditable.
#[derive(Clone, Copy)]
pub struct DmarInfo {
    /// `HOST_ADDRESS_WIDTH` field from the DMAR header (HAW − 1).
    /// The actual host address width used by the IOMMU is `host_address_width + 1`.
    pub host_address_width: u8,

    /// DMAR `FLAGS` byte (see `DMAR_FLAG_*` constants).
    pub flags: u8,

    /// Fixed-size array of discovered DRH units.
    pub units: [Option<DrhUnit>; MAX_DRH_UNITS],

    /// Number of valid entries in `units`.
    pub unit_count: usize,
}

impl DmarInfo {
    const EMPTY: Option<DrhUnit> = None;

    /// Returns the actual host physical address width (= `host_address_width + 1`).
    #[inline]
    pub fn haw(&self) -> u8 { self.host_address_width.saturating_add(1) }

    /// Returns `true` if the platform supports interrupt remapping via the IOMMU.
    #[inline]
    pub fn intr_remap_supported(&self) -> bool {
        self.flags & DMAR_FLAG_INTR_REMAP != 0
    }

    /// Iterate over valid DRH units.
    #[inline]
    pub fn iter_units(&self) -> impl Iterator<Item = &DrhUnit> {
        self.units[..self.unit_count].iter().filter_map(|u| u.as_ref())
    }
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse the DMAR table at `dmar_phys` and return a [`DmarInfo`].
///
/// Returns `None` if:
/// - `dmar_phys` is zero
/// - The 4-byte signature is not `"DMAR"`
/// - The full-table ACPI checksum fails
/// - The table is shorter than the 48-byte minimum
///
/// Unknown remapping-structure types are silently skipped; truncated entries
/// (reported length < minimum for their type) are also skipped.
///
/// # Safety
/// `dmar_phys` must be a valid physical address of an ACPI DMAR table that is
/// identity-mapped into the kernel's virtual address space for its lifetime.
pub fn parse_dmar(dmar_phys: u64) -> Option<DmarInfo> {
    if dmar_phys == 0 { return None; }

    let base = dmar_phys as *const u8;

    // Validate signature "DMAR".
    let sig = unsafe { core::slice::from_raw_parts(base.add(DMAR_OFF_SIGNATURE), 4) };
    if sig != b"DMAR" { return None; }

    let len = unsafe { read_u32(base.add(DMAR_OFF_LENGTH)) } as usize;
    if len < DMAR_HEADER_LEN { return None; }
    if !checksum_valid(base, len) { return None; }

    let host_address_width = unsafe { read_u8(base.add(DMAR_OFF_HOST_ADDR_WIDTH)) };
    let flags              = unsafe { read_u8(base.add(DMAR_OFF_FLAGS)) };

    let mut info = DmarInfo {
        host_address_width,
        flags,
        units:      [DmarInfo::EMPTY; MAX_DRH_UNITS],
        unit_count: 0,
    };

    // Walk the remapping-structure list starting at DMAR_HEADER_LEN.
    let table_end = dmar_phys as usize + len;
    let mut offset = dmar_phys as usize + DMAR_HEADER_LEN;

    while offset + 4 <= table_end {
        let entry = offset as *const u8;
        let entry_type   = unsafe { read_u16(entry.add(REM_OFF_TYPE)) };
        let entry_len    = unsafe { read_u16(entry.add(REM_OFF_LENGTH)) } as usize;

        // A zero-length entry would loop forever; a length smaller than the
        // header is malformed firmware — skip the rest of the table.
        if entry_len < 4 { break; }
        // Guard against reading past the end of the table.
        if offset + entry_len > table_end { break; }

        if entry_type == TYPE_DRHD && entry_len >= DRHD_MIN_LEN {
            let drhd_flags   = unsafe { read_u8(entry.add(DRHD_OFF_FLAGS)) };
            let segment      = unsafe { read_u16(entry.add(DRHD_OFF_SEGMENT)) };
            let register_base= unsafe { read_u64(entry.add(DRHD_OFF_REG_BASE)) };

            if info.unit_count < MAX_DRH_UNITS {
                info.units[info.unit_count] = Some(DrhUnit {
                    flags: drhd_flags,
                    segment,
                    register_base,
                });
                info.unit_count += 1;
            }
        }
        // All other entry types (RMRR, ATSR, RHSA, ANDD, SATC) are skipped.

        offset += entry_len;
    }

    Some(info)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Minimal DMAR buffer helper ────────────────────────────────────────────

    struct DmarBuf { data: Vec<u8> }

    impl DmarBuf {
        /// Allocate a zeroed DMAR buffer of `size` bytes, stamp the SDT header
        /// (signature + length), then fix the checksum.
        fn new(size: usize) -> Self {
            assert!(size >= DMAR_HEADER_LEN);
            let mut data = vec![0u8; size];
            data[0] = b'D'; data[1] = b'M'; data[2] = b'A'; data[3] = b'R';
            let len = size as u32;
            data[4] = (len & 0xFF) as u8;
            data[5] = ((len >>  8) & 0xFF) as u8;
            data[6] = ((len >> 16) & 0xFF) as u8;
            data[7] = ((len >> 24) & 0xFF) as u8;
            Self { data }
        }

        fn set_u8(&mut self, offset: usize, val: u8) { self.data[offset] = val; }

        fn set_u16(&mut self, offset: usize, val: u16) {
            self.data[offset]     = (val & 0xFF) as u8;
            self.data[offset + 1] = ((val >> 8) & 0xFF) as u8;
        }

        fn set_u32(&mut self, offset: usize, val: u32) {
            for i in 0..4 { self.data[offset + i] = ((val >> (i * 8)) & 0xFF) as u8; }
        }

        fn set_u64(&mut self, offset: usize, val: u64) {
            for i in 0..8 { self.data[offset + i] = ((val >> (i * 8)) & 0xFF) as u8; }
        }

        /// Append a minimal DRHD entry at the current end of the buffer.
        fn append_drhd(&mut self, flags: u8, segment: u16, register_base: u64) {
            let entry_start = self.data.len();
            // Extend buffer by DRHD_MIN_LEN bytes.
            self.data.resize(entry_start + DRHD_MIN_LEN, 0);
            // Update declared table length.
            let new_len = self.data.len() as u32;
            self.data[4] = (new_len & 0xFF) as u8;
            self.data[5] = ((new_len >>  8) & 0xFF) as u8;
            self.data[6] = ((new_len >> 16) & 0xFF) as u8;
            self.data[7] = ((new_len >> 24) & 0xFF) as u8;
            // Entry header.
            self.set_u16(entry_start + REM_OFF_TYPE,   TYPE_DRHD);
            self.set_u16(entry_start + REM_OFF_LENGTH, DRHD_MIN_LEN as u16);
            // DRHD body.
            self.set_u8 (entry_start + DRHD_OFF_FLAGS,    flags);
            self.set_u16(entry_start + DRHD_OFF_SEGMENT,  segment);
            self.set_u64(entry_start + DRHD_OFF_REG_BASE, register_base);
        }

        /// Append a minimal non-DRHD entry (e.g. RMRR, type 1) to test skipping.
        fn append_other(&mut self, entry_type: u16) {
            let entry_start = self.data.len();
            self.data.resize(entry_start + 24, 0); // arbitrary body
            let new_len = self.data.len() as u32;
            self.data[4] = (new_len & 0xFF) as u8;
            self.data[5] = ((new_len >>  8) & 0xFF) as u8;
            self.data[6] = ((new_len >> 16) & 0xFF) as u8;
            self.data[7] = ((new_len >> 24) & 0xFF) as u8;
            self.set_u16(entry_start + REM_OFF_TYPE,   entry_type);
            self.set_u16(entry_start + REM_OFF_LENGTH, 24);
        }

        /// Recompute checksum (byte 9 of SDT header).
        fn fix_checksum(&mut self) {
            self.data[9] = 0;
            let sum: u8 = self.data.iter().fold(0u8, |a, &b| a.wrapping_add(b));
            self.data[9] = 0u8.wrapping_sub(sum);
        }

        fn phys(&self) -> u64 { self.data.as_ptr() as u64 }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_parse_dmar_null_returns_none() {
        assert!(parse_dmar(0).is_none());
    }

    #[test]
    fn test_parse_dmar_bad_signature() {
        let mut buf = DmarBuf::new(DMAR_HEADER_LEN);
        buf.data[0] = b'X'; // corrupt signature
        buf.fix_checksum();
        assert!(parse_dmar(buf.phys()).is_none());
    }

    #[test]
    fn test_parse_dmar_bad_checksum() {
        let mut buf = DmarBuf::new(DMAR_HEADER_LEN);
        buf.fix_checksum();
        buf.data[10] ^= 0xFF; // flip bits after checksum fix
        assert!(parse_dmar(buf.phys()).is_none());
    }

    #[test]
    fn test_parse_dmar_too_short() {
        let mut data = vec![0u8; DMAR_HEADER_LEN - 1];
        data[0] = b'D'; data[1] = b'M'; data[2] = b'A'; data[3] = b'R';
        let len = data.len() as u32;
        data[4] = (len & 0xFF) as u8;
        data[5] = ((len >> 8) & 0xFF) as u8;
        // length check fires before checksum
        assert!(parse_dmar(data.as_ptr() as u64).is_none());
    }

    #[test]
    fn test_parse_dmar_no_units() {
        // A DMAR table with no remapping structure entries is valid.
        let mut buf = DmarBuf::new(DMAR_HEADER_LEN);
        buf.set_u8(DMAR_OFF_HOST_ADDR_WIDTH, 38); // HAW-1 for 39-bit addressing
        buf.set_u8(DMAR_OFF_FLAGS, DMAR_FLAG_INTR_REMAP);
        buf.fix_checksum();

        let info = parse_dmar(buf.phys()).expect("should parse empty DMAR");
        assert_eq!(info.unit_count, 0);
        assert_eq!(info.host_address_width, 38);
        assert_eq!(info.haw(), 39);
        assert!(info.intr_remap_supported());
    }

    #[test]
    fn test_parse_dmar_single_drhd() {
        let mut buf = DmarBuf::new(DMAR_HEADER_LEN);
        buf.set_u8(DMAR_OFF_HOST_ADDR_WIDTH, 38);
        buf.set_u8(DMAR_OFF_FLAGS, DMAR_FLAG_INTR_REMAP);
        buf.append_drhd(DRHD_FLAG_INCLUDE_PCI_ALL, 0, 0xFED9_0000);
        buf.fix_checksum();

        let info = parse_dmar(buf.phys()).expect("should parse single-DRHD DMAR");
        assert_eq!(info.unit_count, 1);
        let unit = info.units[0].unwrap();
        assert_eq!(unit.flags, DRHD_FLAG_INCLUDE_PCI_ALL);
        assert_eq!(unit.segment, 0);
        assert_eq!(unit.register_base, 0xFED9_0000);
        assert!(unit.covers_all_pci());
    }

    #[test]
    fn test_parse_dmar_multi_drhd() {
        let mut buf = DmarBuf::new(DMAR_HEADER_LEN);
        buf.set_u8(DMAR_OFF_HOST_ADDR_WIDTH, 38);
        buf.append_drhd(0, 0, 0xFED9_0000);
        buf.append_drhd(DRHD_FLAG_INCLUDE_PCI_ALL, 1, 0xFED9_1000);
        buf.fix_checksum();

        let info = parse_dmar(buf.phys()).expect("two-DRHD");
        assert_eq!(info.unit_count, 2);
        assert_eq!(info.units[0].unwrap().register_base, 0xFED9_0000);
        assert_eq!(info.units[0].unwrap().segment, 0);
        assert!(!info.units[0].unwrap().covers_all_pci());
        assert_eq!(info.units[1].unwrap().register_base, 0xFED9_1000);
        assert_eq!(info.units[1].unwrap().segment, 1);
        assert!(info.units[1].unwrap().covers_all_pci());
    }

    #[test]
    fn test_parse_dmar_skip_non_drhd() {
        // RMRR (type 1) entry must be skipped; DRHD must still be counted.
        let mut buf = DmarBuf::new(DMAR_HEADER_LEN);
        buf.set_u8(DMAR_OFF_HOST_ADDR_WIDTH, 38);
        buf.append_other(1); // RMRR
        buf.append_drhd(DRHD_FLAG_INCLUDE_PCI_ALL, 0, 0xFED9_0000);
        buf.append_other(2); // ATSR
        buf.fix_checksum();

        let info = parse_dmar(buf.phys()).expect("skip non-DRHD");
        assert_eq!(info.unit_count, 1, "only the DRHD must be counted");
        assert_eq!(info.units[0].unwrap().register_base, 0xFED9_0000);
    }

    #[test]
    fn test_parse_dmar_truncated_drhd_skipped() {
        // A DRHD with entry_len < DRHD_MIN_LEN (16) must be skipped.
        let mut buf = DmarBuf::new(DMAR_HEADER_LEN);
        buf.set_u8(DMAR_OFF_HOST_ADDR_WIDTH, 38);

        // Manually append a short DRHD (12 bytes — missing the register_base).
        let entry_start = buf.data.len();
        buf.data.resize(entry_start + 12, 0);
        let new_len = buf.data.len() as u32;
        buf.data[4] = (new_len & 0xFF) as u8;
        buf.data[5] = ((new_len >> 8) & 0xFF) as u8;
        buf.data[6] = ((new_len >> 16) & 0xFF) as u8;
        buf.data[7] = ((new_len >> 24) & 0xFF) as u8;
        buf.set_u16(entry_start + REM_OFF_TYPE,   TYPE_DRHD);
        buf.set_u16(entry_start + REM_OFF_LENGTH, 12); // too short
        buf.fix_checksum();

        let info = parse_dmar(buf.phys()).expect("should parse despite short DRHD");
        assert_eq!(info.unit_count, 0, "truncated DRHD must be skipped");
    }

    #[test]
    fn test_iter_units() {
        let mut buf = DmarBuf::new(DMAR_HEADER_LEN);
        buf.set_u8(DMAR_OFF_HOST_ADDR_WIDTH, 38);
        buf.append_drhd(0, 0, 0xFED9_0000);
        buf.append_drhd(DRHD_FLAG_INCLUDE_PCI_ALL, 0, 0xFED9_1000);
        buf.fix_checksum();

        let info = parse_dmar(buf.phys()).unwrap();
        let bases: Vec<u64> = info.iter_units().map(|u| u.register_base).collect();
        assert_eq!(bases, [0xFED9_0000, 0xFED9_1000]);
    }

    #[test]
    fn test_intr_remap_flag_absent() {
        let mut buf = DmarBuf::new(DMAR_HEADER_LEN);
        buf.set_u8(DMAR_OFF_FLAGS, 0); // INTR_REMAP not set
        buf.fix_checksum();

        let info = parse_dmar(buf.phys()).unwrap();
        assert!(!info.intr_remap_supported());
    }
}
