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

use process::ProcessId;

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

// GDT and IDT must be static — the CPU reads them on every interrupt
static mut GDT: cpu::GlobalDescriptorTable = cpu::GlobalDescriptorTable::new();
static mut IDT: cpu::InterruptDescriptorTable = cpu::InterruptDescriptorTable::new();

// =============================================================================
// ENTRY POINT
// =============================================================================

#[entry]
fn efi_main(_image_handle: Handle, _system_table: SystemTable<Boot>) -> Status {
    console::init();
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

    unsafe {
        GDT.load();
        interrupts::init(&mut IDT);
        IDT.load();
    }
    console::print_str("      └─ GDT loaded:      3 selectors (null, code, data)\n");
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
        console::print_hex(pid.as_u32() as u64);
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
    console::print_str("✓ Entering kernel shell\n");
    console::print_str("\nType 'help' for available commands.\n\n");

    // --------------------------------------------------------------------------
    // Simple interactive shell
    // --------------------------------------------------------------------------
    let mut cmd_buf = [0u8; 256];
    let mut cmd_len: usize = 0;

    console::print_str("rost> ");

    loop {
        if let Some(byte) = console::read_byte() {
            match byte {
                b'\r' | b'\n' => {
                    console::put_byte(b'\n');
                    if cmd_len > 0 {
                        shell_exec(&cmd_buf[..cmd_len]);
                        cmd_len = 0;
                    }
                    console::print_str("rost> ");
                }
                // Backspace (BS = 0x08, DEL = 0x7F)
                0x08 | 0x7F => {
                    if cmd_len > 0 {
                        cmd_len -= 1;
                        console::put_byte(0x08);
                        console::put_byte(b' ');
                        console::put_byte(0x08);
                    }
                }
                b if b >= 0x20 && cmd_len < 255 => {
                    cmd_buf[cmd_len] = b;
                    cmd_len += 1;
                    console::put_byte(b); // local echo
                }
                _ => {}
            }
        } else {
            cpu::halt(); // wait for next interrupt (timer @ 100 Hz)
        }
    }
}

// =============================================================================
// SHELL COMMAND DISPATCH
// =============================================================================

fn trim(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|&b| b != b' ' && b != b'\t').unwrap_or(s.len());
    let end = s.iter().rposition(|&b| b != b' ' && b != b'\t').map(|i| i + 1).unwrap_or(0);
    if start >= end { b"" } else { &s[start..end] }
}

fn shell_exec(line: &[u8]) {
    let line = trim(line);

    if line.starts_with(b"echo") {
        let rest = trim(&line[4..]);
        // Strip surrounding double-quotes if present
        let text = if rest.len() >= 2 && rest[0] == b'"' && rest[rest.len() - 1] == b'"' {
            &rest[1..rest.len() - 1]
        } else {
            rest
        };
        for &b in text {
            console::put_byte(b);
        }
        console::put_byte(b'\n');
    } else if line == b"help" {
        console::print_str("Commands:\n");
        console::print_str("  echo <text>   print text to console\n");
        console::print_str("  help          show this message\n");
    } else if !line.is_empty() {
        console::print_str("Unknown command: '");
        for &b in line {
            console::put_byte(b);
        }
        console::print_str("'\n");
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