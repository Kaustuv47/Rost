/// Maximum number of memory regions the map can hold.
/// Typical UEFI systems have 30–60 entries; 128 is a generous upper bound.
pub const MAX_MEMORY_REGIONS: usize = 128;

/// Maximum number of independent display outputs (GOP handles).
pub const MAX_DISPLAYS: usize = 4;

/// Maximum length of the UEFI load-options string (kernel command line), in bytes.
pub const MAX_LOAD_OPTIONS: usize = 256;

/// Maximum length of the UEFI firmware vendor string, in bytes (ASCII-truncated).
pub const MAX_VENDOR_LEN: usize = 64;

/// Maximum length of the CPU brand string ("Intel(R) Core(TM) …"), in bytes.
pub const MAX_BRAND_LEN: usize = 48;

// ── Memory ───────────────────────────────────────────────────────────────────

/// Classification of a physical memory region, derived from UEFI memory types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    /// Free — available for kernel use.
    Usable,
    /// Firmware-reserved; must not be touched.
    Reserved,
    /// ACPI tables — reclaimable after the ACPI subsystem is initialised.
    AcpiReclaimable,
    /// ACPI non-volatile storage — must be preserved at all times.
    AcpiNvs,
    /// Memory-mapped I/O space.
    Mmio,
    /// UEFI boot services code/data.
    /// Reclaimable once `ExitBootServices` has been called.
    BootServices,
    /// UEFI runtime services code/data.
    /// Must be preserved if UEFI runtime calls are needed after boot.
    RuntimeServices,
    /// Memory occupied by the kernel image itself (loader code/data).
    KernelImage,
    /// Unknown or otherwise unusable region.
    Unknown,
}

/// A single contiguous physical memory region.
#[derive(Clone, Copy, Debug)]
pub struct MemoryRegion {
    /// Physical base address (page-aligned).
    pub base: u64,
    /// Size in bytes.
    pub size: u64,
    /// How this region may be used.
    pub kind: MemoryKind,
}

const EMPTY_REGION: MemoryRegion =
    MemoryRegion { base: 0, size: 0, kind: MemoryKind::Unknown };

/// Fixed-capacity list of physical memory regions, populated from the UEFI map.
pub struct MemoryMap {
    regions: [MemoryRegion; MAX_MEMORY_REGIONS],
    count: usize,
}

impl MemoryMap {
    pub const fn new() -> Self {
        MemoryMap { regions: [EMPTY_REGION; MAX_MEMORY_REGIONS], count: 0 }
    }

    /// Append a region; silently drops entries beyond `MAX_MEMORY_REGIONS`.
    pub fn push(&mut self, region: MemoryRegion) {
        if self.count < MAX_MEMORY_REGIONS {
            self.regions[self.count] = region;
            self.count += 1;
        }
    }

    /// All stored regions.
    pub fn regions(&self) -> &[MemoryRegion] {
        &self.regions[..self.count]
    }

    /// Total number of stored regions.
    pub fn len(&self) -> usize { self.count }

    /// `true` if no regions have been recorded yet.
    pub fn is_empty(&self) -> bool { self.count == 0 }

    /// Sum of all `Usable` region sizes in bytes.
    pub fn total_usable_bytes(&self) -> u64 {
        self.regions()
            .iter()
            .filter(|r| r.kind == MemoryKind::Usable)
            .map(|r| r.size)
            .fold(0u64, |acc, s| acc.saturating_add(s))
    }

    /// Number of `Usable` regions.
    pub fn usable_count(&self) -> usize {
        self.regions().iter().filter(|r| r.kind == MemoryKind::Usable).count()
    }

    /// The largest single `Usable` region — the natural home for the physical
    /// frame allocator.
    pub fn largest_usable_region(&self) -> Option<&MemoryRegion> {
        self.regions()
            .iter()
            .filter(|r| r.kind == MemoryKind::Usable)
            .max_by_key(|r| r.size)
    }
}

// ── Display / Framebuffer ────────────────────────────────────────────────────

