//! Raw syscall wrappers for the VFS server.
//! Identical calling convention to servers/shell/src/syscall.rs.
//!
//! | # | Name      | Used |
//! |---|-----------|------|
//! | 1 | exit      | ✓ panic handler |
//! | 5 | notify    | ✓ signal init on startup / panic |
//! | 6 | recv_msg  | ✓ main dispatch loop |
//! | 7 | send_msg  | ✓ IPC responses |
//! |10 | register  | ✓ register as "rost-vfs" |

/// IPC message — must match core_kernel::ipc::Message layout exactly:
///   offset 0: sender u32   (4 bytes)
///   offset 4: _pad   u32   (4 bytes — alignment pad for u64)
///   offset 8: data  [u64; 8]  (64 bytes)
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
pub fn notify(to_pid: u64, word: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      5u64,
            in("rdi")      to_pid,
            in("rsi")      word,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

/// Block until a full Message arrives (or timeout expires).
/// Returns `true` if a message was received.
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

/// Register the calling process under `name` in the kernel service registry.
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

/// Look up a service name in the kernel registry.
/// Returns the PID, or `u64::MAX` if not registered.
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

/// Synchronous IPC: send a Message and block until the recipient replies.
/// On return `msg` contains the reply.  Returns `true` on success.
#[inline]
pub fn call(to_pid: u64, msg: &mut Msg) -> bool {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      17u64,
            in("rdi")      to_pid,
            in("rsi")      msg as *mut Msg as u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret == 0
}

/// Send a full Message.  The kernel overwrites msg.sender with our PID.
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

/// Write one byte directly to COM1 (SYS_UART_WRITE = 12).  For diagnostics only.
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
