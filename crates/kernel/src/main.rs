#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;
use uefi::prelude::*;
use uefi::table::boot::MemoryType;
use core::alloc::{GlobalAlloc, Layout};

mod boot_collector;
mod pecoff;
pub mod elf;

// ── Embedded ring-3 server binaries ──────────────────────────────────────────
//
// The build script (scripts/build.sh) compiles the servers/ workspace before
// this crate, so these paths are valid at kernel compile time.
//
// Boot spawn order (determines PID assignment):
//   PID 1 — rost-init      (health monitor — MUST be first; fault handler targets PID 1)
//   PID 2 — kernel idle process (priority 255)
//   PID 3 — rost-uart-drv  (UART driver; shell sends bytes here)
//   PID 4 — rost-vfs       (virtual filesystem)
//   PID 5 — rost-shell     (interactive shell)
//   PID 6 — rost-net       (network stack: virtio-net + ARP/IPv4/ICMPv4/UDP/TCP)
static INIT_ELF: &[u8] = include_bytes!(
    "../../../servers/target/x86_64-unknown-none/debug/rost-init"
);
static UART_DRV_ELF: &[u8] = include_bytes!(
    "../../../servers/target/x86_64-unknown-none/debug/rost-uart-drv"
);
static VFS_ELF: &[u8] = include_bytes!(
    "../../../servers/target/x86_64-unknown-none/debug/rost-vfs"
);
static SHELL_ELF: &[u8] = include_bytes!(
    "../../../servers/target/x86_64-unknown-none/debug/rost-shell"
);
static NET_ELF: &[u8] = include_bytes!(
    "../../../servers/target/x86_64-unknown-none/debug/rost-net"
);

use arch_x86_64::cpu::{GlobalDescriptorTable, InterruptDescriptorTable};
use core_kernel::boot_info::BootInfo;

// =============================================================================
// GLOBAL ALLOCATOR
// =============================================================================

pub struct BumpAllocator {
    heap: [u8; 0x100000], // 1 MB
    offset: core::sync::atomic::AtomicUsize,
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let offset = self.offset.load(core::sync::atomic::Ordering::Relaxed);
        let aligned = (offset + layout.align() - 1) & !(layout.align() - 1);
        let new_offset = aligned + layout.size();
        if new_offset >= self.heap.len() {
            // OOM: log over serial then enter safe state.
            hal::uart::print_str("\n[OOM] Kernel heap exhausted — system halted.\n");
            loop { unsafe { core::arch::asm!("cli", "hlt", options(nostack, nomem)); } }
        }
        self.offset.store(new_offset, core::sync::atomic::Ordering::Relaxed);
        self.heap.as_ptr().add(aligned) as *mut u8
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    heap: [0; 0x100000],
    offset: core::sync::atomic::AtomicUsize::new(0),
};

// GDT must be static mut so we can install the TSS descriptor at runtime.
// IDT is static mut because we register handlers before loading it.
static mut GDT: GlobalDescriptorTable = GlobalDescriptorTable::new();
static mut IDT: InterruptDescriptorTable = InterruptDescriptorTable::new();

// Kernel PML4 — must be static so it is 4 KB-aligned (from PageTable's repr)
// and lives for the entire kernel lifetime.  BSS-initialised to all-zeros.
static mut KERNEL_PML4: core_kernel::memory::PageTable = core_kernel::memory::PageTable::new();

/// Physical address of the Local APIC MMIO registers, extracted from the MADT.
///
/// Populated during Stage 1 (MADT parse).  Used by the Local APIC driver (§4)
/// to enable the LAPIC and switch from 8259 PIC to APIC-based IRQ delivery.
/// 0 means the MADT was not found or the system has no Local APIC.
static mut LAPIC_PHYS_ADDR: u64 = 0;

/// Physical address of the primary I/O APIC MMIO registers, extracted from the MADT.
///
/// 0 means no I/O APIC entry was found in the MADT.
static mut IOAPIC_PHYS_ADDR: u64 = 0;

/// GSI base of the primary I/O APIC (offset between IOAPIC pin and global system interrupt).
///
/// Typically 0 for the first I/O APIC.  Used so that IRQ → GSI remapping is correct.
static mut IOAPIC_GSI_BASE: u32 = 0;

/// I/O port of the ACPI PM timer, extracted from the FADT.
///
/// 0 means the FADT was not found or the PM timer block is absent.
/// Used by the future TSC calibration routine (§10) and SYS_CLOCK improvement.
static mut PM_TIMER_PORT: u32 = 0;

/// Physical base addresses of discovered Intel VT-d IOMMU controllers, from DMAR.
///
/// Populated during Stage 1 (DMAR parse).  Used by the future IOMMU init (§4)
/// to enable DMA remapping.  Zero entries mean no VT-d hardware was found.
static mut IOMMU_BASES: [u64; core_kernel::acpi::dmar::MAX_DRH_UNITS] =
    [0u64; core_kernel::acpi::dmar::MAX_DRH_UNITS];
static mut IOMMU_COUNT: usize = 0;

// Hardware description gathered from UEFI; remains valid after boot services exit.
static mut BOOT_INFO: BootInfo = BootInfo::new();

// =============================================================================
// IDLE PROCESS
// =============================================================================

/// Idle process entry point — runs when no other process is Ready.
///
/// Priority 255 (lowest) ensures this is only scheduled when the run queue
/// is otherwise empty.  The `hlt` instruction suspends the CPU until the next
/// interrupt, avoiding a busy-wait that would burn power and starve the timer.
///
/// Interrupts are explicitly enabled at entry because this process may be
/// scheduled for the first time via the timer ISR (which runs with IF=0); the
/// IRETQ in the ISR restores RFLAGS but a fresh context switch via
/// `switch_context_noints` does not call `sti`.
extern "C" fn idle_process() -> ! {
    arch_x86_64::cpu::enable_interrupts();
    let mut tick_count: u64 = 0;
    loop {
        arch_x86_64::cpu::halt();
        tick_count += 1;
        // Pet the hardware watchdog every 50 ticks (500 ms at 100 Hz).
        // If the scheduler stops delivering ticks, the system resets.
        // IEC 61508 §7.4.9: hardware watchdog supervision is mandatory for SIL-4.
        if tick_count % 50 == 0 {
            hal::watchdog::kick();
        }
    }
}

// =============================================================================
// ELF SPAWN HOOK
// =============================================================================

/// Kernel-side ELF spawn hook registered with `core_kernel::elf_spawn`.
///
/// Called by the `SYS_SPAWN_ELF` syscall handler (in `arch-x86_64`) with a
/// pointer to an ELF image in the calling process's user-space memory.
/// Returns the new PID on success or `u32::MAX` on any failure.
///
/// # Safety
/// The caller (syscall dispatcher) guarantees:
///   - `[ptr, ptr + len)` is readable (STAC is active).
///   - The slice does not alias any active kernel data structures.
unsafe fn elf_spawn_kernel_hook(ptr: *const u8, len: usize, priority: u8) -> u32 {
    let data = core::slice::from_raw_parts(ptr, len);
    match elf::spawn_elf(data, priority) {
        Some(pid) => pid.as_u32(),
        None      => u32::MAX,
    }
}

