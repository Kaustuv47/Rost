#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;
use uefi::prelude::*;
use core::alloc::{GlobalAlloc, Layout};

mod boot_collector;
mod shell;

use arch_x86_64::cpu::{GlobalDescriptorTable, InterruptDescriptorTable};
use core_kernel::boot_info::BootInfo;
use core_kernel::process::ProcessId;

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
extern "C" fn idle_process() -> ! {
    loop {
        arch_x86_64::cpu::halt();
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

    // Enable EFER.NXE, CR0.WP, CR4.SMEP, CR4.SMAP before loading CR3.
    arch_x86_64::cpu::init_protection();

    // Load the kernel PML4 — from this point the CPU enforces the page table.
    // Identity mapping keeps phys == virt so execution continues uninterrupted.
    unsafe { arch_x86_64::cpu::activate_page_table(kernel_pml4_phys); }

    hal::uart::print_str("      └─ Page tables:     4-level PML4 (2 MB huge pages, all regions)\n");
    hal::uart::print_str("      └─ CR3 loaded:      ");
    hal::uart::print_hex(kernel_pml4_phys);
    hal::uart::print_str("\n");
    hal::uart::print_str("      └─ Protection:      NXE + WP + SMEP + SMAP enabled\n");
    hal::uart::print_str("      └─ Status:          ✓ OK\n\n");

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
    // STAGE 4: System Timer
    // -------------------------------------------------------------------------
    hal::uart::print_str("[4/7] System Timer\n");
    arch_x86_64::timer::init();
    hal::uart::print_str("      └─ PIT frequency:   100 Hz (10 ms ticks)\n");
    hal::uart::print_str("      └─ PIC configured:  Master & Slave\n");
    hal::uart::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 5: Process Management
    // -------------------------------------------------------------------------
    hal::uart::print_str("[5/7] Process Management\n");

    let test_process_stack = allocator.allocate(8192).expect("Failed to allocate stack");
    hal::uart::print_str("      └─ Process stack:   ");
    hal::uart::print_hex(test_process_stack as u64);
    hal::uart::print_str(" (8 KB)\n");
    hal::uart::print_str("      └─ Max processes:   32\n");
    hal::uart::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 6: Scheduler
    // -------------------------------------------------------------------------
    hal::uart::print_str("[6/7] Scheduler\n");

    let sched = core_kernel::scheduler::Scheduler::new();

    // Register the idle process (priority 255 — runs only when no other
    // process is Ready).  Its entry point is a hlt loop; the kernel
    // scheduler never calls add_process from ISR context, so this is safe.
    let idle_pid = sched.add_process(idle_process as *const () as u64, 0, kernel_pml4_phys);
    if let Some(idle) = idle_pid {
        sched.set_priority(idle, 255);
    }

    // Add the first user-visible process (placeholder entry point 0x400000 —
    // will be replaced by the ELF loader).
    let first_process = sched.add_process(0x400000, test_process_stack as u64, kernel_pml4_phys);

    // Update TSS.RSP0 and CURRENT_PID for the first process to run.
    if let Some(pid) = first_process {
        unsafe { arch_x86_64::cpu::set_rsp0(test_process_stack as u64 + 8192); }
        core_kernel::scheduler::CURRENT_PID
            .store(pid.as_u32(), core::sync::atomic::Ordering::Relaxed);
    }

    // Move the scheduler into the global slot — from this point on, timer
    // ticks and syscalls access it via core_kernel::scheduler::get_global().
    core_kernel::scheduler::init_global(sched);

    hal::uart::print_str("      └─ Algorithm:       Priority (lowest num = highest prio)\n");
    hal::uart::print_str("      └─ Time quantum:    10 ms\n");
    hal::uart::print_str("      └─ Idle process:    PID ");
    if let Some(p) = idle_pid {
        hal::uart::print_hex(p.as_u32() as u64);
        hal::uart::print_str(" (priority 255)\n");
    }
    hal::uart::print_str("      └─ First process:   PID ");
    if let Some(pid) = first_process {
        hal::uart::print_hex(pid.as_u32() as u64);
        hal::uart::print_str("\n");
    }
    hal::uart::print_str("      └─ TSS.RSP0:        Updated per context switch via tick_scheduler()\n");
    hal::uart::print_str("      └─ Audit log:       64-entry IPC ring buffer active\n");
    hal::uart::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 7: IPC
    // -------------------------------------------------------------------------
    hal::uart::print_str("[7/7] Inter-Process Communication\n");

    // Demonstrate IPC with sender-PID stamping and notification.
    if let Some(sender_pid) = first_process {
        if let Some(sched) = core_kernel::scheduler::get_global() {
            let mut test_msg = core_kernel::ipc::Message::new(ProcessId::new(0));
            test_msg.set_data(0, 0xDEAD_BEEF);
            // Kernel (PID 0) sends to first_process; kernel stamps sender = PID 0.
            sched.send_message(ProcessId::new(0), sender_pid, test_msg);
        }
    }

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

    arch_x86_64::cpu::enable_interrupts();
    hal::uart::print_str("✓ Interrupts enabled\n");
    hal::uart::print_str("Type 'help' for available commands.\n\n");

    shell::run()
}

// =============================================================================
// HELPERS
// =============================================================================

/// Print `bytes` as mebibytes (rounded down).
fn print_mib(bytes: u64) {
    hal::uart::print_dec(bytes / (1024 * 1024));
}

/// Print a u8 zero-padded to 2 digits (for timestamps).
fn print_padded_u8(n: u8) {
    if n < 10 { hal::uart::put_byte(b'0'); }
    hal::uart::print_dec(n as u64);
}

// =============================================================================
// CPU FEATURE CHECK
// =============================================================================

/// Verify that all hardware features required by the kernel are present.
/// Halts with a diagnostic message if any required feature is missing.
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

    if !ok {
        hal::uart::print_str("\nFATAL: CPU is missing required features. System halted.\n");
        loop { arch_x86_64::cpu::halt(); }
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
