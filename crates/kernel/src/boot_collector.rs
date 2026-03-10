/// Collects all hardware information from UEFI before boot services are lost.
///
/// Call `collect()` while `SystemTable<Boot>` is still valid.
/// The returned `BootInfo` is fully self-contained and remains usable forever.
use uefi::prelude::*;
use uefi::table::boot::MemoryType;
use uefi::table::runtime::VariableVendor;
use uefi::table::cfg;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};
use uefi::proto::loaded_image::LoadedImage;
use uefi::cstr16;

use core_kernel::boot_info::{
    AcpiInfo, BootInfo, BootTime, CpuInfo,
    FrameBufferFormat, FrameBufferInfo, FirmwareInfo,
    MemoryKind, MemoryMap, MemoryRegion,
    SecureBootState, SmbiosInfo,
};

// ── Entry point ──────────────────────────────────────────────────────────────

/// Populate a `BootInfo` from all available UEFI sources.
/// Safe to call; individual sections degrade gracefully on any error.
pub fn collect(image_handle: Handle, system_table: &SystemTable<Boot>) -> BootInfo {
    let mut info = BootInfo::new();
    let bt = system_table.boot_services();

    collect_firmware(system_table, &mut info.firmware);
    collect_cpu(&mut info.cpu);
    collect_memory(bt, &mut info.memory_map);
    collect_acpi_and_smbios(system_table, &mut info);
    collect_displays(bt, &mut info);
    collect_secure_boot(system_table, &mut info);
    collect_load_options(image_handle, bt, &mut info);
    collect_boot_time(system_table, &mut info);

    info.total_memory_bytes = info.memory_map.total_usable_bytes();
    info
}

// ── Firmware identity ─────────────────────────────────────────────────────────

fn collect_firmware(system_table: &SystemTable<Boot>, fw: &mut FirmwareInfo) {
    // Vendor string: CStr16 → ASCII bytes
    let vendor_cstr = system_table.firmware_vendor();
    let mut len = 0usize;
    for c in vendor_cstr.iter() {
        if len >= fw.vendor.len() - 1 { break; }
        let code = u16::from(*c);
        if code > 0 && code < 128 {
            fw.vendor[len] = code as u8;
            len += 1;
        }
    }
    fw.vendor_len = len;

    // UEFI revision is a Revision newtype; .0 gives the raw u32.
    fw.uefi_revision     = system_table.uefi_revision().0;
    fw.firmware_revision = system_table.firmware_revision();
}

// ── CPU — CPUID ──────────────────────────────────────────────────────────────

/// Execute a CPUID instruction.
///
/// LLVM reserves `rbx` for its own use, so we save/restore it manually
/// around the instruction and capture the EBX output via a scratch register.
fn cpuid(leaf: u32, subleaf: u32) -> (u32, u32, u32, u32) {
    let eax: u32;
    let ebx: u32;
    let ecx_out: u32;
    let edx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "mov {ebx_out:e}, ebx",
            "pop rbx",
            ebx_out = out(reg) ebx,
            inout("eax") leaf    => eax,
            inout("ecx") subleaf => ecx_out,
            out("edx") edx,
        );
    }
    (eax, ebx, ecx_out, edx)
}

