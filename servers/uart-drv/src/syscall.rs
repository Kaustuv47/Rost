//! Syscall wrappers for rost-uart-drv.
//!
//! Syscall table (must match arch-x86_64/src/cpu/syscall.rs):
//!
//! |  # | Name        | Used by uart-drv |
//! |----|-------------|-----------------|
//! |  0 | yield       | ✓ |
//! |  1 | exit        | ✓ panic handler |
//! |  3 | send        | ✓ push byte to shell |
//! |  6 | recv_msg    | ✓ receive write requests |
//! | 10 | register    | ✓ register "uart-drv" |
//! | 11 | lookup      | ✓ find shell PID |
//! | 12 | uart_write  | ✓ write byte to COM1 |
//! | 13 | uart_read   | ✓ read byte from COM1 |

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Msg {
    pub sender: u32,
    pub _pad:   u32,
    pub data:   [u64; 8],
}

impl Msg {
    pub const fn zeroed() -> Self {
        Msg { sender: 0, _pad: 0, data: [0; 8] }
    }
}

#[inline]
pub fn yield_cpu() {
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")  0u64,
            lateout("rax") _,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
}

#[inline]
pub fn exit(code: u64) -> ! {
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") 1u64,
            in("rdi") code,
            options(nostack, noreturn),
        );
    }
}

/// Send a 2-word message (word0, word1) to `to_pid`.
#[inline]
pub fn send(to_pid: u64, word0: u64, word1: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      3u64,
            in("rdi")      to_pid,
            in("rsi")      word0,
            in("rdx")      word1,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

/// Receive a full Message into `buf`.  Returns true if a message arrived.
#[inline]
pub fn recv_msg(timeout_ticks: u64, buf: &mut Msg) -> bool {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      6u64,
            in("rdi")      timeout_ticks,
            in("rsi")      buf as *mut Msg as u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret != u64::MAX
}

/// Register the calling process under the given name.
/// `name` must be a null-terminated ASCII slice of ≤ 15 visible chars.
#[inline]
pub fn register(name: &[u8]) -> bool {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      10u64,
            in("rdi")      name.as_ptr() as u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret == 0
}

/// Look up a PID by service name.
/// Returns u64::MAX if the service is not yet registered.
#[inline]
pub fn lookup(name: &[u8]) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      11u64,
            in("rdi")      name.as_ptr() as u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

/// Write one byte to COM1 via kernel syscall (ring-3 has no IOPL).
#[inline]
pub fn uart_write(byte: u8) {
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")  12u64,
            in("rdi")  byte as u64,
            lateout("rax") _,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
}

/// Non-blocking read from COM1 via kernel syscall.
///
/// Returns the byte value (0–255) if one is available, or `u64::MAX` if not.
#[inline]
pub fn uart_read() -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      13u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}
