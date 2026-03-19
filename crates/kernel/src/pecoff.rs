/// Minimal PE32+ (PE/COFF) section-table parser for the kernel image.
///
/// The Rost kernel is a UEFI application, so the linker emits a PE32+ binary.
/// At boot, `LoadedImage.info()` gives us the physical base address of the
/// loaded image; this module walks the PE headers to locate the per-section
/// virtual addresses and sizes so the memory manager can apply per-section PTE
/// flags (read-only .text, NX .data, etc.).
///
/// Only the fields needed for section-flag remapping are extracted.  The parser
/// is intentionally minimal — no allocation, no unsafe beyond pointer reads.
///
/// Reference: PE/COFF specification §3 (Image Format).

// ── Constants ────────────────────────────────────────────────────────────────

/// DOS header "MZ" magic.
const DOS_MAGIC:    u16 = 0x5A4D;
/// PE signature "PE\0\0".
const PE_SIGNATURE: u32 = 0x0000_4550;
/// COFF machine type for AMD64.
const MACHINE_AMD64: u16 = 0x8664;
/// PE32+ optional header magic.
const PE32PLUS_MAGIC: u16 = 0x020B;

// ── Data types ───────────────────────────────────────────────────────────────

/// The virtual address range occupied by a single PE section.
///
/// Under UEFI identity mapping `virt_addr` equals the physical address.
#[derive(Copy, Clone, Debug)]
pub struct SectionInfo {
    /// Absolute virtual (= physical) address of the section's first byte.
    pub virt_addr: u64,
    /// Virtual size in bytes (as declared in the section header).
    pub size: u64,
}

/// Kernel section boundaries extracted from the PE/COFF header.
///
/// Sections that are absent in the image are `None`.
#[derive(Default, Debug)]
pub struct KernelSections {
    /// `.text` — executable code; map read+execute (no WRITABLE, no NX).
    pub text:   Option<SectionInfo>,
    /// `.rdata` — read-only data; map read+NX (no WRITABLE).
    pub rodata: Option<SectionInfo>,
    /// `.data` — writable initialised data; map read+write+NX.
    pub data:   Option<SectionInfo>,
    /// `.bss` — zeroed data (may be merged into `.data`); read+write+NX.
    pub bss:    Option<SectionInfo>,
}

// ── Parser ───────────────────────────────────────────────────────────────────

/// Parse the PE/COFF section table of the kernel image and return section
/// boundaries.
///
/// # Safety
/// `image_base` must be the physical/virtual base of a fully-loaded PE32+
/// image whose headers are accessible under the current mapping.  The call
/// must occur while UEFI identity mapping (or the kernel's own identity
/// mapping) is active.
///
/// Returns a zero-filled `KernelSections` on any parse error (bad magic,
/// wrong architecture, truncated table).
pub unsafe fn parse_kernel_sections(image_base: u64) -> KernelSections {
    let mut sections = KernelSections::default();

    if image_base == 0 { return sections; }

    let base = image_base as usize;

    // ── DOS header ───────────────────────────────────────────────────────────
    // Offset 0x00: WORD Magic ("MZ")
    if read_u16(base, 0) != DOS_MAGIC { return sections; }
    // Offset 0x3C: DWORD e_lfanew — file offset of the PE signature.
    let e_lfanew = read_u32(base, 0x3C) as usize;

    // ── PE signature ─────────────────────────────────────────────────────────
    if read_u32(base, e_lfanew) != PE_SIGNATURE { return sections; }

    // ── COFF file header (immediately after the 4-byte PE signature) ─────────
    // +0: Machine (u16)
    let machine = read_u16(base, e_lfanew + 4);
    if machine != MACHINE_AMD64 { return sections; }

    // +2: NumberOfSections (u16)
    let num_sections = read_u16(base, e_lfanew + 6) as usize;
    // +16: SizeOfOptionalHeader (u16)
    let opt_hdr_size = read_u16(base, e_lfanew + 20) as usize;

    // ── Optional header — verify PE32+ magic ─────────────────────────────────
    // Optional header starts at e_lfanew + 4 (sig) + 20 (COFF header) = +24.
    if opt_hdr_size < 2 { return sections; }
    let opt_off = e_lfanew + 24;
    if read_u16(base, opt_off) != PE32PLUS_MAGIC { return sections; }

    // ── Section table ────────────────────────────────────────────────────────
    // Starts immediately after the optional header.
    let sec_table_off = opt_off + opt_hdr_size;

    for i in 0..num_sections {
        let s = sec_table_off + i * 40;

        // Name: 8 bytes, null-padded (not necessarily null-terminated if 8 chars).
        let name = read_name(base, s);

        // VirtualSize (u32) at offset +8 within the section header.
        let vsize = read_u32(base, s + 8) as u64;
        // VirtualAddress (RVA, u32) at offset +12.
        let rva   = read_u32(base, s + 12) as u64;

        // Skip empty sections.
        if vsize == 0 || rva == 0 { continue; }

        let info = SectionInfo {
            virt_addr: image_base + rva,
            size: vsize,
        };

        if name_eq(&name, b".text") {
            sections.text = Some(info);
        } else if name_eq(&name, b".rdata") {
            sections.rodata = Some(info);
        } else if name_eq(&name, b".data") {
            sections.data = Some(info);
        } else if name_eq(&name, b".bss") {
            sections.bss = Some(info);
        }
    }

    sections
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Read a little-endian `u16` from `base + off` (unaligned).
#[inline]
unsafe fn read_u16(base: usize, off: usize) -> u16 {
    ((base + off) as *const u16).read_unaligned()
}

/// Read a little-endian `u32` from `base + off` (unaligned).
#[inline]
unsafe fn read_u32(base: usize, off: usize) -> u32 {
    ((base + off) as *const u32).read_unaligned()
}

/// Read the 8-byte section name field.
#[inline]
unsafe fn read_name(base: usize, sec_off: usize) -> [u8; 8] {
    let ptr = (base + sec_off) as *const [u8; 8];
    *ptr
}

/// Compare a section name against a short ASCII prefix, treating any
/// null byte or `$` as the end of the name (e.g. `.text$mn` matches `.text`).
fn name_eq(name: &[u8; 8], prefix: &[u8]) -> bool {
    let n = prefix.len().min(8);
    if &name[..n] != &prefix[..n] { return false; }
    // After the prefix, the name must end (NUL, `$`, or at position 8).
    n == 8 || name[n] == 0 || name[n] == b'$'
}
