/// ACPI table parser — RSDP / XSDT / MADT / FADT / DMAR.
///
/// Implements the minimal subset of ACPI 6.5 and Intel VT-d required to:
///   - Locate any child SDT via a generic XSDT/RSDT scanner
///   - MADT: enumerate Local APIC and I/O APIC addresses + IRQ overrides
///   - FADT: PM timer I/O port, ACPI reset register (software reset path)
///   - DMAR: Intel VT-d IOMMU controller MMIO bases + interrupt-remap flag
///
/// All parsing is read-only and operates directly on ACPI table memory,
/// which is identity-mapped for the kernel's lifetime.  No heap allocation
/// is used; results are stored in fixed-size Info structs.
///
/// # Safety model
/// Every ACPI table access is bounds-checked against the declared table length.
/// Packed struct fields are always read via `core::ptr::read_unaligned`  with
/// `core::ptr::addr_of!` — never through a Rust reference — to avoid UB from
/// misaligned field references (Rust §E0793).
///
/// IEC 61508 §7.4.2: all external data (including firmware tables) must be
/// validated before use.

pub mod dmar;
pub mod fadt;
pub mod hpet;
pub mod madt;

pub use dmar::{DmarInfo, DrhUnit, parse_dmar};
pub use fadt::{FadtInfo, parse_fadt};
pub use hpet::{HpetInfo, find_hpet, hpet_mmio_base, parse_hpet};
pub use madt::{MadtInfo, LocalApic, IoApic, IrqOverride, parse_madt};

// ── SDT constants ─────────────────────────────────────────────────────────────

/// Byte length of the common ACPI System Description Table header.
const SDT_HEADER_LEN: usize = 36;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Validate the 8-bit checksum of an ACPI table.
///
/// Per ACPI §5.2.3.3, the sum of all bytes in the table must equal zero mod 256.
fn checksum_valid(ptr: *const u8, len: usize) -> bool {
    let mut sum: u8 = 0;
    for i in 0..len {
        sum = sum.wrapping_add(unsafe { *ptr.add(i) });
    }
    sum == 0
}

/// Read a `u8` at an arbitrary address.
///
/// # Safety
/// `ptr` must be a valid readable byte address.
#[inline]
pub(super) unsafe fn read_u8(ptr: *const u8) -> u8 {
    *ptr
}

/// Read a little-endian `u16` at a byte-unaligned address.
///
/// # Safety
/// `ptr` must be readable for at least 2 bytes.
#[inline]
pub(super) unsafe fn read_u16(ptr: *const u8) -> u16 {
    u16::from_le(core::ptr::read_unaligned(ptr as *const u16))
}

/// Read a `u32` at a byte-unaligned address (e.g. inside a packed ACPI struct).
///
/// # Safety
/// `ptr` must be readable for at least 4 bytes.
#[inline]
unsafe fn read_u32(ptr: *const u8) -> u32 {
    u32::from_le(core::ptr::read_unaligned(ptr as *const u32))
}

/// Read a `u64` at a byte-unaligned address.
///
/// # Safety
/// `ptr` must be readable for at least 8 bytes.
#[inline]
unsafe fn read_u64(ptr: *const u8) -> u64 {
    u64::from_le(core::ptr::read_unaligned(ptr as *const u64))
}

// ── Byte offsets into ACPI headers ────────────────────────────────────────────
//
// Instead of `#[repr(C, packed)]` structs + field references (which cause
// E0793 in current stable Rust), we read fields at their known byte offsets.
// Offsets are per ACPI 6.5 §5.2.5 (SDT header) and §5.2.14 (RSDP).

// SDT common header field offsets
#[allow(dead_code)]
const SDT_OFF_SIGNATURE: usize = 0;  // [u8; 4]
const SDT_OFF_LENGTH:    usize = 4;  // u32
// (revision at 8, checksum at 9 — only used as a block for the checksum)

// RSDP field offsets (ACPI 1.0, 20-byte structure)
const RSDP_OFF_SIGNATURE:  usize = 0;  // [u8; 8] = "RSD PTR "
#[allow(dead_code)]
const RSDP_OFF_CHECKSUM:   usize = 8;  // u8  — checksum of first 20 bytes
const RSDP_OFF_REVISION:   usize = 15; // u8  — 0=v1, 2=v2
const RSDP_OFF_RSDT_ADDR:  usize = 16; // u32 — physical address of RSDT
const RSDP_V1_LEN:         usize = 20;

// RSDP extension (ACPI ≥ 2.0, appended at offset 20)
#[allow(dead_code)]
const RSDP_OFF_LENGTH:     usize = 20; // u32 — total length of RSDP v2 struct
const RSDP_OFF_XSDT_ADDR:  usize = 24; // u64 — physical address of XSDT
const RSDP_V2_LEN:         usize = 36;

// ── Generic XSDT / RSDT scanner ───────────────────────────────────────────────

