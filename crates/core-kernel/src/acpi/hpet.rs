/// ACPI HPET (High Precision Event Timer — signature "HPET") parser.
///
/// The HPET table is defined by the IA-PC HPET Architecture Specification
/// (v1.0a §3.2) and is also described in ACPI 6.5 §5.2.20.  It provides
/// the MMIO base address of the HPET register block and the timer block ID
/// (number of comparators, 64-bit counter capability, hardware revision).
///
/// Rost uses this table to:
///   1. Locate the HPET MMIO register block.
///   2. Read the counter clock period (femtoseconds per tick) from GCI[63:32].
///   3. Calibrate the TSC frequency against the HPET main counter at boot.
///
/// All field accesses use byte-offset constants to avoid E0793.
/// Offsets match ACPI 6.5 §5.2.20 (HPET table) and §5.2.3 (GAS structure).
///
/// IEC 61508 §7.4.1: all external data (including ACPI firmware tables) must
/// be validated before use; table checksum and minimum length are verified.

use super::{checksum_valid, read_u32, read_u64, read_u8, find_table};

// ── HPET table field offsets (ACPI 6.5 §5.2.20) ──────────────────────────────

const HPET_OFF_SIGNATURE:   usize = 0;  // [u8; 4] = "HPET"
const HPET_OFF_LENGTH:      usize = 4;  // u32 — total table byte count
// Standard SDT header occupies bytes 0–35 (36 bytes).
const HPET_OFF_BLOCK_ID:    usize = 36; // u32 — Event Timer Block ID
// Generic Address Structure (GAS) starts at offset 40.
// GAS layout (ACPI 6.5 §5.2.3.2):
//   +0  u8  AddressSpaceID (0=SystemMemory)
//   +1  u8  RegisterBitWidth
//   +2  u8  RegisterBitOffset
//   +3  u8  AccessSize
//   +4  u64 Address
const HPET_OFF_GAS_ADDR_SPACE:  usize = 40; // u8
const HPET_OFF_GAS_ADDRESS:     usize = 44; // u64 — HPET MMIO base address
#[allow(dead_code)]
const HPET_OFF_SEQUENCE:        usize = 52; // u8  — HPET block sequence number
// Minimum valid table length: 36 (SDT) + 4 (block_id) + 12 (GAS) + 4 = 56
const HPET_MIN_LEN: usize = 56;

// ── Event Timer Block ID bit fields ───────────────────────────────────────────

/// Mask for hardware revision ID (bits[7:0] of block_id).
const BLOCK_ID_REV_MASK: u32 = 0xFF;
/// Mask for number of comparators − 1 (bits[12:8] of block_id).
const BLOCK_ID_NUM_TIM_MASK: u32 = 0x1F00;
const BLOCK_ID_NUM_TIM_SHIFT: u32 = 8;
/// Count size capability bit (bit 13): 1 = 64-bit main counter.
const BLOCK_ID_COUNT_SIZE_CAP: u32 = 1 << 13;

// ── HPET MMIO register offsets ────────────────────────────────────────────────

/// General Capabilities and ID register offset (read-only, 64-bit).
/// bits[63:32] = COUNTER_CLK_PERIOD (femtoseconds per tick).
const HPET_REG_GCI: u64 = 0x000;

/// AddressSpaceID value for System Memory (MMIO).
const GAS_ADDR_SPACE_MEMORY: u8 = 0;

// ── Public types ──────────────────────────────────────────────────────────────

/// Parsed fields extracted from the ACPI HPET table.
///
/// Contains the information needed to initialise the HPET hardware and
/// to calibrate the TSC frequency at boot.
#[derive(Debug, Clone, Copy)]
pub struct HpetInfo {
    /// Physical base address of the HPET MMIO register block.
    ///
    /// Identity-mapped into the kernel's virtual address space for its
    /// lifetime.  Used to enable the main counter and read its value.
    pub base_address: u64,