/// Kernel-side server restart hook registered with `core_kernel::elf_spawn`.
///
/// Called by `SYS_RESTART_SERVER` (27) when init asks the kernel to respawn
/// a named server.  The kernel already holds the embedded ELF image for every
/// well-known server, so no user-space buffer is needed.
///
/// Name matching uses `starts_with` on the 16-byte user buffer so that both
/// null-padded ("uart-drv\0\0\0\0\0\0\0\0") and bare ("uart-drv") names work.
///
/// # Safety
/// The caller (syscall dispatcher) guarantees:
///   - `[name_ptr, name_ptr + 16)` is readable (STAC is active).
unsafe fn restart_server_hook(name_ptr: *const u8, name_len: usize) -> u32 {
    let name = core::slice::from_raw_parts(name_ptr, name_len);
    // Strip trailing null bytes for comparison.
    let end = name.iter().position(|&b| b == 0).unwrap_or(name_len);
    let name = &name[..end];

    let (elf_data, priority): (&[u8], u8) = if name == b"uart-drv" {
        (UART_DRV_ELF, 64)
    } else if name == b"rost-vfs" {
        (VFS_ELF, 64)
    } else if name == b"rost-shell" {
        (SHELL_ELF, 64)
    } else if name == b"rost-net" {
        (NET_ELF, 48)
    } else {
        return u32::MAX; // unknown server name
    };

    match elf::spawn_elf(elf_data, priority) {
        Some(pid) => pid.as_u32(),
        None      => u32::MAX,
    }
}

// =============================================================================
// ENTRY POINT
// =============================================================================