/// Pixel encoding of a GOP framebuffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameBufferFormat {
    /// 32-bit: R[7:0] G[7:0] B[7:0] x[7:0]
    Rgb32,
    /// 32-bit: B[7:0] G[7:0] R[7:0] x[7:0]
    Bgr32,
    /// Custom bitmask — inspect the mask fields to decode each channel.
    Bitmask { red: u32, green: u32, blue: u32, reserved: u32 },
    /// BLT-only mode; no direct framebuffer access.
    BltOnly,
}

/// GOP (Graphics Output Protocol) framebuffer descriptor.
#[derive(Clone, Copy, Debug)]
pub struct FrameBufferInfo {
    /// Physical base address of the framebuffer.
    pub base: u64,
    /// Total size of the framebuffer in bytes.
    pub size: usize,
    /// Horizontal resolution in pixels.
    pub width: u32,
    /// Vertical resolution in pixels.
    pub height: u32,
    /// Pixels per scan line (≥ width; may be larger for hardware alignment).
    pub stride: u32,
    /// Pixel encoding.
    pub format: FrameBufferFormat,
}

/// All GOP outputs discovered at boot time (up to `MAX_DISPLAYS`).
///
/// `get(0)` / `primary()` returns the first handle opened, which is typically
/// the display the firmware chose as its console.
pub struct DisplayList {
    entries: [Option<FrameBufferInfo>; MAX_DISPLAYS],
    count: usize,
}

impl DisplayList {
    pub const fn new() -> Self {
        DisplayList { entries: [None; MAX_DISPLAYS], count: 0 }
    }

    /// Add a display; ignores overflow beyond `MAX_DISPLAYS`.
    pub fn push(&mut self, fb: FrameBufferInfo) {
        if self.count < MAX_DISPLAYS {
            self.entries[self.count] = Some(fb);
            self.count += 1;
        }
    }

    /// Framebuffer of the n-th display (0-based).
    pub fn get(&self, i: usize) -> Option<&FrameBufferInfo> {
        self.entries.get(i)?.as_ref()
    }

    /// The primary (first) display, if any.
    pub fn primary(&self) -> Option<&FrameBufferInfo> {
        self.get(0)
    }

    /// Number of displays discovered.
    pub fn len(&self) -> usize { self.count }

    /// `true` if no displays were found.
    pub fn is_empty(&self) -> bool { self.count == 0 }
}

// ── ACPI ─────────────────────────────────────────────────────────────────────

/// ACPI firmware table pointer, found in the UEFI configuration table.
#[derive(Clone, Copy, Debug)]
pub struct AcpiInfo {
    /// Physical address of the Root System Description Pointer (RSDP).
    pub rsdp_address: u64,
    /// ACPI revision: 1 for the original RSDP, 2 for the XSDP (ACPI ≥ 2.0).
    pub version: u8,
}

// ── SMBIOS ───────────────────────────────────────────────────────────────────

/// SMBIOS entry-point pointer, found in the UEFI configuration table.
///
/// Pass `address` to the SMBIOS subsystem; `version` tells it which structure
/// format to expect (1/2 → 32-bit entry point, 3 → 64-bit entry point).
#[derive(Clone, Copy, Debug)]
pub struct SmbiosInfo {
    /// Physical address of the SMBIOS entry-point structure.
    pub address: u64,
    /// Entry-point version: 2 for the 32-bit form, 3 for the 64-bit form.
    pub version: u8,
}

// ── Firmware ─────────────────────────────────────────────────────────────────

/// UEFI firmware identification, read from the UEFI System Table header.
#[derive(Clone, Copy, Debug)]
pub struct FirmwareInfo {
    /// OEM firmware vendor name, truncated to ASCII.
    pub vendor: [u8; MAX_VENDOR_LEN],
    /// Number of valid bytes in `vendor`.
    pub vendor_len: usize,
    /// UEFI specification revision encoded as `(major << 16) | minor`.
    /// Access via `uefi_major()` / `uefi_minor()`.
    pub uefi_revision: u32,
    /// OEM-defined firmware revision number.
    pub firmware_revision: u32,
}

impl FirmwareInfo {
    pub const fn new() -> Self {
        FirmwareInfo {
            vendor: [0; MAX_VENDOR_LEN],
            vendor_len: 0,
            uefi_revision: 0,
            firmware_revision: 0,
        }
    }

