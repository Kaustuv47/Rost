/// Intel VT-d IOMMU initialisation (passthrough mode).
///
/// Implements the minimal boot-time IOMMU setup required by IEC 61508 §7.4.3:
/// the IOMMU is enabled so that untranslated DMA from any bus master passes
/// through a kernel-controlled root/context table, enabling per-device
/// restriction in the future.
///
/// # Current implementation: permissive passthrough
/// A single 4 KB context table is shared across all PCI buses.  Every device
/// entry is configured for **passthrough** translation (ECAP.PT = 1 required),
/// which allows DMA to any physical address.  This is equivalent to having
/// no IOMMU from a security standpoint, but it:
/// - Proves the hardware can be enabled without crashing.
/// - Installs the root / context table infrastructure that per-device
///   restriction will overlay in a future patch.
///
/// # Root table layout (4 KB = 256 × 16-byte entries)
/// ```text
///   RootEntry[bus] = { present=1, ctx_table_ptr = ctx_phys }
/// ```
///
/// # Context table layout (4 KB = 256 × 16-byte entries)
/// ```text
///   CtxEntry[dev*8+func] = { present=1, TT=passthrough, domain_id=1 }
/// ```
///
/// # IOMMU register map (offsets from IOMMU MMIO base)
/// ```text
///   +0x00  VER     — version (bits[7:4]=major, bits[3:0]=minor)
///   +0x08  CAP     — capability
///   +0x10  ECAP    — extended capability (bit 6 = PT passthrough support)
///   +0x18  GCMD    — global command (write; bit31=TE, bit30=SRTP, bit27=WBF)
///   +0x1C  GSTS    — global status (read; bit31=TES, bit30=RTPS, bit27=WBFS)
///   +0x20  RTADDR  — root table address (bits[63:12] = root table phys >> 12)
/// ```

use crate::memory::global_alloc_4k;

// ── IOMMU register offsets ────────────────────────────────────────────────────

const OFF_VER:    usize = 0x00;
#[allow(dead_code)]
const OFF_CAP:    usize = 0x08;
const OFF_ECAP:   usize = 0x10;
const OFF_GCMD:   usize = 0x18;
const OFF_GSTS:   usize = 0x1C;
const OFF_RTADDR: usize = 0x20;

/// GCMD: Translation Enable (bit 31).
const GCMD_TE:   u32 = 1 << 31;
/// GCMD: Set Root Table Pointer (bit 30).
const GCMD_SRTP: u32 = 1 << 30;
/// GCMD: Write Buffer Flush (bit 27).
const GCMD_WBF:  u32 = 1 << 27;

/// GSTS: Translation Enable Status (bit 31).
const GSTS_TES:  u32 = 1 << 31;
/// GSTS: Root Table Pointer Status (bit 30).
const GSTS_RTPS: u32 = 1 << 30;
/// GSTS: Write Buffer Flush Status (bit 27).
const GSTS_WBFS: u32 = 1 << 27;

/// ECAP bit 6: Passthrough (PT) translation type supported.
const ECAP_PT:   u64 = 1 << 6;

// ── MMIO access ───────────────────────────────────────────────────────────────

#[inline]
unsafe fn write32(base: u64, off: usize, val: u32) {
    core::ptr::write_volatile((base as usize + off) as *mut u32, val);
}

#[inline]
unsafe fn read32(base: u64, off: usize) -> u32 {
    core::ptr::read_volatile((base as usize + off) as *const u32)
}

#[inline]
unsafe fn write64(base: u64, off: usize, val: u64) {
    core::ptr::write_volatile((base as usize + off) as *mut u64, val);
}

#[inline]
unsafe fn read64(base: u64, off: usize) -> u64 {
    core::ptr::read_volatile((base as usize + off) as *const u64)
}

// ── Entry encoding (pure functions, unit-testable) ────────────────────────────

/// Encode a root table entry pointing at `ctx_phys`.
///
/// Returns `(low_64, high_64)`.  The low word has bit 0 (present) set.
/// The high word is reserved and written as 0.
#[inline]
pub fn root_entry(ctx_phys: u64) -> (u64, u64) {
    ((ctx_phys & !0xFFF) | 1, 0)
}

/// Encode a passthrough context entry.
///
/// Translation Type 10b (passthrough) requires ECAP.PT = 1.
/// `domain_id` labels the DMA domain (non-zero; use 1 for the shared passthrough domain).
///
/// Returns `(low_64, high_64)`.
#[inline]
pub fn passthrough_context_entry(domain_id: u16) -> (u64, u64) {
    // TT = 0b10 occupies bits[3:2]; present = bit[0].
    // bit[3]=1, bit[2]=0, bit[0]=1 → 0b1001 = 9 = 0x9
    let low  = (1u64 << 3) | (0u64 << 2) | 1u64; // TT=10, P=1
    let high = domain_id as u64;
    (low, high)
}

// ── Per-unit initialisation ───────────────────────────────────────────────────

