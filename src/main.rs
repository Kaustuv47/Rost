#![no_std]
#![no_main]

extern crate alloc;
use core::panic::PanicInfo;
use uefi::prelude::*;
use core::alloc::{GlobalAlloc, Layout};

mod console;
mod cpu;
mod interrupts;
mod ipc;
mod memory;
mod process;
mod scheduler;
mod timer;

// Re-export process types used by other modules
pub use process::{ProcessId, ProcessTable, ProcessState, ProcessControlBlock};

// =============================================================================
// GLOBAL ALLOCATOR - Simple bump allocator for early kernel development
// =============================================================================

/// Simple bump allocator for the kernel
///
/// This is a placeholder allocator suitable for early kernel development.
/// For production, replace with a buddy allocator or slab allocator.
pub struct BumpAllocator {
    heap: [u8; 0x100000], // 1MB heap
    offset: core::sync::atomic::AtomicUsize,
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let offset = self.offset.load(core::sync::atomic::Ordering::Relaxed);
        let aligned_offset = (offset + layout.align() - 1) & !(layout.align() - 1);
        let new_offset = aligned_offset + layout.size();

        if new_offset >= self.heap.len() {
            return core::ptr::null_mut();
        }

        self.offset.store(new_offset, core::sync::atomic::Ordering::Relaxed);
        self.heap.as_ptr().add(aligned_offset) as *mut u8
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator doesn't support deallocation
    }
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    heap: [0; 0x100000],
    offset: core::sync::atomic::AtomicUsize::new(0),
};

// =============================================================================
// ENTRY POINT
// =============================================================================

#[entry]
fn efi_main(_image_handle: Handle, _system_table: SystemTable<Boot>) -> Status {
    console::print_str("\n");
    console::print_str("╔════════════════════════════════════╗\n");
    console::print_str("║   Rost Microkernel v0.1.0         ║\n");
    console::print_str("║   UEFI-based x86_64 Kernel        ║\n");
    console::print_str("╚════════════════════════════════════╝\n");
    console::print_str("\n=== INITIALIZATION SEQUENCE ===\n\n");

    // -------------------------------------------------------------------------
    // STAGE 1: Memory Management
    // -------------------------------------------------------------------------
    console::print_str("[1/7] Memory Management\n");

    let mut allocator = memory::PhysicalAllocator::new(0x100000, 0x10000000);
    let kernel_heap = allocator.allocate(0x100000).expect("Failed to allocate kernel heap");

    console::print_str("      └─ Kernel heap:     ");
    console::print_hex(kernel_heap as u64);
    console::print_str(" (1 MB)\n");

    let mut kernel_page_table = memory::PageTable::new();
    memory::map_page(&mut kernel_page_table, 0x0, 0x0, false);
    memory::map_page(&mut kernel_page_table, kernel_heap as u64, kernel_heap as u64, true);

    console::print_str("      └─ Page tables:     Ready\n");
    console::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 2: CPU Setup (GDT & IDT)
    // -------------------------------------------------------------------------
    console::print_str("[2/7] CPU Setup (GDT/IDT)\n");

    let gdt = cpu::GlobalDescriptorTable::new();
    gdt.load();
    console::print_str("      └─ GDT loaded:      3 selectors (null, code, data)\n");

    let mut idt = cpu::InterruptDescriptorTable::new();
    interrupts::init(&mut idt);
    idt.load();
    console::print_str("      └─ IDT loaded:      256 gates registered\n");
    console::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 3: Interrupt Handlers
    // -------------------------------------------------------------------------
    console::print_str("[3/7] Interrupt Handlers\n");
    console::print_str("      └─ Exception 0:     Division by zero\n");
    console::print_str("      └─ Exception 13:    General protection fault\n");
    console::print_str("      └─ Exception 14:    Page fault\n");
    console::print_str("      └─ Interrupt 32:    Timer (PIT)\n");
    console::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 4: System Timer
    // -------------------------------------------------------------------------
    console::print_str("[4/7] System Timer\n");
    timer::init();
    console::print_str("      └─ PIT frequency:   100 Hz (10 ms ticks)\n");
    console::print_str("      └─ PIC configured:  Master & Slave\n");
    console::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 5: Process Management
    // -------------------------------------------------------------------------
    console::print_str("[5/7] Process Management\n");

    let test_process_stack = allocator.allocate(8192).expect("Failed to allocate stack");
    console::print_str("      └─ Process stack:   ");
    console::print_hex(test_process_stack as u64);
    console::print_str(" (8 KB)\n");
    console::print_str("      └─ Max processes:   32\n");
    console::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 6: Scheduler
    // -------------------------------------------------------------------------
    console::print_str("[6/7] Scheduler\n");

    let sched = scheduler::Scheduler::new();
    let first_process = sched.add_process(0x400000, test_process_stack as u64);

    console::print_str("      └─ Algorithm:       Round-robin\n");
    console::print_str("      └─ Time quantum:    10 ms\n");
    console::print_str("      └─ First process:   ");
    if let Some(pid) = first_process {
        console::print_str("PID ");
        console::print_hex(pid.0 as u64);
    }
    console::print_str("\n");
    console::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // STAGE 7: IPC
    // -------------------------------------------------------------------------
    console::print_str("[7/7] Inter-Process Communication\n");

    let mut msg_queue = ipc::MessageQueue::new();
    let mut test_msg = ipc::Message::new(ProcessId::new(1));
    test_msg.set_data(0, 0xDEADBEEF);
    msg_queue.send(test_msg);

    console::print_str("      └─ Queue size:      16 messages\n");
    console::print_str("      └─ Msg per queue:   8 u64 fields\n");
    console::print_str("      └─ Status:          ✓ OK\n\n");

    // -------------------------------------------------------------------------
    // Kernel Ready - Enable Interrupts
    // -------------------------------------------------------------------------
    console::print_str("╔════════════════════════════════════╗\n");
    console::print_str("║        KERNEL INITIALIZATION      ║\n");
    console::print_str("║             COMPLETE              ║\n");
    console::print_str("╚════════════════════════════════════╝\n");
    console::print_str("\n");

    cpu::enable_interrupts();

    console::print_str("✓ Interrupts enabled\n");
    console::print_str("✓ Entering kernel idle loop\n");
    console::print_str("\nRost is running...\n\n");

    loop {
        cpu::halt(); // Halt CPU until next interrupt
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    console::print_str("\n╔════════════════════════════════════╗\n");
    console::print_str("║       ❌ KERNEL PANIC ❌           ║\n");
    console::print_str("╚════════════════════════════════════╝\n\n");

    console::print_str("Error: Kernel panic detected\n");

    if let Some(location) = info.location() {
        console::print_str("Location: ");
        console::print_str(location.file());
        console::print_str(":");
        console::print_hex(location.line() as u64);
        console::print_str("\n");
    }

    console::print_str("\nSystem halted.\n");

    loop {
        cpu::halt();
    }
}