    /// ASCII slice of the vendor string.
    pub fn vendor_str(&self) -> &[u8] {
        &self.vendor[..self.vendor_len]
    }

    /// UEFI spec major version (e.g. `2` for UEFI 2.x).
    pub fn uefi_major(&self) -> u16 {
        (self.uefi_revision >> 16) as u16
    }

    /// UEFI spec minor version (e.g. `10` for UEFI 2.10).
    pub fn uefi_minor(&self) -> u16 {
        (self.uefi_revision & 0xFFFF) as u16
    }
}

// ── CPU ──────────────────────────────────────────────────────────────────────

/// Feature flags from CPUID, stored as raw bitfields.
///
/// All fields map 1-to-1 to the corresponding CPUID leaf output registers.
/// Helpers like `has_sse()` provide readable access to individual bits.
#[derive(Clone, Copy, Debug)]
pub struct CpuFeatures {
    /// CPUID leaf 1, ECX output (SSE3, PCLMULQDQ, SSSE3, SSE4.1, SSE4.2,
    /// POPCNT, AES-NI, AVX, F16C, RDRAND, …).
    pub leaf1_ecx: u32,
    /// CPUID leaf 1, EDX output (FPU, VME, PSE, PAE, MSR, APIC, MTRR,
    /// PGE, CMOV, PAT, SSE, SSE2, HTT, …).
    pub leaf1_edx: u32,
    /// CPUID leaf 7 subleaf 0, EBX output (FSGSBASE, BMI1, AVX2, BMI2,
    /// SMEP, RDSEED, ADX, SMAP, CLFLUSHOPT, SHA, …).
    pub leaf7_ebx: u32,
    /// CPUID leaf 7 subleaf 0, ECX output (PREFETCHWT1, AVX-512_VBMI,
    /// UMIP, PKU, OSPKE, VAES, VPCLMULQDQ, …).
    pub leaf7_ecx: u32,
}

impl CpuFeatures {
    pub const fn new() -> Self {
        CpuFeatures { leaf1_ecx: 0, leaf1_edx: 0, leaf7_ebx: 0, leaf7_ecx: 0 }
    }

    // ── Leaf 1 EDX ───────────────────────────────────────────────────────────

    /// x87 floating-point unit on-chip.
    pub fn has_fpu(&self) -> bool   { self.leaf1_edx & (1 << 0)  != 0 }
    /// Physical Address Extension (36-bit addresses).
    pub fn has_pae(&self) -> bool   { self.leaf1_edx & (1 << 6)  != 0 }
    /// Model-specific registers.
    pub fn has_msr(&self) -> bool   { self.leaf1_edx & (1 << 5)  != 0 }
    /// APIC on-chip.
    pub fn has_apic(&self) -> bool  { self.leaf1_edx & (1 << 9)  != 0 }
    /// SSE instructions.
    pub fn has_sse(&self) -> bool   { self.leaf1_edx & (1 << 25) != 0 }
    /// SSE2 instructions.
    pub fn has_sse2(&self) -> bool  { self.leaf1_edx & (1 << 26) != 0 }
    /// Hyper-Threading Technology (logical processors per package > 1).
    pub fn has_htt(&self) -> bool   { self.leaf1_edx & (1 << 28) != 0 }

    // ── Leaf 1 ECX ───────────────────────────────────────────────────────────

    /// SSE3 instructions.
    pub fn has_sse3(&self) -> bool    { self.leaf1_ecx & (1 << 0)  != 0 }
    /// PCLMULQDQ carry-less multiply.
    pub fn has_pclmul(&self) -> bool  { self.leaf1_ecx & (1 << 1)  != 0 }
    /// SSSE3 supplemental SSE3.
    pub fn has_ssse3(&self) -> bool   { self.leaf1_ecx & (1 << 9)  != 0 }
    /// SSE4.1 instructions.
    pub fn has_sse4_1(&self) -> bool  { self.leaf1_ecx & (1 << 19) != 0 }
    /// SSE4.2 instructions.
    pub fn has_sse4_2(&self) -> bool  { self.leaf1_ecx & (1 << 20) != 0 }
    /// AES-NI hardware acceleration.
    pub fn has_aes(&self) -> bool     { self.leaf1_ecx & (1 << 25) != 0 }
    /// AVX (256-bit vector extensions).
    pub fn has_avx(&self) -> bool     { self.leaf1_ecx & (1 << 28) != 0 }
    /// RDRAND instruction.
    pub fn has_rdrand(&self) -> bool  { self.leaf1_ecx & (1 << 30) != 0 }
    /// POPCNT instruction.
    pub fn has_popcnt(&self) -> bool  { self.leaf1_ecx & (1 << 23) != 0 }