/// Initialise a single IOMMU unit at `base`.
///
/// Returns `true` if the unit was successfully enabled, `false` on any error
/// (passthrough not supported, allocation failure, etc.).
unsafe fn init_unit(base: u64) -> bool {
    if base == 0 { return false; }

    // Read version and extended capability.
    let ver  = read32(base, OFF_VER);
    let ecap = read64(base, OFF_ECAP);

    let major = (ver >> 4) & 0xF;
    let minor =  ver        & 0xF;

    hal::uart::print_str("        VT-d unit: ver=");
    hal::uart::print_dec(major as u64);
    hal::uart::print_str(".");
    hal::uart::print_dec(minor as u64);

    if ecap & ECAP_PT == 0 {
        hal::uart::print_str("  [no PT support — skipped]\n");
        return false;
    }
    hal::uart::print_str("  PT=yes");

    // Allocate and zero root table (4 KB, 256 × 16-byte entries).
    let root_phys = match global_alloc_4k() {
        Some(p) => p,
        None => {
            hal::uart::print_str("  [alloc failed]\n");
            return false;
        }
    };
    core::ptr::write_bytes(root_phys as *mut u8, 0, 4096);

    // Allocate and zero context table (shared across all buses).
    let ctx_phys = match global_alloc_4k() {
        Some(p) => p,
        None => {
            hal::uart::print_str("  [ctx alloc failed]\n");
            return false;
        }
    };
    core::ptr::write_bytes(ctx_phys as *mut u8, 0, 4096);

    // Fill context table: 256 passthrough entries (covers all dev+func combos).
    let (ce_lo, ce_hi) = passthrough_context_entry(1);
    let ctx_ptr = ctx_phys as *mut u64;
    for i in 0..256usize {
        ctx_ptr.add(i * 2    ).write_volatile(ce_lo);
        ctx_ptr.add(i * 2 + 1).write_volatile(ce_hi);
    }

    // Fill root table: 256 entries, all pointing to the shared context table.
    let (re_lo, re_hi) = root_entry(ctx_phys);
    let root_ptr = root_phys as *mut u64;
    for i in 0..256usize {
        root_ptr.add(i * 2    ).write_volatile(re_lo);
        root_ptr.add(i * 2 + 1).write_volatile(re_hi);
    }

    // Program root table address register (already 4 KB-aligned; low 12 bits = 0).
    write64(base, OFF_RTADDR, root_phys);

    // Issue SRTP (Set Root Table Pointer) and wait for hardware to acknowledge.
    // IEC 61508 §7.4.1: all busy-waits must be bounded.  VT-d SRTP typically
    // completes in < 1 µs; 100 000 iterations is a safe upper bound (~10 ms).
    write32(base, OFF_GCMD, GCMD_SRTP);
    {
        let mut t = 100_000u32;
        while read32(base, OFF_GSTS) & GSTS_RTPS == 0 {
            t -= 1;
            if t == 0 {
                hal::uart::print_str("  [iommu] WARN: SRTP timeout\n");
                return false;
            }
        }
    }

    // Flush write buffers before enabling translation.
    write32(base, OFF_GCMD, GCMD_WBF);
    {
        let mut t = 100_000u32;
        while read32(base, OFF_GSTS) & GSTS_WBFS != 0 {
            t -= 1;
            if t == 0 {
                hal::uart::print_str("  [iommu] WARN: WBF timeout\n");
                return false;
            }
        }
    }

    // Enable IOMMU translation (TE) and wait for GSTS.TES.
    write32(base, OFF_GCMD, GCMD_TE);
    {
        let mut t = 100_000u32;
        while read32(base, OFF_GSTS) & GSTS_TES == 0 {
            t -= 1;
            if t == 0 {
                hal::uart::print_str("  [iommu] WARN: TE timeout\n");
                return false;
            }
        }
    }

    hal::uart::print_str("  enabled\n");
    true
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Initialise all discovered VT-d IOMMU units.
///
/// `units` is the slice of MMIO base addresses obtained from the ACPI DMAR
/// table (see `core_kernel::acpi::dmar`).  Skips zero entries.
///
/// Returns the number of units successfully enabled.
pub fn init(units: &[u64]) -> usize {
    if units.is_empty() || units.iter().all(|&b| b == 0) {
        return 0;
    }
    let mut enabled = 0usize;
    for &base in units {
        if base == 0 { continue; }
        if unsafe { init_unit(base) } {
            enabled += 1;
        }
    }
    enabled
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// root_entry sets present bit and aligns address.
    #[test]
    fn test_root_entry_present() {
        let ctx_phys: u64 = 0x0000_1234_5678_0000; // 4 KB aligned
        let (lo, hi) = root_entry(ctx_phys);
        assert_eq!(lo & 1, 1, "present bit must be set");
        assert_eq!(lo & !0xFFF, ctx_phys, "context table ptr must be preserved");
        assert_eq!(hi, 0, "high 64 bits are reserved/zero");
    }

    /// root_entry masks off low 12 bits of a non-aligned address.
    #[test]
    fn test_root_entry_alignment() {
        let (lo, _) = root_entry(0x1234_5000 + 0xABC); // not 4 KB aligned
        assert_eq!(lo & 0xFFF, 1, "only present bit in low 12 bits");
    }

    /// passthrough_context_entry: present bit set, TT = passthrough (0b10).
    #[test]
    fn test_passthrough_ctx_entry() {
        let (lo, hi) = passthrough_context_entry(1);
        assert_eq!(lo & 1, 1, "present bit must be set");
        // bits[3:2] = TT: bit3=1, bit2=0 → TT=10 (passthrough)
        assert_eq!((lo >> 2) & 0x3, 0b10, "TT must be 0b10 (passthrough)");
        assert_eq!(hi, 1, "domain_id must appear in upper 64 bits");
    }

    /// passthrough_context_entry uses the supplied domain_id.
    #[test]
    fn test_passthrough_ctx_domain_id() {
        let (_, hi) = passthrough_context_entry(42);
        assert_eq!(hi, 42, "domain_id must be in high 64-bit word");
    }
}
