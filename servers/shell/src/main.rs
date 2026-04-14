//! Rost Shell — default userspace interactive shell.
//!
//! This is the ring-3 replacement for the kernel's emergency console.
//! It runs as a normal process with no kernel privileges and communicates
//! with hardware exclusively through IPC:
//!
//! ```text
//!   rost-shell  ──SYS_SEND──►  uart-drv  ──port I/O──►  COM1
//!               ◄─SYS_RECV──   uart-drv  ◄─port I/O──   COM1
//! ```
//!
//! # Prerequisites before this binary can run
//! - ELF loader implemented (loads this .elf into a ring-3 address space)
//! - Ring-3 process entry (`iretq` to CS=0x20)
//! - `servers/uart-drv` running as PID 2 and forwarding keystrokes
//!
//! # Build
//! ```sh
//! cd servers
//! cargo build --target x86_64-unknown-none
//! # produces: target/x86_64-unknown-none/debug/rost-shell
//! ```
#![no_std]
#![no_main]

mod gop;
mod io;
mod shell;
mod syscall;

use shell::Shell;

/// Userspace entry point — the ELF loader jumps here after mapping segments.
///
/// Stack is set up per System V AMD64 ABI:
///   [rsp]     = argc
///   [rsp + 8] = argv[0] pointer
///   ...
#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Register under "rost-shell" so uart-drv can discover us and push
    // keystroke IPC messages to our queue.
    syscall::register(b"rost-shell\0\0\0\0\0\0");

    // Notify init (PID 1) that the shell is alive.
    syscall::notify(1, 0x5348454C_4C524459); // "SHELLRDY" as two u32s

    Shell::new().run()
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // Notify init (PID 1) about the crash so it can restart us.
    syscall::notify(1, 0x5348454C_4C455252); // "SHELLERR"
    syscall::exit(1);
}