    // ── Leaf 7 EBX ───────────────────────────────────────────────────────────

    /// AVX2 256-bit integer vector instructions.
    pub fn has_avx2(&self) -> bool     { self.leaf7_ebx & (1 << 5)  != 0 }
    /// BMI1 (ANDN, BEXTR, BLSI, BLSMSK, BLSR, TZCNT).
    pub fn has_bmi1(&self) -> bool     { self.leaf7_ebx & (1 << 3)  != 0 }
    /// BMI2 (BZHI, MULX, PDEP, PEXT, RORX, SARX, SHRX, SHLX).
    pub fn has_bmi2(&self) -> bool     { self.leaf7_ebx & (1 << 8)  != 0 }
    /// RDSEED instruction.
    pub fn has_rdseed(&self) -> bool   { self.leaf7_ebx & (1 << 18) != 0 }
    /// ADCX/ADOX multi-precision add.
    pub fn has_adx(&self) -> bool      { self.leaf7_ebx & (1 << 19) != 0 }
    /// SHA hardware acceleration.
    pub fn has_sha(&self) -> bool      { self.leaf7_ebx & (1 << 29) != 0 }
    /// Supervisor Mode Execution Prevention.
    pub fn has_smep(&self) -> bool     { self.leaf7_ebx & (1 << 7)  != 0 }
    /// Supervisor Mode Access Prevention.
    pub fn has_smap(&self) -> bool     { self.leaf7_ebx & (1 << 20) != 0 }
}

/// CPU identification and feature description, populated via CPUID.
#[derive(Clone, Copy, Debug)]
pub struct CpuInfo {
    /// 12-byte vendor string ("GenuineIntel" or "AuthenticAMD", etc.).
    pub vendor: [u8; 12],
    /// CPU brand string ("Intel(R) Core(TM) i7-…"), ASCII.
    pub brand: [u8; MAX_BRAND_LEN],
    /// Number of valid bytes in `brand`.
    pub brand_len: usize,
    /// Effective CPU family (accounting for extended family).
    pub family: u8,
    /// Effective CPU model (accounting for extended model).
    pub model: u8,
    /// Processor stepping ID.
    pub stepping: u8,
    /// Width of a physical (guest-physical) address in bits (typically 36–52).
    pub physical_address_bits: u8,
    /// Width of a virtual address in bits (typically 48 or 57).
    pub virtual_address_bits: u8,
    /// Maximum number of logical processors per physical package from CPUID.
    pub max_logical_cpus: u32,
    /// CPUID feature flags.
    pub features: CpuFeatures,
}

impl CpuInfo {
    pub const fn new() -> Self {
        CpuInfo {
            vendor: [0; 12],
            brand: [0; MAX_BRAND_LEN],
            brand_len: 0,
            family: 0,
            model: 0,
            stepping: 0,
            physical_address_bits: 0,
            virtual_address_bits: 0,
            max_logical_cpus: 0,
            features: CpuFeatures::new(),
        }
    }

    /// The 12-byte vendor identification string (not null-terminated).
    pub fn vendor_str(&self) -> &[u8] { &self.vendor }

    /// The trimmed CPU brand string (trailing NULs removed).
    pub fn brand_str(&self) -> &[u8] { &self.brand[..self.brand_len] }
}

// ── Secure Boot ──────────────────────────────────────────────────────────────

/// UEFI Secure Boot state read from the `SecureBoot` / `SetupMode` variables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecureBootState {
    /// Could not determine state (firmware bug or variable not present).
    Unknown,
    /// Secure Boot is administratively disabled.
    Disabled,
    /// Secure Boot is fully enabled; unsigned images are rejected.
    Enabled,
    /// Platform is in Setup Mode — keys can be enrolled without authentication.
    SetupMode,
}