#[entry]
fn efi_main(image_handle: Handle, system_table: SystemTable<Boot>) -> Status {
    hal::uart::init();
    hal::uart::print_str("\n");
    hal::uart::print_str("╔════════════════════════════════════╗\n");
    hal::uart::print_str("║   Rost Microkernel v0.1.0         ║\n");
    hal::uart::print_str("║   UEFI-based x86_64 Kernel        ║\n");
    hal::uart::print_str("╚════════════════════════════════════╝\n");
    hal::uart::print_str("\n=== INITIALIZATION SEQUENCE ===\n\n");

    // -------------------------------------------------------------------------
    // STAGE 0: UEFI Hardware Discovery
    // -------------------------------------------------------------------------
    hal::uart::print_str("[0/7] UEFI Hardware Discovery\n");

    // Collect all UEFI-provided hardware data while boot services are alive.
    unsafe {
        *core::ptr::addr_of_mut!(BOOT_INFO) =
            boot_collector::collect(image_handle, &system_table);
    }

    // Exit UEFI Boot Services — take exclusive ownership of all hardware.
    //
    // IEC 61508 §7.2.1 requires the safety software to have exclusive control
    // of every hardware resource it relies on.  Leaving UEFI boot services
    // active allows firmware timer callbacks and memory-management routines to
    // run concurrently with kernel initialisation, violating this requirement.
    //
    // `exit_boot_services` consumes `system_table` so no UEFI boot-services
    // call can be issued after this point.  The returned `RuntimeServices`
    // table (UEFI battery-backed clock, NVRAM, etc.) is retained for future
    // use; the final memory map is discarded because we captured the map we
    // need inside `collect()` above.
    //
    // Failure to call `ExitBootServices` is treated as a cold reset (the uefi
    // crate resets the machine internally if the call cannot be completed after
    // two retries).
    let (_runtime_table, _final_mmap) =
        system_table.exit_boot_services(MemoryType::LOADER_DATA);
    // `system_table` is gone; boot services are permanently unavailable.

    let boot_info = unsafe { &*core::ptr::addr_of!(BOOT_INFO) };

    // Firmware
    hal::uart::print_str("      ├─ Firmware:        ");
    hal::uart::print_str(core::str::from_utf8(boot_info.firmware.vendor_str()).unwrap_or("?"));
    hal::uart::print_str(" UEFI ");
    hal::uart::print_dec(boot_info.firmware.uefi_major() as u64);
    hal::uart::print_str(".");
    hal::uart::print_dec(boot_info.firmware.uefi_minor() as u64);
    hal::uart::print_str("\n");

    // CPU
    hal::uart::print_str("      ├─ CPU vendor:      ");
    hal::uart::print_str(core::str::from_utf8(&boot_info.cpu.vendor).unwrap_or("?"));
    hal::uart::print_str("\n");
    if !boot_info.cpu.brand_str().is_empty() {
        hal::uart::print_str("      ├─ CPU brand:       ");
        // brand_str may contain leading spaces on some Intel CPUs — trim them
        let brand = boot_info.cpu.brand_str();
        let trimmed = brand.iter().position(|&b| b != b' ').map_or(brand, |i| &brand[i..]);
        hal::uart::print_str(core::str::from_utf8(trimmed).unwrap_or("?"));
        hal::uart::print_str("\n");
    }
    hal::uart::print_str("      ├─ CPU addr bits:   phys=");
    hal::uart::print_dec(boot_info.cpu.physical_address_bits as u64);
    hal::uart::print_str(" virt=");
    hal::uart::print_dec(boot_info.cpu.virtual_address_bits as u64);
    hal::uart::print_str("\n");

    // Memory
    hal::uart::print_str("      ├─ Memory regions:  ");
    hal::uart::print_dec(boot_info.memory_map.len() as u64);
    hal::uart::print_str(" entries (");
    hal::uart::print_dec(boot_info.memory_map.usable_count() as u64);
    hal::uart::print_str(" usable)\n");
    hal::uart::print_str("      ├─ Usable RAM:      ");
    print_mib(boot_info.total_memory_bytes);
    hal::uart::print_str(" MiB\n");

    // Display
    if let Some(fb) = boot_info.displays.primary() {
        hal::uart::print_str("      ├─ Display (GOP):   ");
        hal::uart::print_dec(fb.width as u64);
        hal::uart::print_str("x");
        hal::uart::print_dec(fb.height as u64);
        hal::uart::print_str(" @ ");
        hal::uart::print_hex(fb.base);
        hal::uart::print_str("  [");
        hal::uart::print_dec(boot_info.displays.len() as u64);
        hal::uart::print_str(" output(s)]\n");
    } else {
        hal::uart::print_str("      ├─ Display (GOP):   Not found\n");
    }

    // ACPI
    if let Some(acpi) = &boot_info.acpi {
        hal::uart::print_str("      ├─ ACPI RSDP:       ");
        hal::uart::print_hex(acpi.rsdp_address);
        hal::uart::print_str(" (v");
        hal::uart::print_dec(acpi.version as u64);
        hal::uart::print_str(")\n");
    } else {
        hal::uart::print_str("      ├─ ACPI RSDP:       Not found\n");
    }

    // SMBIOS
    if let Some(sm) = &boot_info.smbios {
        hal::uart::print_str("      ├─ SMBIOS:          ");
        hal::uart::print_hex(sm.address);
        hal::uart::print_str(" (v");
        hal::uart::print_dec(sm.version as u64);
        hal::uart::print_str(")\n");
    } else {
        hal::uart::print_str("      ├─ SMBIOS:          Not found\n");
    }

    // Secure Boot
    hal::uart::print_str("      ├─ Secure Boot:     ");
    hal::uart::print_str(match boot_info.secure_boot {
        core_kernel::boot_info::SecureBootState::Enabled   => "Enabled",
        core_kernel::boot_info::SecureBootState::Disabled  => "Disabled",
        core_kernel::boot_info::SecureBootState::SetupMode => "Setup Mode",
        core_kernel::boot_info::SecureBootState::Unknown   => "Unknown",
    });
    hal::uart::print_str("\n");

    // Boot time
    if let Some(t) = &boot_info.boot_time {
        hal::uart::print_str("      ├─ Boot time:       ");
        hal::uart::print_dec(t.year as u64);
        hal::uart::print_str("-");
        print_padded_u8(t.month);
        hal::uart::print_str("-");
        print_padded_u8(t.day);
        hal::uart::print_str(" ");
        print_padded_u8(t.hour);
        hal::uart::print_str(":");
        print_padded_u8(t.minute);
        hal::uart::print_str(":");
        print_padded_u8(t.second);
        hal::uart::print_str("\n");
    }

    // Load options
    if !boot_info.load_options.is_empty() {
        hal::uart::print_str("      ├─ Load options:    ");
        for &b in boot_info.load_options.as_bytes() { hal::uart::put_byte(b); }
        hal::uart::print_str("\n");
    }

    hal::uart::print_str("      ├─ ExitBootServices: Exclusive hardware ownership acquired\n");
    hal::uart::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 1: Memory Management
    // -------------------------------------------------------------------------
    hal::uart::print_str("[1/7] Memory Management\n");

    // Use the largest free physical region reported by UEFI instead of a
    // hardcoded address — matches what real operating systems do.
    let (phys_start, phys_size) = boot_info
        .memory_map
        .largest_usable_region()
        .map(|r| (r.base as usize, r.size as usize))
        .unwrap_or((0x100000, 0x10000000)); // safe fallback

    let mut allocator = core_kernel::memory::PhysicalAllocator::new(phys_start, phys_size);
    let kernel_heap = allocator.allocate(0x100000).expect("Failed to allocate kernel heap");

    hal::uart::print_str("      └─ Phys base:       ");
    hal::uart::print_hex(phys_start as u64);
    hal::uart::print_str("\n");
    hal::uart::print_str("      └─ Kernel heap:     ");
    hal::uart::print_hex(kernel_heap as u64);
    hal::uart::print_str(" (1 MB)\n");

    hal::uart::print_str("      [DBG] pml4=");
    let kernel_pml4 = unsafe { &mut *core::ptr::addr_of_mut!(KERNEL_PML4) };
    hal::uart::print_hex(core::ptr::addr_of!(*kernel_pml4) as u64);
    hal::uart::print_str(" phys_size=");
    hal::uart::print_hex(phys_size as u64);
    hal::uart::print_str("\n");

    // Build the kernel PML4: identity-map every physical region from the UEFI
    // memory map using 2 MB huge pages.  This covers the kernel image, ACPI
    // tables, MMIO windows, and all free RAM in one pass.
    let kernel_pml4 = unsafe { &mut *core::ptr::addr_of_mut!(KERNEL_PML4) };
    for region in boot_info.memory_map.regions() {
        core_kernel::memory::identity_map_region(
            kernel_pml4,
            region.base,
            region.size,
            core_kernel::memory::PTE_PRESENT | core_kernel::memory::PTE_WRITABLE,
            &mut allocator,
        );
    }
    let kernel_pml4_phys = core::ptr::addr_of!(*kernel_pml4) as u64;

    // Publish the kernel PML4 address so SYS_SPAWN / SYS_MAP can find it.
    core_kernel::scheduler::set_kernel_pml4(kernel_pml4_phys);

    // Initialise the global physical bump allocator starting AFTER whatever
    // the local allocator consumed for kernel page tables.  Starting at a
    // fixed offset would alias the kernel PT frames (which start immediately
    // after the 1 MB heap) and corrupt them on the first global_alloc_4k().
    let global_base = allocator.current_base();
    let global_size = (phys_start + phys_size).saturating_sub(global_base + 0x100000);
    hal::uart::print_str("      [DBG] global_alloc base=");
    hal::uart::print_hex(global_base as u64);
    hal::uart::print_str(" size=");
    hal::uart::print_hex(global_size as u64);
    hal::uart::print_str("\n");
    core_kernel::memory::init_global_allocator(global_base, global_size);
    hal::uart::print_str("      [DBG] init_global_allocator done\n");

    // Pre-fill the page-table frame pool.  Drawing PT_POOL_CAP frames upfront
    // ensures that map_page_global / split_huge_page_global always get O(1)
    // allocations rather than the O(N/64) bitmap scan.
    // IEC 61508 §7.4.5: static pool size = proven upper bound on PT usage.
    let pool_filled = core_kernel::memory::pool_init(core_kernel::memory::PT_POOL_CAP);
    hal::uart::print_str("      [DBG] pt_pool: filled=");
    hal::uart::print_dec(pool_filled as u64);
    hal::uart::print_str("/");
    hal::uart::print_dec(core_kernel::memory::PT_POOL_CAP as u64);
    hal::uart::print_str("\n");

    // Disable interrupts before we switch page tables.  UEFI's timer ISR could
    // fire after activate_page_table() while the UEFI IDT is still in place;
    // those handlers reference UEFI-mapped virtual addresses that our kernel
    // PML4 may not cover.  Interrupts are re-enabled in efi_main's tail after
    // our IDT is loaded and the scheduler is running.
    arch_x86_64::cpu::disable_interrupts();

    // Enable EFER.NXE, CR0.WP, and CR4.SMEP.
    // CR4.SMAP is enabled AFTER we switch to our own CR3 (see below) because
    // UEFI page tables often set PTE_USER which would cause an immediate fault.
    arch_x86_64::cpu::init_protection();
    hal::uart::print_str("      [DBG] init_protection done\n");

    // Install kernel stack guard pages BEFORE activating our PML4.
    // The CR3 load flushes the entire TLB, so no per-page invlpg is needed.
    install_kernel_stack_guard_pages(kernel_pml4);

    // Apply per-section PTE flags to the kernel image BEFORE activating the
    // PML4.  The subsequent CR3 load will flush the TLB, so no invlpg is needed.
    //
    // Goals (IEC 61508 §7.4.3 — memory protection / spatial isolation):
    //   .text   → PRESENT only            (read + execute; not writable, not NX)
    //   .rdata  → PRESENT + NX            (read-only data; not executable)
    //   .data   → PRESENT + WRITABLE + NX (writable data; not executable)
    //   .bss    → PRESENT + WRITABLE + NX (same as .data)
    //
    // The 2 MB huge pages laid down by identity_map_region must be split to
    // 4 KB granularity before per-page flags can be applied.
    remap_kernel_sections(kernel_pml4, boot_info.kernel_image_base);

    {
        let s = core_kernel::memory::frame_stats();
        hal::uart::print_str("      [DBG] frame tracker: free=");
        hal::uart::print_dec(s[0] as u64);
        hal::uart::print_str(" kdata=");
        hal::uart::print_dec(s[1] as u64);
        hal::uart::print_str(" user=");
        hal::uart::print_dec(s[2] as u64);
        hal::uart::print_str(" guard=");
        hal::uart::print_dec(s[3] as u64);
        hal::uart::print_str(" mmio=");
        hal::uart::print_dec(s[4] as u64);
        hal::uart::print_str("\n");
    }

    // Load the kernel PML4 — from this point the CPU enforces our page table.
    // Identity mapping keeps phys == virt so execution continues uninterrupted.
    unsafe { arch_x86_64::cpu::activate_page_table(kernel_pml4_phys); }
    hal::uart::print_str("      [DBG] activate_page_table done\n");

    // Check for crash records left by the previous boot BEFORE init() stamps
    // the region — drain() sees whatever warm-reset preserved.
    let crash_count = core_kernel::crash_log::drain(|i, total, rec| {
        if i == 0 {
            hal::uart::print_str("\n╔══════════════════════════════════════╗\n");
            hal::uart::print_str("║  PERSISTENT CRASH LOG (prev. boot)  ║\n");
            hal::uart::print_str("╚══════════════════════════════════════╝\n");
        }
        hal::uart::print_str("\n  Record ");
        hal::uart::print_dec(i as u64 + 1);
        hal::uart::print_str(" of ");
        hal::uart::print_dec(total as u64);
        hal::uart::print_str("\n");
        hal::uart::print_str("  vector=0x"); hal::uart::print_hex(rec.vector);
        hal::uart::print_str("  tick=");     hal::uart::print_dec(rec.tick);
        hal::uart::print_str("  pid=");      hal::uart::print_dec(rec.pid);
        hal::uart::print_str("\n");
        hal::uart::print_str("  rip=");      hal::uart::print_hex(rec.rip);
        hal::uart::print_str("  rfl=");      hal::uart::print_hex(rec.rflags);
        hal::uart::print_str("\n");
        if rec.cr2 != 0 {
            hal::uart::print_str("  cr2=");  hal::uart::print_hex(rec.cr2);
            hal::uart::print_str("  (#PF fault address)\n");
        }
    });
    if crash_count > 0 {
        hal::uart::print_str("\n  [crash log cleared]\n\n");
    }
    // Stamp the region header so subsequent write() calls can store new records.
    core_kernel::crash_log::init();

    // NOW safe to enable SMAP: our PML4 has no PTE_USER pages.
    arch_x86_64::cpu::init_smap();
    hal::uart::print_str("      [DBG] init_smap done\n");

    hal::uart::print_str("      └─ Page tables:     4-level PML4 (2 MB huge pages, all regions)\n");
    hal::uart::print_str("      └─ CR3 loaded:      ");
    hal::uart::print_hex(kernel_pml4_phys);
    hal::uart::print_str("\n");
    hal::uart::print_str("      └─ Protection:      NXE + WP + SMEP + SMAP enabled\n");
    hal::uart::print_str("      └─ Guard pages:     4 KB unmapped below each kernel stack\n");
    hal::uart::print_str("      └─ .text:           read-only + executable (PTE_WRITABLE cleared)\n");
    hal::uart::print_str("      └─ .rdata:          read-only + NX\n");
    hal::uart::print_str("      └─ .data/.bss:      read-write + NX (PTE_NO_EXECUTE set)\n");
    hal::uart::print_str("      └─ Status:          ✓ OK\n\n");

    // Parse ACPI MADT — discover Local APIC + I/O APIC addresses.
    // The MADT is required for Local APIC init (§4) and I/O APIC routing.
    // On QEMU with default settings, QEMU emulates ACPI 2.0 with an XSDT.
    {
        let rsdp_phys = boot_info.acpi.as_ref().map(|a| a.rsdp_address).unwrap_or(0);
        if let Some(madt_phys) = core_kernel::acpi::find_madt(rsdp_phys) {
            if let Some(madt) = core_kernel::acpi::parse_madt(madt_phys) {
                hal::uart::print_str("      ├─ MADT found:      lapic_addr=");
                hal::uart::print_hex(madt.local_apic_addr as u64);
                hal::uart::print_str("  lapics=");
                hal::uart::print_dec(madt.local_apic_count as u64);
                hal::uart::print_str("  ioapics=");
                hal::uart::print_dec(madt.io_apic_count as u64);
                hal::uart::print_str("\n");
                if let Some(ioa) = madt.primary_io_apic() {
                    hal::uart::print_str("      │   I/O APIC[0]:    addr=");
                    hal::uart::print_hex(ioa.address as u64);
                    hal::uart::print_str("  gsi_base=");
                    hal::uart::print_dec(ioa.gsi_base as u64);
                    hal::uart::print_str("\n");
                }
                // IRQ 0 (PIT timer) override is common on APIC systems.
                let pit_gsi = madt.irq_to_gsi(0);
                if pit_gsi != 0 {
                    hal::uart::print_str("      │   IRQ 0 → GSI ");
                    hal::uart::print_dec(pit_gsi as u64);
                    hal::uart::print_str(" (PIT timer override)\n");
                }
                // Store LAPIC and I/O APIC addresses for Stage 4 init.
                unsafe {
                    LAPIC_PHYS_ADDR = madt.local_apic_addr as u64;
                    if let Some(ioa) = madt.primary_io_apic() {
                        IOAPIC_PHYS_ADDR = ioa.address as u64;
                        IOAPIC_GSI_BASE  = ioa.gsi_base;
                    }
                }
            } else {
                hal::uart::print_str("      ├─ MADT:            parse failed (bad checksum?)\n");
            }
        } else {
            hal::uart::print_str("      ├─ MADT:            not found (RSDP=");
            hal::uart::print_hex(boot_info.acpi.as_ref().map(|a| a.rsdp_address).unwrap_or(0));
            hal::uart::print_str(")\n");
        }
    }

    // Parse ACPI FADT — discover PM timer I/O port and ACPI reset register.
    // The PM timer is used by §10 (TSC calibration); the reset register provides
    // a software-initiated safe-state transition path (IEC 61508 §7.4.9).
    {
        let rsdp_phys = boot_info.acpi.as_ref().map(|a| a.rsdp_address).unwrap_or(0);
        if let Some(fadt_phys) = core_kernel::acpi::find_fadt(rsdp_phys) {
            if let Some(fadt) = core_kernel::acpi::parse_fadt(fadt_phys) {
                hal::uart::print_str("      ├─ FADT found:      ");
                if fadt.pm_timer_valid() {
                    hal::uart::print_str("pm_timer=0x");
                    hal::uart::print_hex(fadt.pm_timer_port as u64);
                } else {
                    hal::uart::print_str("pm_timer=absent");
                }
                if fadt.reset_supported() {
                    hal::uart::print_str("  reset_reg=0x");
                    hal::uart::print_hex(fadt.reset_reg_addr);
                    hal::uart::print_str(" val=0x");
                    hal::uart::print_hex(fadt.reset_value as u64);
                } else {
                    hal::uart::print_str("  reset_reg=absent");
                }
                hal::uart::print_str("\n");
                unsafe { PM_TIMER_PORT = fadt.pm_timer_port; }
            } else {
                hal::uart::print_str("      ├─ FADT:            parse failed (bad checksum?)\n");
            }
        } else {
            hal::uart::print_str("      ├─ FADT:            not found\n");
        }
    }

    // Parse ACPI DMAR — discover Intel VT-d IOMMU controllers.
    // The DMAR is required for IOMMU init (§4), which confines DMA-capable
    // peripherals to their own memory regions (IEC 61508 §7.4.3).
    {
        let rsdp_phys = boot_info.acpi.as_ref().map(|a| a.rsdp_address).unwrap_or(0);
        if let Some(dmar_phys) = core_kernel::acpi::find_dmar(rsdp_phys) {
            if let Some(dmar) = core_kernel::acpi::parse_dmar(dmar_phys) {
                hal::uart::print_str("      ├─ DMAR found:      iommu_units=");
                hal::uart::print_dec(dmar.unit_count as u64);
                hal::uart::print_str("  haw=");
                hal::uart::print_dec(dmar.haw() as u64);
                hal::uart::print_str("-bit");
                if dmar.intr_remap_supported() {
                    hal::uart::print_str("  intr_remap=yes");
                }
                hal::uart::print_str("\n");
                // Print each unit's MMIO base.
                for unit in dmar.iter_units() {
                    hal::uart::print_str("      │    iommu base=0x");
                    hal::uart::print_hex(unit.register_base);
                    if unit.covers_all_pci() {
                        hal::uart::print_str(" (covers all PCI)");
                    }
                    hal::uart::print_str("\n");
                }
                // Store IOMMU addresses for future IOMMU init.
                unsafe {
                    IOMMU_COUNT = dmar.unit_count;
                    for (i, unit) in dmar.iter_units().enumerate() {
                        IOMMU_BASES[i] = unit.register_base;
                    }
                }
            } else {
                hal::uart::print_str("      ├─ DMAR:            parse failed (bad checksum?)\n");
            }
        } else {
            hal::uart::print_str("      ├─ DMAR:            not found (no VT-d hardware)\n");
        }
    }

    // -------------------------------------------------------------------------
    // STAGE 2: CPU Setup (GDT, TSS, IDT, SYSCALL)
    // -------------------------------------------------------------------------
    hal::uart::print_str("[2/7] CPU Setup (GDT/TSS/IDT)\n");

    // CPU feature check — abort if required hardware features are absent.
    check_cpu_features(boot_info);

    unsafe {
        // Initialise TSS IST stacks and install TSS descriptor into GDT.
        let tss_ptr = arch_x86_64::cpu::init_tss();
        let gdt = core::ptr::addr_of_mut!(GDT);
        (*gdt).install_tss(tss_ptr);
        (*gdt).load();
        // Load the TSS selector (0x28) into the Task Register.
        arch_x86_64::cpu::load_tss();

        let idt = core::ptr::addr_of_mut!(IDT);
        arch_x86_64::interrupts::init(&mut *idt);
        (*idt).load();
    }

    hal::uart::print_str("      └─ GDT loaded:      7 selectors (null, ring0/3 code/data, TSS)\n");
    hal::uart::print_str("      └─ TSS loaded:      RSP0/IST1/IST2/IST3 configured\n");
    hal::uart::print_str("      └─ IDT loaded:      256 gates (all vectors handled)\n");
    arch_x86_64::cpu::syscall::init();
    hal::uart::print_str("      └─ SYSCALL/SYSRET:  MSRs configured (EFER.SCE, STAR, LSTAR, SFMASK)\n");
    hal::uart::print_str("      └─ Single-core:     Enforced (SIL-4 §7.4.10 temporal partition)\n");
    hal::uart::print_str("      └─ Secure Boot:     Validated (halt if Disabled in safety-mode)\n");
    hal::uart::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 3: Interrupt Handlers
    // -------------------------------------------------------------------------
    hal::uart::print_str("[3/7] Interrupt Handlers\n");
    hal::uart::print_str("      └─ Exception  0:    #DE Division by zero (user/kernel detect)\n");
    hal::uart::print_str("      └─ Exception  2:    #NMI (IST2 dedicated stack)\n");
    hal::uart::print_str("      └─ Exception  8:    #DF Double fault (IST1 dedicated stack)\n");
    hal::uart::print_str("      └─ Exception 13:    #GP General protection fault\n");
    hal::uart::print_str("      └─ Exception 14:    #PF Page fault + CR2 dump\n");
    hal::uart::print_str("      └─ Exception 18:    #MC Machine check (IST3 dedicated stack)\n");
    hal::uart::print_str("      └─ Interrupt 32:    IRQ0 PIT timer 100 Hz\n");
    hal::uart::print_str("      └─ Vectors 1–254:   Catch-all (EOI + log, no triple fault)\n");
    hal::uart::print_str("      └─ Vector 255:      Spurious (LAPIC, iretq only)\n");
    hal::uart::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 4: System Timer + APIC + IOMMU + Hardware Watchdog
    // -------------------------------------------------------------------------
    hal::uart::print_str("[4/7] System Timer\n");
    arch_x86_64::timer::init();
    hal::uart::print_str("      └─ PIT frequency:   100 Hz (10 ms ticks)\n");
    hal::uart::print_str("      └─ PIC configured:  Master & Slave\n");

    // Local APIC: software-enable, calibrate 100 Hz periodic timer, mask 8259.
    // After this call, vector 32 is delivered by the LAPIC instead of the PIC.
    // LAPIC_EOI_ADDR is set inside lapic::init() so the timer ISR can ACK LAPIC ticks.
    {
        let lapic_base = unsafe { LAPIC_PHYS_ADDR };
        // The LAPIC MMIO window (4 KB at lapic_base) may not appear in the UEFI
        // memory map, so the Stage-1 identity-mapping loop may have skipped it.
        // Explicitly map the page before the first register access; mapping an
        // already-present entry is a no-op (map_page_global checks PTE_PRESENT).
        if lapic_base != 0 {
            let pml4 = unsafe { &mut *core::ptr::addr_of_mut!(KERNEL_PML4) };
            core_kernel::memory::map_page_global(
                pml4,
                lapic_base,
                lapic_base,
                core_kernel::memory::PTE_PRESENT | core_kernel::memory::PTE_WRITABLE,
            );
        }
        arch_x86_64::apic::lapic::init(lapic_base);
    }

    // I/O APIC: mask all redirect entries (safe initial state).
    // Individual IRQs can be routed later via apic::ioapic::route_irq().
    {
        let ioapic_base = unsafe { IOAPIC_PHYS_ADDR };
        // Same reasoning as LAPIC: map the I/O APIC MMIO page on demand.
        if ioapic_base != 0 {
            let pml4 = unsafe { &mut *core::ptr::addr_of_mut!(KERNEL_PML4) };
            core_kernel::memory::map_page_global(
                pml4,
                ioapic_base,
                ioapic_base,
                core_kernel::memory::PTE_PRESENT | core_kernel::memory::PTE_WRITABLE,
            );
        }
        arch_x86_64::apic::ioapic::init(ioapic_base);

        // Route COM1 UART (ISA IRQ 4) to IDT vector 36 on BSP (LAPIC ID 0).
        // ISA IRQ 4 sits on IOAPIC pin (4 - gsi_base); gsi_base is 0 on q35.
        if ioapic_base != 0 {
            let gsi_base = unsafe { IOAPIC_GSI_BASE };
            let pin      = 4u32.saturating_sub(gsi_base) as u8;
            unsafe { arch_x86_64::apic::ioapic::route_irq(ioapic_base, pin, 36, 0); }
            hal::uart::print_str("      ├─ IOAPIC:          IRQ4(COM1) → vector 36\n");
        }
    }

    // Intel VT-d IOMMU: enable passthrough DMA remapping (IEC 61508 §7.4.3).
    // Installs root/context tables so DMA is routed through kernel-controlled
    // structures; per-device restriction will be added in a future patch.
    {
        // Map all IOMMU MMIO register pages before the driver touches them.
        {
            let pml4  = unsafe { &mut *core::ptr::addr_of_mut!(KERNEL_PML4) };
            let count = unsafe { IOMMU_COUNT };
            for i in 0..count {
                let base = unsafe { IOMMU_BASES[i] };
                if base != 0 {
                    core_kernel::memory::map_page_global(
                        pml4,
                        base,
                        base,
                        core_kernel::memory::PTE_PRESENT | core_kernel::memory::PTE_WRITABLE,
                    );
                }
            }
        }
        let iommu_bases = unsafe { &IOMMU_BASES[..IOMMU_COUNT] };
        let enabled = core_kernel::iommu::init(iommu_bases);
        if enabled > 0 {
            hal::uart::print_str("      ├─ IOMMU:           ");
            hal::uart::print_dec(enabled as u64);
            hal::uart::print_str(" unit(s) enabled (passthrough mode)\n");
        } else {
            hal::uart::print_str("      ├─ IOMMU:           not available (VT-d absent or no PT support)\n");
        }
    }

    // HPET: high-resolution event timer, monotonic clock source for TSC calibration.
    // Parsed from ACPI and enabled here; TSC is then calibrated against the HPET.
    {
        let rsdp_phys = boot_info.acpi.as_ref().map(|a| a.rsdp_address).unwrap_or(0);
        if let Some(hpet_phys) = core_kernel::acpi::find_hpet(rsdp_phys) {
            // Map the HPET MMIO page BEFORE parse_hpet() reads the live GCI
            // register (parse_hpet reads MMIO to extract the counter period).
            if let Some(hpet_mmio) = core_kernel::acpi::hpet_mmio_base(hpet_phys) {
                let pml4 = unsafe { &mut *core::ptr::addr_of_mut!(KERNEL_PML4) };
                core_kernel::memory::map_page_global(
                    pml4,
                    hpet_mmio,
                    hpet_mmio,
                    core_kernel::memory::PTE_PRESENT | core_kernel::memory::PTE_WRITABLE,
                );
            }
            if let Some(hpet) = core_kernel::acpi::parse_hpet(hpet_phys) {
                core_kernel::hpet::init(&hpet);
                hal::uart::print_str("      ├─ HPET:            enabled, period=");
                hal::uart::print_dec(hpet.period_fs);
                hal::uart::print_str(" fs, timers=");
                hal::uart::print_dec(hpet.timer_count as u64);
                hal::uart::print_str(if hpet.counter_64bit { ", 64-bit\n" } else { ", 32-bit\n" });

                // Calibrate TSC against the HPET main counter (~10 ms busy-wait).
                arch_x86_64::timer::tsc::calibrate(hpet.base_address, hpet.period_fs);
                let khz = arch_x86_64::timer::tsc::khz();
                hal::uart::print_str("      ├─ TSC calibrated:  ");
                hal::uart::print_dec(khz);
                hal::uart::print_str(" kHz\n");
            } else {
                hal::uart::print_str("      ├─ HPET:            parse failed (bad checksum or I/O-space GAS)\n");
            }
        } else {
            hal::uart::print_str("      ├─ HPET:            not found in ACPI tables\n");
        }
    }

    // Arm the iB700 hardware watchdog with a 10-second timeout.
    // The idle process will kick it every 50 ticks (500 ms) — well inside the window.
    // If the scheduler stops, the watchdog fires and the system resets to safe state.
    // IEC 61508 §7.4.9: mandatory for SIL-3/4.
    // QEMU: add `-device ib700,id=watchdog0 -watchdog-action reset` to scripts/run.sh.
    hal::watchdog::init(10);
    hal::uart::print_str("      └─ Watchdog:        iB700 armed (10 s timeout, kick every 500 ms)\n");
    hal::uart::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 5: Process Management
    // -------------------------------------------------------------------------
    hal::uart::print_str("[5/7] Process Management\n");
    hal::uart::print_str("      └─ Max processes:   32\n");
    hal::uart::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 6: Scheduler + Server Launch
    // -------------------------------------------------------------------------
    hal::uart::print_str("[6/7] Scheduler\n");

    let sched = core_kernel::scheduler::Scheduler::new();

    // Move the scheduler into the global slot FIRST (empty — no processes yet),
    // so all spawn_elf() calls below can use get_global() internally.
    core_kernel::scheduler::init_global(sched);

    // Register the ELF spawn hook so that SYS_SPAWN_ELF (26) from ring-3
    // can call into the kernel ELF loader without creating a circular
    // dependency between arch-x86_64 and the kernel crate.
    //
    // SAFETY: elf_spawn_kernel_hook / restart_server_hook are well-typed
    // function pointers; STAC is active in the syscall handler.
    core_kernel::elf_spawn::set_elf_spawn_fn(elf_spawn_kernel_hook);
    core_kernel::elf_spawn::set_restart_server_fn(restart_server_hook);

    // ── Launch ring-3 servers from embedded ELF images ────────────────────────
    //
    // Spawn order determines PID assignment.  init MUST be first so the kernel
    // fault handler's hardcoded `ProcessId::new(1)` target resolves to init.
    //
    // Spawn order: init(PID1) → idle(PID2) → uart-drv(PID3) → vfs(PID4) → shell(PID5).

    hal::uart::print_str("      └─ Spawning init (PID 1)...\n");
    let init_pid = elf::spawn_elf(INIT_ELF, 32); // priority 32: health monitor
    if init_pid.is_none() {
        hal::uart::print_str("      [WARN] init ELF load failed\n");
    }

    // Idle process — priority 255, runs only when no other process is Ready.
    // Added after init so idle does NOT steal PID 1.
    let idle_pid = if let Some(sched) = core_kernel::scheduler::get_global() {
        let idle = sched.add_process(idle_process as *const () as u64, 0, kernel_pml4_phys);
        if let Some(idle) = idle {
            sched.set_priority(idle, 255);
        }
        idle
    } else {
        None
    };

    hal::uart::print_str("      └─ Spawning uart-drv...\n");
    let uart_pid = elf::spawn_elf(UART_DRV_ELF, 64);
    if uart_pid.is_none() {
        hal::uart::print_str("      [WARN] uart-drv ELF load failed\n");
    }

    hal::uart::print_str("      └─ Spawning vfs...\n");
    let vfs_pid = elf::spawn_elf(VFS_ELF, 64);
    if vfs_pid.is_none() {
        hal::uart::print_str("      [WARN] vfs ELF load failed\n");
    }

    hal::uart::print_str("      └─ Spawning rost-shell...\n");
    let shell_pid = elf::spawn_elf(SHELL_ELF, 64);
    if shell_pid.is_none() {
        hal::uart::print_str("      [WARN] shell ELF load failed\n");
    }

    hal::uart::print_str("      └─ Spawning rost-net...\n");
    let net_pid = elf::spawn_elf(NET_ELF, 48); // priority 48: between init(32) and uart-drv(64)
    if net_pid.is_none() {
        hal::uart::print_str("      [WARN] rost-net ELF load failed\n");
    }

    // Set TSS.RSP0, CURRENT_PID, and the scheduler's internal current_process
    // to the idle process so that the first timer tick advances idle's cpu_time
    // and preempts it in favour of init / uart-drv / shell.
    //
    // CRITICAL: both the global atomic CURRENT_PID *and* the scheduler's own
    // current_process RefCell must point to idle.  timer_tick() reads only the
    // scheduler's RefCell — if it is None, preempt is never set to true and
    // no context switch ever fires (all processes starve forever).
    if let Some(idle) = idle_pid {
        core_kernel::scheduler::CURRENT_PID
            .store(idle.as_u32(), core::sync::atomic::Ordering::Relaxed);
        if let Some(sched) = core_kernel::scheduler::get_global() {
            sched.set_current(idle);
        }
    }

    hal::uart::print_str("      └─ Algorithm:       Priority (lowest num = highest prio)\n");
    hal::uart::print_str("      └─ Time quantum:    10 ms\n");
    hal::uart::print_str("      └─ init (PID 1):   PID ");
    if let Some(p) = init_pid {
        hal::uart::print_hex(p.as_u32() as u64);
        hal::uart::print_str(" (priority 32 — health monitor)\n");
    }
    hal::uart::print_str("      └─ Idle process:    PID ");
    if let Some(p) = idle_pid {
        hal::uart::print_hex(p.as_u32() as u64);
        hal::uart::print_str(" (priority 255)\n");
    }
    hal::uart::print_str("      └─ uart-drv:        PID ");
    if let Some(p) = uart_pid { hal::uart::print_hex(p.as_u32() as u64); }
    hal::uart::print_str("\n");
    hal::uart::print_str("      └─ vfs:             PID ");
    if let Some(p) = vfs_pid { hal::uart::print_hex(p.as_u32() as u64); }
    hal::uart::print_str("\n");
    hal::uart::print_str("      └─ rost-shell:      PID ");
    if let Some(p) = shell_pid { hal::uart::print_hex(p.as_u32() as u64); }
    hal::uart::print_str("\n");
    hal::uart::print_str("      └─ TSS.RSP0:        Updated per context switch\n");
    hal::uart::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 7: IPC
    // -------------------------------------------------------------------------
    hal::uart::print_str("[7/7] Inter-Process Communication\n");

    hal::uart::print_str("      └─ Queue capacity:  16 messages + notification word\n");
    hal::uart::print_str("      └─ Msg fields:      8 × u64 (64 bytes payload)\n");
    hal::uart::print_str("      └─ Sender stamp:    Kernel overwrites sender PID\n");
    hal::uart::print_str("      └─ Rate limiting:   Per-process ipc_rate_limit supported\n");
    hal::uart::print_str("      └─ IPC timeout:     blocked_deadline per blocked process\n");
    hal::uart::print_str("      └─ Notifications:   Lightweight word-OR signalling\n");
    hal::uart::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // Kernel Ready
    // -------------------------------------------------------------------------
    hal::uart::print_str("╔════════════════════════════════════╗\n");
    hal::uart::print_str("║        KERNEL INITIALIZATION      ║\n");
    hal::uart::print_str("║             COMPLETE              ║\n");
    hal::uart::print_str("╚════════════════════════════════════╝\n\n");

    // Verify init's kernel stack is mapped in the kernel PML4 before enabling interrupts.
    if let Some(sched) = core_kernel::scheduler::get_global() {
        let init_pid = core_kernel::process::ProcessId::new(1);
        if let Some((ctx_rsp, kern_rsp)) = sched.get_ctx_rsp(init_pid) {
            let page = ctx_rsp & !0xFFF;
            let phys = core_kernel::memory::translate_address(kernel_pml4, page);
            hal::uart::print_str("[DBG] init ctx.rsp=");
            hal::uart::print_hex(ctx_rsp);
            hal::uart::print_str(" kern_rsp=");
            hal::uart::print_hex(kern_rsp);
            hal::uart::print_str(" stack_page=");
            hal::uart::print_hex(page);
            hal::uart::print_str(" mapped=");
            match phys {
                Some(p) => hal::uart::print_hex(p),
                None    => hal::uart::print_str("UNMAPPED!"),
            }
            hal::uart::print_str("\n");
        }
    }

    arch_x86_64::cpu::enable_interrupts();
    hal::uart::print_str("✓ Interrupts enabled — ring-3 servers running\n\n");

    // The kernel's job is done.  Enter the idle loop; the timer ISR will
    // preempt us and schedule uart-drv / vfs / shell as appropriate.
    idle_process()
}