/// Find a child SDT with the given 4-byte `signature` inside an XSDT
/// (ACPI ≥ 2.0, 64-bit entry pointers).
///
/// Returns the physical address of the matching table, or `None` if not found
/// or the XSDT has an invalid checksum or length.
fn find_table_via_xsdt(xsdt_phys: u64, signature: &[u8; 4]) -> Option<u64> {
    let base = xsdt_phys as *const u8;
    let len = unsafe { read_u32(base.add(SDT_OFF_LENGTH)) } as usize;
    if len < SDT_HEADER_LEN { return None; }
    if !checksum_valid(base, len) { return None; }

    let entries_start = xsdt_phys as usize + SDT_HEADER_LEN;
    let entry_count   = (len - SDT_HEADER_LEN) / 8;

    for i in 0..entry_count {
        let entry_ptr = (entries_start + i * 8) as *const u8;
        let child_phys = unsafe { read_u64(entry_ptr) };
        if child_phys == 0 { continue; }

        let sig = unsafe { core::slice::from_raw_parts(child_phys as *const u8, 4) };
        if sig == signature { return Some(child_phys); }
    }
    None
}

/// Find a child SDT with the given 4-byte `signature` inside an RSDT
/// (ACPI 1.0, 32-bit entry pointers).
fn find_table_via_rsdt(rsdt_phys: u64, signature: &[u8; 4]) -> Option<u64> {
    let base = rsdt_phys as *const u8;
    let len = unsafe { read_u32(base.add(SDT_OFF_LENGTH)) } as usize;
    if len < SDT_HEADER_LEN { return None; }
    if !checksum_valid(base, len) { return None; }

    let entries_start = rsdt_phys as usize + SDT_HEADER_LEN;
    let entry_count   = (len - SDT_HEADER_LEN) / 4;

    for i in 0..entry_count {
        let entry_ptr = (entries_start + i * 4) as *const u8;
        let child_phys = unsafe { read_u32(entry_ptr) } as u64;
        if child_phys == 0 { continue; }

        let sig = unsafe { core::slice::from_raw_parts(child_phys as *const u8, 4) };
        if sig == signature { return Some(child_phys); }
    }
    None
}

// ── RSDP entry point ──────────────────────────────────────────────────────────

/// Parse the RSDP at `rsdp_phys` and locate a child SDT by `signature`.
///
/// Prefers XSDT (ACPI ≥ 2.0) over RSDT when both are available.
/// Returns `None` if the RSDP signature or checksum is invalid, or if the
/// requested table cannot be found in any descriptor table.
fn find_table(rsdp_phys: u64, signature: &[u8; 4]) -> Option<u64> {
    if rsdp_phys == 0 { return None; }

    let base = rsdp_phys as *const u8;

    // Signature: "RSD PTR " (8 bytes, note trailing space).
    let sig = unsafe { core::slice::from_raw_parts(base.add(RSDP_OFF_SIGNATURE), 8) };
    if sig != b"RSD PTR " { return None; }

    // v1 checksum covers only the first 20 bytes.
    if !checksum_valid(base, RSDP_V1_LEN) { return None; }

    let revision = unsafe { *base.add(RSDP_OFF_REVISION) };

    if revision >= 2 {
        // Validate the extended checksum (covers all 36 bytes).
        if checksum_valid(base, RSDP_V2_LEN) {
            let xsdt_phys = unsafe { read_u64(base.add(RSDP_OFF_XSDT_ADDR)) };
            if xsdt_phys != 0 {
                if let Some(t) = find_table_via_xsdt(xsdt_phys, signature) {
                    return Some(t);
                }
            }
        }
    }

    // Fall back to RSDT.
    let rsdt_phys = unsafe { read_u32(base.add(RSDP_OFF_RSDT_ADDR)) } as u64;
    if rsdt_phys != 0 { return find_table_via_rsdt(rsdt_phys, signature); }

    None
}

/// Parse the RSDP at `rsdp_phys` and return the physical address of the MADT.
///
/// Prefers XSDT (ACPI ≥ 2.0) over RSDT when both are available.
/// Returns `None` if the RSDP signature or checksum is invalid, or if the MADT
/// cannot be found in any descriptor table.
pub fn find_madt(rsdp_phys: u64) -> Option<u64> {
    find_table(rsdp_phys, b"APIC")
}

/// Parse the RSDP at `rsdp_phys` and return the physical address of the FADT.
///
/// The FADT ("FACP") provides the PM timer I/O port and the ACPI reset register.
/// Returns `None` if the RSDP is invalid or the FADT is not listed in
/// the descriptor table.
pub fn find_fadt(rsdp_phys: u64) -> Option<u64> {
    find_table(rsdp_phys, b"FACP")
}

/// Parse the RSDP at `rsdp_phys` and return the physical address of the DMAR.
///
/// The DMAR ("DMAR") provides the MMIO base addresses of Intel VT-d IOMMU
/// controllers.  Returns `None` if the RSDP is invalid, the platform has no
/// VT-d hardware, or the DMAR is not listed in the descriptor table.
pub fn find_dmar(rsdp_phys: u64) -> Option<u64> {
    find_table(rsdp_phys, b"DMAR")
}


// ── Kept for backward compatibility (used by callers that pass xsdt/rsdt directly) ──

/// Find the MADT inside the XSDT (ACPI ≥ 2.0, 64-bit entry pointers).
pub fn find_madt_via_xsdt(xsdt_phys: u64) -> Option<u64> {
    find_table_via_xsdt(xsdt_phys, b"APIC")
}

/// Find the MADT inside the RSDT (ACPI 1.0, 32-bit entry pointers).
pub fn find_madt_via_rsdt(rsdt_phys: u64) -> Option<u64> {
    find_table_via_rsdt(rsdt_phys, b"APIC")
}
