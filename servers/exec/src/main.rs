//! rost-exec — ring-3 ELF exec server.
//!
//! Handles all runtime ELF spawning requests from user processes (e.g. the
//! shell's `exec` command).  Receiving ELF bytes via physical-address IPC
//! and performing all ELF parsing in ring-3, it uses four new kernel
//! primitives (SYS_ALLOC_PHYS/CREATE_VAS/MAP_INTO_VAS/SPAWN_WITH_VAS) to
//! build the new address space and create the process without any kernel-mode
//! ELF parsing.
//!
//! # IPC Protocol
//!
//! Callers (e.g. shell) use `SYS_CALL` (17) — the call blocks the caller
//! until exec server replies via `SYS_SEND_MSG` (7).
//!
//! **Request (OP_EXEC):**
//! ```text
//!   data[0] = OP_EXEC (0x8000)
//!   data[1] = physical base address of contiguous ELF buffer (from SYS_PHYS_ADDR)
//!   data[2] = ELF byte length
//!   data[3] = priority (0 = default 128)
//! ```
//!
//! **Reply:**
//! ```text
//!   data[0] = OP_EXEC_REPLY (0x8001)
//!   data[1] = new PID on success, u32::MAX on failure
//! ```
//!
//! # Physical address contiguity
//!
//! The caller passes a single physical base address.  This works because
//! static BSS buffers (like the shell's EXEC_BUF) are allocated in one
//! contiguous run by the bitmap physical allocator during ELF loading —
//! consecutive SYS_ALLOC_PHYS calls during segment mapping return adjacent
//! frames.

#![no_std]
#![no_main]

mod elf;
mod syscall;

use syscall::Msg;

const MY_NAME: &[u8] = b"exec\0\0\0\0\0\0\0\0\0\0\0\0";

const OP_EXEC:       u64 = 0x8000;
const OP_EXEC_REPLY: u64 = 0x8001;

// SYS_MAP flags (bit 0 = writable, bit 1 = user-accessible).
const MAP_USER_RO: u64 = 0b10;

// Maximum ELF size we accept (512 KB = 128 pages).
const MAX_ELF_PAGES: usize = 128;

fn print_str(s: &str) {
    for b in s.bytes() { syscall::uart_write(b); }
}

#[allow(dead_code)]
fn print_hex(v: u64) {
    print_str("0x");
    for shift in (0..16u32).rev() {
        let nibble = (v >> (shift * 4)) & 0xF;
        let c = if nibble < 10 { b'0' + nibble as u8 } else { b'a' + (nibble as u8 - 10) };
        syscall::uart_write(c);
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    syscall::register(MY_NAME);
    print_str("[exec] rost-exec ready (ring-3 ELF loader)\n");

    let mut msg = Msg::zeroed();
    loop {
        if !syscall::recv_msg(u64::MAX, &mut msg) { continue; }

        match msg.data[0] {
            OP_EXEC => handle_exec(&msg),
            _       => {} // ignore unknown opcodes
        }
    }
}

fn handle_exec(msg: &Msg) {
    let phys_base  = msg.data[1];
    let elf_len    = msg.data[2] as usize;
    let priority   = msg.data[3] as u8;
    let caller_pid = msg.sender;

    let new_pid = exec_from_phys(phys_base, elf_len, priority);

    let mut reply = Msg::zeroed();
    reply.data[0] = OP_EXEC_REPLY;
    reply.data[1] = new_pid.unwrap_or(u32::MAX) as u64;
    syscall::send_msg(caller_pid as u64, &reply);
}

/// Map the caller's ELF buffer (given as a contiguous physical address range)
/// into exec server's staging area, then parse and spawn the ELF.
fn exec_from_phys(phys_base: u64, elf_len: usize, priority: u8) -> Option<u32> {
    if elf_len == 0 || phys_base == 0 { return None; }

    let pages = (elf_len + 4095) / 4096;
    if pages > MAX_ELF_PAGES { return None; }

    // Allocate a contiguous run of fresh VAs in exec server's staging area
    // for the input pages (read-only view of caller's ELF buffer).
    // We use elf::next_staging_va() here via a local helper since input staging
    // is logically separate from output staging — both use the same cursor.
    let input_va_base = alloc_staging_run(pages);

    for i in 0..pages {
        let pa = phys_base + (i as u64 * 4096);
        let va = input_va_base + (i as u64 * 4096);
        if !syscall::map(va, pa, MAP_USER_RO) {
            print_str("[exec] failed to map input page\n");
            return None;
        }
    }

    elf::load_and_spawn(input_va_base, elf_len, priority)
}

// ── Staging VA allocator (input pages) ────────────────────────────────────────
//
// exec server's elf.rs has its own STAGING_VA cursor for output (segment) pages.
// This separate cursor is for input pages (the caller's ELF buffer).
// Both cursors grow monotonically from 1 GB but in different ranges:
//   input  staging:  0x4000_0000 – 0x8000_0000  (input pages here first)
//   output staging:  above that (elf.rs starts at 0x4000_0000, grows up)
//
// In practice only one exec at a time; the ranges don't conflict.

static mut INPUT_STAGING_VA: u64 = 0x0000_8000_0000; // 2 GB mark for input

fn alloc_staging_run(pages: usize) -> u64 {
    // SAFETY: exec server is single-threaded.
    let base = unsafe { INPUT_STAGING_VA };
    unsafe { INPUT_STAGING_VA = INPUT_STAGING_VA.wrapping_add((pages as u64) * 4096); }
    base
}

// ── Panic handler ──────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    print_str("[exec] PANIC\n");
    syscall::exit(1);
}