// =============================================================================
// HELPERS
// =============================================================================

/// Print `bytes` as mebibytes (rounded down).
fn print_mib(bytes: u64) {
    hal::uart::print_dec(bytes / (1024 * 1024));
}

// =============================================================================
// KERNEL STACK GUARD PAGES
// =============================================================================

/// Unmap the 4 KB guard slot below every kernel stack.
///
/// Each slot in `KERNEL_STACKS` is 12 KB: 4 KB guard + 8 KB stack.  The guard
/// page is at offset 0 within the slot and is never legitimately touched.
/// After this function runs, any kernel-stack overflow will fault into that
/// unmapped page and produce a #PF, making the overflow detectable instead of
/// silently corrupting the adjacent slot.
///
/// ## Why before `activate_page_table`
/// Modifications are applied to `kernel_pml4` while UEFI's identity mapping
/// is still active.  The CR3 load that follows flushes the entire TLB, so no
/// `invlpg` instruction is needed here.  If guard pages need to be modified
/// after CR3 is loaded, callers must call `arch_x86_64::cpu::invlpg()`.
///
/// ## Algorithm
/// 1. For each stack slot, read the guard-page physical address.
/// 2. `split_huge_page_global(pml4, guard_addr)` — if the 2 MB PD entry
///    covering the guard page is a huge-page leaf, replace it with 512 × 4 KB
///    PT entries mapping the same physical range with identical flags.  This
///    allows the next step to operate on individual 4 KB slots.
/// 3. `unmap_page(pml4, guard_addr)` — clear the PT entry for the guard page,
///    marking it not-present.
fn install_kernel_stack_guard_pages(pml4: &mut core_kernel::memory::PageTable) {
    use core_kernel::process::pcb::{kernel_stack_guard_addr, MAX_KERNEL_STACKS};
    use core_kernel::memory::{split_huge_page_global, unmap_page, frame_tag, FrameKind};

    let mut installed = 0u32;
    for id in 0..MAX_KERNEL_STACKS {
        let guard_addr = match kernel_stack_guard_addr(id) {
            Some(a) => a,
            None => break,
        };

        // Step 1: demote the 2 MB huge page to 4 KB entries (no-op if already split).
        if !split_huge_page_global(pml4, guard_addr) {
            // Allocation failure — log and continue; remaining stacks still get guards.
            hal::uart::print_str("      [WARN] guard split failed for stack ");
            hal::uart::print_dec(id as u64);
            hal::uart::print_str("\n");
            continue;
        }

        // Step 2: mark the guard 4 KB slot not-present.
        if unmap_page(pml4, guard_addr) {
            frame_tag(guard_addr, FrameKind::Guard);
            installed += 1;
        }
    }

    hal::uart::print_str("      [DBG] guard pages: ");
    hal::uart::print_dec(installed as u64);
    hal::uart::print_str("/");
    hal::uart::print_dec(MAX_KERNEL_STACKS as u64);
    hal::uart::print_str(" installed\n");
}

