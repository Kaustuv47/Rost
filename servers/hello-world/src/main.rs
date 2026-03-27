//! hello-world — minimal ring-3 demo process.
//!
//! Prints a banner over the kernel serial console (SYS_UART_WRITE = 12)
//! and exits cleanly (SYS_EXIT = 1).  No dependencies; no heap.
//!
//! This binary is embedded in the VFS RAM disk at /bin/hello so that the
//! shell's `exec /bin/hello` command has a real ELF to spawn.
#![no_std]
#![no_main]

/// Print a byte to COM1 via the kernel SYS_UART_WRITE syscall (12).
#[inline(always)]
fn put_byte(b: u8) {
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")  12u64,
            in("rdi")  b as u64,
            lateout("rax") _,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
}

fn print(s: &str) {
    for b in s.bytes() { put_byte(b); }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    print("[hello-world] Hello from ring-3!\n");
    print("[hello-world] SYS_SPAWN_ELF works.\n");
    // SYS_EXIT(0)
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") 1u64,
            in("rdi") 0u64,
            options(nostack, noreturn),
        );
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // SYS_EXIT(1)
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") 1u64,
            in("rdi") 1u64,
            options(nostack, noreturn),
        );
    }
}
