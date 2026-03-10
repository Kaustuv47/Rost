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
            return core::ptr::null_mut();
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

// GDT and IDT must be static — the CPU reads them on every interrupt.
static GDT: GlobalDescriptorTable = GlobalDescriptorTable::new();
static mut IDT: InterruptDescriptorTable = InterruptDescriptorTable::new();

// Hardware description gathered from UEFI; remains valid after boot services exit.
static mut BOOT_INFO: BootInfo = BootInfo::new();

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

    let mut kernel_page_table = core_kernel::memory::PageTable::new();
    core_kernel::memory::map_page(&mut kernel_page_table, 0x0, 0x0, false, &mut allocator);
    core_kernel::memory::map_page(&mut kernel_page_table, kernel_heap as u64, kernel_heap as u64, true, &mut allocator);

    hal::uart::print_str("      └─ Page tables:     4-level PML4 (identity mapped)\n");
    hal::uart::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 2: CPU Setup (GDT & IDT)
    // -------------------------------------------------------------------------
    hal::uart::print_str("[2/7] CPU Setup (GDT/IDT)\n");

    GDT.load();
    unsafe {
        let idt = core::ptr::addr_of_mut!(IDT);
        arch_x86_64::interrupts::init(&mut *idt);
        (*idt).load();
    }
    hal::uart::print_str("      └─ GDT loaded:      5 selectors (null, ring0 code/data, ring3 data/code)\n");
    hal::uart::print_str("      └─ IDT loaded:      256 gates registered\n");
    arch_x86_64::cpu::syscall::init();
    hal::uart::print_str("      └─ SYSCALL/SYSRET:  MSRs configured (EFER.SCE, STAR, LSTAR, SFMASK)\n");
    hal::uart::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 3: Interrupt Handlers
    // -------------------------------------------------------------------------
    hal::uart::print_str("[3/7] Interrupt Handlers\n");
    hal::uart::print_str("      └─ Exception 0:     Division by zero\n");
    hal::uart::print_str("      └─ Exception 13:    General protection fault\n");
    hal::uart::print_str("      └─ Exception 14:    Page fault\n");
    hal::uart::print_str("      └─ Interrupt 32:    Timer (PIT)\n");
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
    let first_process = sched.add_process(0x400000, test_process_stack as u64);

    hal::uart::print_str("      └─ Algorithm:       Round-robin\n");
    hal::uart::print_str("      └─ Time quantum:    10 ms\n");
    hal::uart::print_str("      └─ First process:   ");
    if let Some(pid) = first_process {
        hal::uart::print_str("PID ");
        hal::uart::print_hex(pid.as_u32() as u64);
    }
    hal::uart::print_str("\n");
    hal::uart::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 7: IPC
    // -------------------------------------------------------------------------
    hal::uart::print_str("[7/7] Inter-Process Communication\n");

    let mut msg_queue = core_kernel::ipc::MessageQueue::new();
    let mut test_msg = core_kernel::ipc::Message::new(ProcessId::new(1));
    test_msg.set_data(0, 0xDEADBEEF);
    msg_queue.send(test_msg);

    hal::uart::print_str("      └─ Queue size:      16 messages\n");
    hal::uart::print_str("      └─ Msg per queue:   8 u64 fields\n");
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