/// Print a u8 zero-padded to 2 digits (for timestamps).
fn print_padded_u8(n: u8) {
    if n < 10 { hal::uart::put_byte(b'0'); }
    hal::uart::print_dec(n as u64);
}

// =============================================================================
// CPU FEATURE CHECK
// =============================================================================

/// Verify that all hardware features and firmware security requirements are met.
/// Halts with a diagnostic message if any mandatory requirement is absent.
fn check_cpu_features(boot_info: &core_kernel::boot_info::BootInfo) {
    let f = &boot_info.cpu;
    let mut ok = true;

    macro_rules! require {
        ($cond:expr, $msg:literal) => {
            if !$cond {
                hal::uart::print_str("  [MISSING] ");
                hal::uart::print_str($msg);
                hal::uart::print_str("\n");
                ok = false;
            }
        };
    }

    require!(f.features.has_smep(), "SMEP — required for CR4.SMEP");
    require!(f.features.has_msr(),  "MSR — required for RDMSR/WRMSR (EFER, STAR, LSTAR)");

    // Single-core enforcement.
    //
    // The kernel uses Relaxed atomics and no spinlocks — unsafe on more than one
    // logical CPU.  Hyper-Threading (HTT bit) exposes multiple logical cores on
    // the same die; max_logical_cpus > 1 with HTT active means two logical CPUs
    // can see shared kernel state simultaneously, causing data races on every
    // kernel data structure.
    //
    // IEC 61508 SIL-4 §7.4.10 requires temporal partitioning between concurrent
    // execution contexts; without symmetric multi-processing support in the
    // scheduler and all shared data structures this is not achievable.
    //
    // Boot the system with SMP disabled (e.g. QEMU: -smp 1) or use a
    // single-core CPU.
    if f.features.has_htt() && f.max_logical_cpus > 1 {
        hal::uart::print_str("  [FATAL] Multi-core CPU detected (");
        hal::uart::print_dec(f.max_logical_cpus as u64);
        hal::uart::print_str(" logical CPUs).\n");
        hal::uart::print_str("          Rost requires a single logical CPU.\n");
        hal::uart::print_str("          Boot with SMP disabled (QEMU: -smp 1) or\n");
        hal::uart::print_str("          use a single-core CPU.\n");
        ok = false;
    }

    // Secure Boot validation.
    //
    // IEC 61508 requires that the safety software's integrity can be verified
    // before execution.  UEFI Secure Boot provides cryptographic chain-of-trust
    // from firmware → bootloader → kernel image.  Without it, an attacker with
    // physical access can replace the kernel image with a modified binary.
    //
    // In `safety-mode` builds (production) we halt if Secure Boot is not
    // Enabled.  In development builds (default) we emit a warning so that
    // QEMU and lab testing can continue without a Secure Boot configuration.
    let sb_ok = matches!(
        boot_info.secure_boot,
        core_kernel::boot_info::SecureBootState::Enabled
    );
    if !sb_ok {
        let label = match boot_info.secure_boot {
            core_kernel::boot_info::SecureBootState::Disabled  => "Disabled",
            core_kernel::boot_info::SecureBootState::SetupMode => "SetupMode",
            core_kernel::boot_info::SecureBootState::Unknown   => "Unknown",
            core_kernel::boot_info::SecureBootState::Enabled   => unreachable!(),
        };
        #[cfg(feature = "safety-mode")]
        {
            hal::uart::print_str("  [FATAL] Secure Boot is ");
            hal::uart::print_str(label);
            hal::uart::print_str(".\n");
            hal::uart::print_str("          Safety-mode kernels require Secure Boot = Enabled.\n");
            hal::uart::print_str("          Enable Secure Boot in firmware setup and re-enroll keys.\n");
            ok = false;
        }
        #[cfg(not(feature = "safety-mode"))]
        {
            hal::uart::print_str("  [WARN]  Secure Boot is ");
            hal::uart::print_str(label);
            hal::uart::print_str(" (non-safety build — continuing).\n");
            hal::uart::print_str("          Production builds require Secure Boot = Enabled\n");
            hal::uart::print_str("          (build with --features safety-mode to enforce).\n");
        }
    }

    if !ok {
        hal::uart::print_str("\nFATAL: Boot requirements not met. System halted.\n");
        loop { arch_x86_64::cpu::halt(); }
    }
}

