//! Minimal syscall wrappers for rost-init.
//!
//! Syscall numbers must match arch-x86_64/src/cpu/syscall.rs.
//!
//! | # | Name           | Used |
//! |---|----------------|------|
//! | 1 | exit           | ✓ panic handler / ordered shutdown |
//! | 2 | getpid         | ✓ discover own PID at startup |
//! | 6 | recv_msg       | ✓ receive fault notifications + heartbeats |
//! |10 | register       | ✓ register as "init" so SYS_LOOKUP finds us |
//! |11 | lookup         | ✓ resolve peer service names to PIDs |
//! |12 | uart_write     | ✓ direct serial output (bypasses uart-drv queue) |
//! |14 | clock          | ✓ monotonic timestamp for heartbeat watchdog |
//! |27 | restart_server | ✓ ask kernel to respawn a named server |

/// Message layout (must match core_kernel::ipc::Message + ProcessId padding).
///
/// Kernel layout (#[repr(C)]):
///   offset 0:  sender: ProcessId(u32)   = 4 bytes
///   offset 4:  _pad                     = 4 bytes (alignment to u64)
///   offset 8:  data: [u64; 8]           = 64 bytes
///   total: 72 bytes
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

#[inline]
pub fn getpid() -> u32 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      2u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret as u32
}

/// Receive a full Message into `buf`.
///
/// Returns `true` if a message was received before the timeout.
/// `timeout_ticks = u64::MAX` blocks indefinitely.
/// `timeout_ticks = 0` returns immediately (non-blocking poll).
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

/// Register this process under `name` in the kernel service registry.
/// `name` must be a null-terminated ASCII slice padded to 16 bytes.
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
/// Returns `u64::MAX` if the service is not yet registered.
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

/// Write one byte directly to COM1 via kernel SYS_UART_WRITE (syscall 12).
///
/// Bypasses the uart-drv IPC queue so init can log without a running uart-drv.
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

/// Return nanoseconds since boot (100 Hz resolution → 10 ms granularity).
#[inline]
pub fn clock() -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      14u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

/// Send a full Message to `to_pid` (SYS_SEND_MSG = 7).
///
/// Used to reply to SYS_CALL (17) callers: the caller blocks until someone
/// sends a message to its mailbox; `send_msg` delivers that reply.
/// Returns `true` on success.
#[inline]
pub fn send_msg(to_pid: u64, msg: &Msg) -> bool {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      7u64,
            in("rdi")      to_pid,
            in("rsi")      msg as *const Msg as u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret == 0
}

/// Ask the kernel to restart a named server from its embedded ELF image.
///
/// `name` must be a 16-byte null-padded ASCII service name slice, e.g.
/// `b"uart-drv\0\0\0\0\0\0\0\0"`.
///
/// Returns `Some(new_pid)` on success, `None` if the name is unknown or
/// the process table / memory is exhausted.
#[inline]
pub fn restart_server(name: &[u8]) -> Option<u32> {
    debug_assert!(name.len() >= 16, "restart_server: name must be >= 16 bytes");
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      27u64,
            in("rdi")      name.as_ptr() as u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    // EINVAL = u64::MAX - 1; ENOSYS = u64::MAX — both mean failure
    if ret >= u64::MAX - 7 { None } else { Some(ret as u32) }
}