    /// Counter clock period in femtoseconds (fs) per tick.
    ///
    /// Read from GCI bits[63:32] after the main counter is enabled.
    /// Typical values: 69841 fs (14.318 MHz PIT-derived), 100000 fs (10 MHz).
    /// Must be in the range 1..=0x05F5_E100 (ACPI minimum 10 MHz guarantee).
    pub period_fs: u64,

    /// Number of HPET comparators present (= block_id bits[12:8] + 1).
    ///
    /// Minimum is 3 per the HPET specification.
    pub timer_count: u8,

    /// `true` if the main counter register is 64-bit (block_id bit 13).
    ///
    /// ACPI guarantees at least 32-bit; 64-bit is recommended.
    pub counter_64bit: bool,

    /// HPET hardware revision ID (block_id bits[7:0]).
    pub hardware_rev: u8,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Parse the RSDP at `rsdp_phys` and return the physical address of the HPET
/// table, or `None` if the RSDP is invalid or the platform has no HPET.
pub fn find_hpet(rsdp_phys: u64) -> Option<u64> {
    find_table(rsdp_phys, b"HPET")
}

/// Extract only the HPET MMIO base address from the ACPI HPET table.
///
/// Reads the GAS.Address field at offset 44 from the ACPI table without
/// accessing the HPET MMIO registers.  This is used by the caller to map
/// the MMIO page before calling `parse_hpet()`, which reads the live GCI
/// register to obtain the counter clock period.
///
/// Returns `None` if `hpet_phys` is zero, the signature is wrong, or the
/// GAS.Address field is zero.
pub fn hpet_mmio_base(hpet_phys: u64) -> Option<u64> {
    if hpet_phys == 0 { return None; }
    let base = hpet_phys as *const u8;
    let sig = unsafe { core::slice::from_raw_parts(base.add(HPET_OFF_SIGNATURE), 4) };
    if sig != b"HPET" { return None; }
    let addr = unsafe { read_u64(base.add(HPET_OFF_GAS_ADDRESS)) };
    if addr == 0 { return None; }
    Some(addr)
}

/// Parse the HPET table at `hpet_phys` and return an [`HpetInfo`].
///
/// Returns `None` if:
/// - `hpet_phys` is zero
/// - The 4-byte signature is not `"HPET"`
/// - The declared length is shorter than the 56-byte minimum
/// - The ACPI checksum fails
/// - The GAS `AddressSpaceID` is not 0 (SystemMemory) — I/O-space HPET
///   is not supported by this driver
/// - The GCI period field is zero or implausibly large (> 0x05F5_E100 fs,
///   i.e., slower than 10 MHz — the ACPI-mandated minimum frequency)
///
/// # Safety
/// `hpet_phys` must be a valid physical address of an ACPI HPET table that
/// is identity-mapped into the kernel's virtual address space for its lifetime.
pub fn parse_hpet(hpet_phys: u64) -> Option<HpetInfo> {
    if hpet_phys == 0 { return None; }

    let base = hpet_phys as *const u8;

    // Validate signature "HPET".
    let sig = unsafe { core::slice::from_raw_parts(base.add(HPET_OFF_SIGNATURE), 4) };
    if sig != b"HPET" { return None; }

    // Length check before checksum (cheaper fail path).
    let len = unsafe { read_u32(base.add(HPET_OFF_LENGTH)) } as usize;
    if len < HPET_MIN_LEN { return None; }

    // Full ACPI checksum.
    if !checksum_valid(base, len) { return None; }

    // Parse Event Timer Block ID.
    let block_id = unsafe { read_u32(base.add(HPET_OFF_BLOCK_ID)) };
    let hardware_rev    = (block_id & BLOCK_ID_REV_MASK) as u8;
    let timer_count_raw = ((block_id & BLOCK_ID_NUM_TIM_MASK) >> BLOCK_ID_NUM_TIM_SHIFT) as u8;
    let timer_count     = timer_count_raw + 1; // field is N−1
    let counter_64bit   = block_id & BLOCK_ID_COUNT_SIZE_CAP != 0;

    // Validate GAS AddressSpaceID — must be System Memory (0).
    let addr_space = unsafe { read_u8(base.add(HPET_OFF_GAS_ADDR_SPACE)) };
    if addr_space != GAS_ADDR_SPACE_MEMORY { return None; }

    // Extract MMIO base address from GAS.
    let base_address = unsafe { read_u64(base.add(HPET_OFF_GAS_ADDRESS)) };
    if base_address == 0 { return None; }

    // Read COUNTER_CLK_PERIOD from the live HPET GCI register.
    // bits[63:32] of the 64-bit GCI register at MMIO offset 0x000.
    let gci = unsafe {
        core::ptr::read_volatile((base_address + HPET_REG_GCI) as *const u64)
    };
    let period_fs = gci >> 32;

    // Validate period_fs: must be non-zero and ≤ 0x05F5_E100 (100 ns, i.e.
    // 10 MHz — the ACPI-mandated minimum HPET frequency).
    if period_fs == 0 || period_fs > 0x05F5_E100 { return None; }

    Some(HpetInfo { base_address, period_fs, timer_count, counter_64bit, hardware_rev })
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── HPET buffer builder ───────────────────────────────────────────────────

    struct HpetBuf { data: Vec<u8> }

    impl HpetBuf {
        /// Create a minimal, valid HPET table buffer (no live MMIO — GCI is
        /// faked in memory at `base_address`).
        ///
        /// The `gci_buf` slice must be at least 8 bytes; its address is used
        /// as the `base_address` in the GAS field so `parse_hpet` can read a
        /// synthesised `period_fs` without touching real MMIO.
        fn new(block_id: u32, gci_buf: &[u8]) -> Self {
            assert!(gci_buf.len() >= 8, "gci_buf must be ≥ 8 bytes");

            let size = HPET_MIN_LEN;
            let mut data = vec![0u8; size];

            // Signature.
            data[0] = b'H'; data[1] = b'P'; data[2] = b'E'; data[3] = b'T';

            // Length.
            let len = size as u32;
            data[4] = (len & 0xFF) as u8;
            data[5] = ((len >>  8) & 0xFF) as u8;
            data[6] = ((len >> 16) & 0xFF) as u8;
            data[7] = ((len >> 24) & 0xFF) as u8;

            // Block ID.
            data[HPET_OFF_BLOCK_ID]     = (block_id & 0xFF) as u8;
            data[HPET_OFF_BLOCK_ID + 1] = ((block_id >>  8) & 0xFF) as u8;
            data[HPET_OFF_BLOCK_ID + 2] = ((block_id >> 16) & 0xFF) as u8;
            data[HPET_OFF_BLOCK_ID + 3] = ((block_id >> 24) & 0xFF) as u8;

            // GAS: AddressSpaceID = 0 (SystemMemory).
            data[HPET_OFF_GAS_ADDR_SPACE] = GAS_ADDR_SPACE_MEMORY;

            // GAS address = pointer to caller-provided gci_buf.
            let addr = gci_buf.as_ptr() as u64;
            for i in 0..8 {
                data[HPET_OFF_GAS_ADDRESS + i] = ((addr >> (i * 8)) & 0xFF) as u8;
            }

            HpetBuf { data }
        }

        fn fix_checksum(&mut self) {
            self.data[9] = 0;
            let sum: u8 = self.data.iter().fold(0u8, |a, &b| a.wrapping_add(b));
            self.data[9] = 0u8.wrapping_sub(sum);
        }

        fn phys(&self) -> u64 { self.data.as_ptr() as u64 }
    }

    /// Build an 8-byte GCI value (little-endian) with the given period_fs.
    fn make_gci(period_fs: u64) -> [u8; 8] {
        let gci: u64 = period_fs << 32;
        let mut out = [0u8; 8];
        for i in 0..8 { out[i] = ((gci >> (i * 8)) & 0xFF) as u8; }
        out
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_parse_hpet_null_returns_none() {
        assert!(parse_hpet(0).is_none());
    }

    #[test]
    fn test_parse_hpet_bad_signature() {
        let gci = make_gci(100_000); // 100 ns (10 MHz)
        let mut buf = HpetBuf::new(0x0000_8102, &gci); // 1 timer, 64-bit
        buf.data[0] = b'X'; // corrupt signature
        buf.fix_checksum();
        assert!(parse_hpet(buf.phys()).is_none());
    }

    #[test]
    fn test_parse_hpet_bad_checksum() {
        let gci = make_gci(100_000);
        let mut buf = HpetBuf::new(0x0000_8102, &gci);
        buf.fix_checksum();
        buf.data[10] ^= 0xFF; // flip bits after checksum fix
        assert!(parse_hpet(buf.phys()).is_none());
    }

    #[test]
    fn test_parse_hpet_too_short() {
        let mut data = vec![0u8; HPET_MIN_LEN - 1];
        data[0] = b'H'; data[1] = b'P'; data[2] = b'E'; data[3] = b'T';
        let len = data.len() as u32;
        data[4] = (len & 0xFF) as u8;
        data[5] = ((len >> 8) & 0xFF) as u8;
        // length check fires before checksum
        assert!(parse_hpet(data.as_ptr() as u64).is_none());
    }

    #[test]
    fn test_parse_hpet_non_memory_gas_rejected() {
        let gci = make_gci(100_000);
        let mut buf = HpetBuf::new(0x0000_8102, &gci);
        buf.data[HPET_OFF_GAS_ADDR_SPACE] = 1; // SystemIO — unsupported
        buf.fix_checksum();
        assert!(parse_hpet(buf.phys()).is_none());
    }

    #[test]
    fn test_parse_hpet_zero_period_rejected() {
        let gci = make_gci(0); // period = 0 → invalid
        let mut buf = HpetBuf::new(0x0000_8102, &gci);
        buf.fix_checksum();
        assert!(parse_hpet(buf.phys()).is_none());
    }

    #[test]
    fn test_parse_hpet_period_too_large_rejected() {
        let gci = make_gci(0x05F5_E101); // > 100 ns → below 10 MHz minimum
        let mut buf = HpetBuf::new(0x0000_8102, &gci);
        buf.fix_checksum();
        assert!(parse_hpet(buf.phys()).is_none());
    }

    #[test]
    fn test_parse_hpet_valid_10mhz() {
        // QEMU emulates HPET at 10 MHz (period = 100 ns = 100_000 fs).
        let gci = make_gci(100_000);
        // block_id: rev=1, 3 timers (num_tim=2→field=2), 64-bit (bit13), no LRR
        // bits[7:0]=0x01, bits[12:8]=0x02 (3 timers-1=2), bit[13]=1
        // = 0x0000_2201
        let block_id: u32 = 0x01 | (2 << 8) | (1 << 13);
        let mut buf = HpetBuf::new(block_id, &gci);
        buf.fix_checksum();

        let info = parse_hpet(buf.phys()).expect("valid 10 MHz HPET");
        assert_eq!(info.period_fs, 100_000);
        assert_eq!(info.timer_count, 3);
        assert!(info.counter_64bit);
        assert_eq!(info.hardware_rev, 1);
    }

    #[test]
    fn test_parse_hpet_valid_14mhz() {
        // 14.318 MHz PIT-derived HPET: period ≈ 69841 fs.
        let gci = make_gci(69_841);
        let block_id: u32 = 0x01 | (4 << 8); // 5 timers, 32-bit counter
        let mut buf = HpetBuf::new(block_id, &gci);
        buf.fix_checksum();

        let info = parse_hpet(buf.phys()).expect("valid 14.318 MHz HPET");
        assert_eq!(info.period_fs, 69_841);
        assert_eq!(info.timer_count, 5);
        assert!(!info.counter_64bit);
    }

    #[test]
    fn test_parse_hpet_period_at_boundary_accepted() {
        // period_fs == 0x05F5_E100 is exactly 10 MHz — should be accepted.
        let gci = make_gci(0x05F5_E100);
        let block_id: u32 = 0x02 | (2 << 8) | (1 << 13);
        let mut buf = HpetBuf::new(block_id, &gci);
        buf.fix_checksum();
        assert!(parse_hpet(buf.phys()).is_some());
    }
}