// =============================================================================
// KERNEL SECTION PROTECTION
// =============================================================================

/// Parse the kernel PE/COFF section table and remap each section with the
/// appropriate PTE flags.
///
/// Must be called BEFORE `activate_page_table()` so that the subsequent CR3
/// load flushes the entire TLB (making per-page `invlpg` calls unnecessary).
///
/// The function is a no-op when `image_base == 0` (boot services unavailable).
fn remap_kernel_sections(pml4: &mut core_kernel::memory::PageTable, image_base: u64) {
    if image_base == 0 {
        hal::uart::print_str("      [WARN] kernel image base unknown — section protection skipped\n");
        return;
    }

    let sections = unsafe { pecoff::parse_kernel_sections(image_base) };

    // .text: execute + read; no write, no NX.
    if let Some(s) = sections.text {
        apply_section_flags(pml4, s.virt_addr, s.size,
            core_kernel::memory::PTE_PRESENT);
        hal::uart::print_str("      [DBG] .text    remapped RX  base=");
        hal::uart::print_hex(s.virt_addr);
        hal::uart::print_str(" size=");
        hal::uart::print_hex(s.size);
        hal::uart::print_str("\n");
    }

    // .rdata: read-only + NX.
    if let Some(s) = sections.rodata {
        apply_section_flags(pml4, s.virt_addr, s.size,
            core_kernel::memory::PTE_PRESENT | core_kernel::memory::PTE_NO_EXECUTE);
        hal::uart::print_str("      [DBG] .rdata   remapped R-NX base=");
        hal::uart::print_hex(s.virt_addr);
        hal::uart::print_str(" size=");
        hal::uart::print_hex(s.size);
        hal::uart::print_str("\n");
    }

    // .data and .bss: read + write + NX.
    let rw_nx = core_kernel::memory::PTE_PRESENT
        | core_kernel::memory::PTE_WRITABLE
        | core_kernel::memory::PTE_NO_EXECUTE;

    if let Some(s) = sections.data {
        apply_section_flags(pml4, s.virt_addr, s.size, rw_nx);
        hal::uart::print_str("      [DBG] .data    remapped RW-NX base=");
        hal::uart::print_hex(s.virt_addr);
        hal::uart::print_str(" size=");
        hal::uart::print_hex(s.size);
        hal::uart::print_str("\n");
    }

    if let Some(s) = sections.bss {
        apply_section_flags(pml4, s.virt_addr, s.size, rw_nx);
        hal::uart::print_str("      [DBG] .bss     remapped RW-NX base=");
        hal::uart::print_hex(s.virt_addr);
        hal::uart::print_str(" size=");
        hal::uart::print_hex(s.size);
        hal::uart::print_str("\n");
    }
}