// ── Load options (kernel command line) ───────────────────────────────────────

/// Raw load-options data passed by the UEFI boot manager.
///
/// The UEFI spec allows any format; in practice boot managers pass a
/// NUL-terminated UCS-2 string.  This field stores a lossy ASCII projection.
pub struct LoadOptions {
    data: [u8; MAX_LOAD_OPTIONS],
    len: usize,
}

impl LoadOptions {
    pub const fn new() -> Self {
        LoadOptions { data: [0; MAX_LOAD_OPTIONS], len: 0 }
    }

    /// Store raw bytes (already ASCII-projected by the collector).
    pub fn set(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(MAX_LOAD_OPTIONS);
        self.data[..n].copy_from_slice(&bytes[..n]);
        self.len = n;
    }

    /// Byte slice of the stored string.
    pub fn as_bytes(&self) -> &[u8] { &self.data[..self.len] }

    /// Interpret as a UTF-8 string if valid.
    pub fn as_str(&self) -> Option<&str> {
        core::str::from_utf8(self.as_bytes()).ok()
    }

    pub fn is_empty(&self) -> bool { self.len == 0 }
}

// ── Boot time ────────────────────────────────────────────────────────────────

/// Wall-clock time read from UEFI `GetTime()` at the start of kernel init.
#[derive(Clone, Copy, Debug)]
pub struct BootTime {
    pub year:   u16,
    pub month:  u8,
    pub day:    u8,
    pub hour:   u8,
    pub minute: u8,
    pub second: u8,
}

// ── BootInfo — top-level container ───────────────────────────────────────────

/// All hardware and firmware information gathered from UEFI at boot time.
///
/// Populated by `boot_collector::collect()` while UEFI boot services are active,
/// then stored in a `static` so every kernel subsystem can read it indefinitely.
///
/// Design rules:
/// - All fields use fixed-size arrays so the struct can live in `.bss`.
/// - `BootInfo::new()` is `const fn` — suitable as a `static` initialiser.
/// - The collector leaves fields at their default (zero / `None` / `Unknown`)
///   on any error, so callers must treat `Option` / `Unknown` as "not found".
pub struct BootInfo {
    // ── Memory layout ────────────────────────────────────────────────────────

    /// Physical memory map as reported by UEFI.
    pub memory_map: MemoryMap,
    /// Sum of all `Usable` region sizes (convenience cache).
    pub total_memory_bytes: u64,

    // ── Display / GPU ─────────────────────────────────────────────────────────

    /// All GOP framebuffers found.  `displays.primary()` is the boot console.
    pub displays: DisplayList,

    // ── Platform firmware tables ──────────────────────────────────────────────

    /// ACPI Root System Description Pointer, if the firmware exposed one.
    pub acpi: Option<AcpiInfo>,
    /// SMBIOS entry-point pointer, if present.
    pub smbios: Option<SmbiosInfo>,

    // ── Firmware / platform identity ─────────────────────────────────────────

    /// UEFI firmware vendor and revision.
    pub firmware: FirmwareInfo,

    // ── Processor ─────────────────────────────────────────────────────────────

    /// CPU identification and feature flags (from CPUID).
    pub cpu: CpuInfo,

    // ── Boot environment ──────────────────────────────────────────────────────

    /// Secure Boot state at the time of kernel entry.
    pub secure_boot: SecureBootState,
    /// Kernel command line / load options from the boot manager.
    pub load_options: LoadOptions,
    /// Wall-clock time read at kernel entry.
    pub boot_time: Option<BootTime>,
}

impl BootInfo {
    pub const fn new() -> Self {
        BootInfo {
            memory_map:        MemoryMap::new(),
            total_memory_bytes: 0,
            displays:          DisplayList::new(),
            acpi:              None,
            smbios:            None,
            firmware:          FirmwareInfo::new(),
            cpu:               CpuInfo::new(),
            secure_boot:       SecureBootState::Unknown,
            load_options:      LoadOptions::new(),
            boot_time:         None,
        }
    }
}