fn collect_cpu(cpu: &mut CpuInfo) {
    // ── Leaf 0: max basic leaf + vendor string ────────────────────────────────
    let (max_basic, ebx, ecx, edx) = cpuid(0, 0);

    // Vendor is packed as EBX:EDX:ECX (in that order per Intel spec)
    cpu.vendor[0..4].copy_from_slice(&ebx.to_le_bytes());
    cpu.vendor[4..8].copy_from_slice(&edx.to_le_bytes());
    cpu.vendor[8..12].copy_from_slice(&ecx.to_le_bytes());

    // ── Leaf 1: family / model / stepping + feature flags ────────────────────
    if max_basic >= 1 {
        let (eax1, ebx1, ecx1, edx1) = cpuid(1, 0);

        let family_id      = (eax1 >> 8)  & 0xF;
        let extended_family = (eax1 >> 20) & 0xFF;
        let model_id       = (eax1 >> 4)  & 0xF;
        let extended_model = (eax1 >> 16) & 0xF;

        // Intel / AMD convention for effective family and model
        cpu.family = if family_id == 0xF {
            (extended_family + 0xF) as u8
        } else {
            family_id as u8
        };
        cpu.model = if family_id == 0xF || family_id == 0x6 {
            ((extended_model << 4) | model_id) as u8
        } else {
            model_id as u8
        };
        cpu.stepping = (eax1 & 0xF) as u8;

        // Logical processor count per package (leaf 1 EBX bits[23:16])
        cpu.max_logical_cpus = (ebx1 >> 16) & 0xFF;

        cpu.features.leaf1_ecx = ecx1;
        cpu.features.leaf1_edx = edx1;
    }

    // ── Leaf 7: extended structured features ─────────────────────────────────
    if max_basic >= 7 {
        let (_eax7, ebx7, ecx7, _edx7) = cpuid(7, 0);
        cpu.features.leaf7_ebx = ebx7;
        cpu.features.leaf7_ecx = ecx7;
    }

    // ── Extended leaves: max extended leaf ────────────────────────────────────
    let (max_ext, _, _, _) = cpuid(0x8000_0000, 0);

    // ── Extended leaf 0x80000001: extended flags (for future use) ────────────
    // (stored in CpuFeatures if fields are added later)

    // ── Extended leaves 0x80000002-4: brand string ───────────────────────────
    if max_ext >= 0x8000_0004 {
        let mut brand = [0u8; 48];
        for i in 0..3u32 {
            let (a, b, c, d) = cpuid(0x8000_0002 + i, 0);
            let off = (i as usize) * 16;
            brand[off..off +  4].copy_from_slice(&a.to_le_bytes());
            brand[off +  4..off +  8].copy_from_slice(&b.to_le_bytes());
            brand[off +  8..off + 12].copy_from_slice(&c.to_le_bytes());
            brand[off + 12..off + 16].copy_from_slice(&d.to_le_bytes());
        }
        // Trim trailing NULs; brand string may have leading spaces on some Intel CPUs.
        let end = brand.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
        cpu.brand[..end].copy_from_slice(&brand[..end]);
        cpu.brand_len = end;
    }

    // ── Extended leaf 0x80000008: address size ────────────────────────────────
    if max_ext >= 0x8000_0008 {
        let (eax_addr, _, _, _) = cpuid(0x8000_0008, 0);
        cpu.physical_address_bits = (eax_addr & 0xFF) as u8;
        cpu.virtual_address_bits  = ((eax_addr >> 8) & 0xFF) as u8;
    }
}

// ── Physical memory map ──────────────────────────────────────────────────────

fn collect_memory(bt: &BootServices, map: &mut MemoryMap) {
    // Vec<u64> guarantees 8-byte alignment, as required by the UEFI spec.
    // 1 024 × 8 = 8 KiB — more than enough for any real system.
    let mut buf = alloc::vec![0u64; 1024];
    let byte_buf = unsafe {
        core::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8)
    };

    if let Ok(mmap) = bt.memory_map(byte_buf) {
        for desc in mmap.entries() {
            map.push(MemoryRegion {
                base: desc.phys_start,
                size: desc.page_count * 4096,
                kind: uefi_memory_type_to_kind(desc.ty),
            });
        }
    }
}

fn uefi_memory_type_to_kind(ty: MemoryType) -> MemoryKind {
    if      ty == MemoryType::CONVENTIONAL          { MemoryKind::Usable           }
    else if ty == MemoryType::RESERVED              { MemoryKind::Reserved         }
    else if ty == MemoryType::ACPI_RECLAIM          { MemoryKind::AcpiReclaimable  }
    else if ty == MemoryType::ACPI_NON_VOLATILE     { MemoryKind::AcpiNvs          }
    else if ty == MemoryType::MMIO                  { MemoryKind::Mmio             }
    else if ty == MemoryType::MMIO_PORT_SPACE       { MemoryKind::Mmio             }
    else if ty == MemoryType::BOOT_SERVICES_CODE
         || ty == MemoryType::BOOT_SERVICES_DATA    { MemoryKind::BootServices     }
    else if ty == MemoryType::RUNTIME_SERVICES_CODE
         || ty == MemoryType::RUNTIME_SERVICES_DATA { MemoryKind::RuntimeServices  }
    else if ty == MemoryType::LOADER_CODE
         || ty == MemoryType::LOADER_DATA           { MemoryKind::KernelImage      }
    else                                             { MemoryKind::Unknown          }
}

// ── ACPI + SMBIOS (both live in the config table) ────────────────────────────

fn collect_acpi_and_smbios(system_table: &SystemTable<Boot>, info: &mut BootInfo) {
    // Scan once; collect all three table types in a single pass.
    let mut acpi1_addr: Option<u64> = None;

    for entry in system_table.config_table() {
        if entry.guid == cfg::ACPI2_GUID {
            // ACPI 2.0+ (XSDP) — preferred; stop looking for ACPI.
            info.acpi = Some(AcpiInfo { rsdp_address: entry.address as u64, version: 2 });
        } else if entry.guid == cfg::ACPI_GUID && info.acpi.is_none() {
            // ACPI 1.0 — keep as fallback if 2.0 not found yet.
            acpi1_addr = Some(entry.address as u64);
        } else if entry.guid == cfg::SMBIOS3_GUID {
            // SMBIOS 3.0 (64-bit) — preferred.
            info.smbios = Some(SmbiosInfo { address: entry.address as u64, version: 3 });
        } else if entry.guid == cfg::SMBIOS_GUID && info.smbios.is_none() {
            // SMBIOS 2.x (32-bit) — fallback.
            info.smbios = Some(SmbiosInfo { address: entry.address as u64, version: 2 });
        }
    }

    // Apply ACPI 1.0 address only if 2.0 wasn't found.
    if info.acpi.is_none() {
        if let Some(addr) = acpi1_addr {
            info.acpi = Some(AcpiInfo { rsdp_address: addr, version: 1 });
        }
    }
}