/// Split all 2 MB huge pages covering `[base, base+size)` and then set
/// the PTE flags of every 4 KB page in that range to `flags`.
///
/// Pages outside the mapped range are silently skipped (remap_page_flags
/// returns false; no action taken).
fn apply_section_flags(
    pml4:  &mut core_kernel::memory::PageTable,
    base:  u64,
    size:  u64,
    flags: u64,
) {
    const PAGE_4K: u64 = 0x1000;
    const PAGE_2M: u64 = 0x200000;

    // Page-align the section range outward.
    let start = base & !(PAGE_4K - 1);
    let end   = (base.saturating_add(size) + PAGE_4K - 1) & !(PAGE_4K - 1);
    if end <= start { return; }

    // Split all 2 MB huge pages that overlap the section.
    let split_start = start & !(PAGE_2M - 1);
    let split_end   = (end + PAGE_2M - 1) & !(PAGE_2M - 1);
    let mut addr = split_start;
    while addr < split_end {
        // split_huge_page_global is a no-op if the entry is already a PT pointer.
        core_kernel::memory::split_huge_page_global(pml4, addr);
        addr = addr.wrapping_add(PAGE_2M);
    }

    // Remap each 4 KB page within the section with the requested flags.
    addr = start;
    while addr < end {
        core_kernel::memory::remap_page_flags(pml4, addr, flags);
        addr = addr.wrapping_add(PAGE_4K);
    }
}

// =============================================================================
// PANIC HANDLER
// =============================================================================

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    hal::uart::print_str("\n╔════════════════════════════════════╗\n");
    hal::uart::print_str("║          KERNEL PANIC              ║\n");
    hal::uart::print_str("╚════════════════════════════════════╝\n\n");

    if let Some(location) = info.location() {
        hal::uart::print_str("Location: ");
        hal::uart::print_str(location.file());
        hal::uart::print_str(":");
        hal::uart::print_hex(location.line() as u64);
        hal::uart::print_str("\n");
    }

    hal::uart::print_str("\nSystem halted.\n");

    loop { arch_x86_64::cpu::halt(); }
}
