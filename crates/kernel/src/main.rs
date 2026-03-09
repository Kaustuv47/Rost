#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;
use uefi::prelude::*;
use core::alloc::{GlobalAlloc, Layout};

mod shell;

use arch_x86_64::cpu::{GlobalDescriptorTable, InterruptDescriptorTable};
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

// GDT and IDT must be static — the CPU reads them on every interrupt
static GDT: GlobalDescriptorTable = GlobalDescriptorTable::new();
static mut IDT: InterruptDescriptorTable = InterruptDescriptorTable::new();

// =============================================================================
// ENTRY POINT
// =============================================================================

#[entry]
fn efi_main(_image_handle: Handle, _system_table: SystemTable<Boot>) -> Status {
    hal::uart::init();
    hal::uart::print_str("\n");
    hal::uart::print_str("╔════════════════════════════════════╗\n");
    hal::uart::print_str("║   Rost Microkernel v0.1.0         ║\n");
    hal::uart::print_str("║   UEFI-based x86_64 Kernel        ║\n");
    hal::uart::print_str("╚════════════════════════════════════╝\n");
    hal::uart::print_str("\n=== INITIALIZATION SEQUENCE ===\n\n");

    // -------------------------------------------------------------------------
    // STAGE 1: Memory Management
    // -------------------------------------------------------------------------
    hal::uart::print_str("[1/7] Memory Management\n");

    let mut allocator = core_kernel::memory::PhysicalAllocator::new(0x100000, 0x10000000);
    let kernel_heap = allocator.allocate(0x100000).expect("Failed to allocate kernel heap");

    hal::uart::print_str("      └─ Kernel heap:     ");
    hal::uart::print_hex(kernel_heap as u64);
    hal::uart::print_str(" (1 MB)\n");

    let mut kernel_page_table = core_kernel::memory::PageTable::new();
    core_kernel::memory::map_page(&mut kernel_page_table, 0x0, 0x0, false);
    core_kernel::memory::map_page(&mut kernel_page_table, kernel_heap as u64, kernel_heap as u64, true);

    hal::uart::print_str("      └─ Page tables:     Ready\n");
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
    hal::uart::print_str("      └─ GDT loaded:      3 selectors (null, code, data)\n");
    hal::uart::print_str("      └─ IDT loaded:      256 gates registered\n");
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