// ── Display / GOP ─────────────────────────────────────────────────────────────

fn collect_displays(bt: &BootServices, info: &mut BootInfo) {
    // find_handles returns all handles that implement GraphicsOutput.
    let Ok(handles) = bt.find_handles::<GraphicsOutput>() else { return };

    for handle in handles.iter() {
        let Ok(mut gop) = bt.open_protocol_exclusive::<GraphicsOutput>(*handle) else { continue };

        // Copy ModeInfo before calling frame_buffer() which takes &mut self.
        let mode = gop.current_mode_info();
        let (width, height) = mode.resolution();

        let format = match mode.pixel_format() {
            PixelFormat::Rgb     => FrameBufferFormat::Rgb32,
            PixelFormat::Bgr     => FrameBufferFormat::Bgr32,
            PixelFormat::BltOnly => FrameBufferFormat::BltOnly,
            PixelFormat::Bitmask => {
                if let Some(bm) = mode.pixel_bitmask() {
                    FrameBufferFormat::Bitmask {
                        red:      bm.red,
                        green:    bm.green,
                        blue:     bm.blue,
                        reserved: bm.reserved,
                    }
                } else {
                    FrameBufferFormat::Bgr32 // defensive fallback
                }
            }
        };

        let mut fb = gop.frame_buffer();
        info.displays.push(FrameBufferInfo {
            base:   fb.as_mut_ptr() as u64,
            size:   fb.size(),
            width:  width  as u32,
            height: height as u32,
            stride: mode.stride() as u32,
            format,
        });
    }
}

// ── Secure Boot ───────────────────────────────────────────────────────────────

fn collect_secure_boot(system_table: &SystemTable<Boot>, info: &mut BootInfo) {
    let rt = system_table.runtime_services();
    let mut buf = [0u8; 4];

    // Read SecureBoot variable (0 = disabled, 1 = enabled).
    let secure_boot_value = rt
        .get_variable(cstr16!("SecureBoot"), &VariableVendor::GLOBAL_VARIABLE, &mut buf)
        .ok()
        .and_then(|(data, _)| data.first().copied());

    // Read SetupMode variable (1 = key enrollment mode, Secure Boot not enforced).
    let setup_mode_value = rt
        .get_variable(cstr16!("SetupMode"), &VariableVendor::GLOBAL_VARIABLE, &mut buf)
        .ok()
        .and_then(|(data, _)| data.first().copied());

    info.secure_boot = match (secure_boot_value, setup_mode_value) {
        (_, Some(1))    => SecureBootState::SetupMode,
        (Some(1), _)    => SecureBootState::Enabled,
        (Some(0), _)    => SecureBootState::Disabled,
        _               => SecureBootState::Unknown,
    };
}

// ── Load options (kernel command line) ───────────────────────────────────────

fn collect_load_options(
    image_handle: Handle,
    bt: &BootServices,
    info: &mut BootInfo,
) {
    let Ok(img) = bt.open_protocol_exclusive::<LoadedImage>(image_handle) else { return };

    if let Some(raw_bytes) = img.load_options_as_bytes() {
        // Load options are a UCS-2 string (2 bytes per char).
        // Project to ASCII: skip high bytes, keep printable low bytes.
        if raw_bytes.len() >= 2 && raw_bytes.len() % 2 == 0 {
            let mut ascii = alloc::vec![0u8; raw_bytes.len() / 2];
            let mut ascii_len = 0usize;
            for chunk in raw_bytes.chunks_exact(2) {
                let code = u16::from_le_bytes([chunk[0], chunk[1]]);
                if code == 0 { break; } // NUL terminator
                if code < 128 {
                    ascii[ascii_len] = code as u8;
                    ascii_len += 1;
                }
            }
            info.load_options.set(&ascii[..ascii_len]);
        } else {
            // Not UCS-2; store as-is (may be a custom format).
            info.load_options.set(raw_bytes);
        }
    }
}

// ── Boot time ─────────────────────────────────────────────────────────────────

fn collect_boot_time(system_table: &SystemTable<Boot>, info: &mut BootInfo) {
    if let Ok(time) = system_table.runtime_services().get_time() {
        if time.is_valid() {
            info.boot_time = Some(BootTime {
                year:   time.year(),
                month:  time.month(),
                day:    time.day(),
                hour:   time.hour(),
                minute: time.minute(),
                second: time.second(),
            });
        }
    }
}
