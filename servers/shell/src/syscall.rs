//! Raw syscall wrappers.
//!
//! Syscall table (must match arch-x86_64/src/cpu/syscall.rs):
//!
//! | # | Name        |
//! |---|-------------|
//! | 0 | yield       |
//! | 1 | exit        |
//! | 2 | getpid      |
//! | 3 | send        | 2-word shorthand (uart-drv byte output)
//! | 4 | recv        | returns word0 only (uart-drv byte input)
//! | 5 | notify      |
//! | 6 | recv_msg    | receive full 8-word Message into user buffer
//! | 7 | send_msg    | send full 8-word Message from user buffer

// ── Message struct (must match core_kernel::ipc::Message layout) ─────────────
//
// kernel layout (#[repr(C)]):
//   offset 0:  sender: ProcessId(u32)   = 4 bytes
//   offset 4:  _pad                     = 4 bytes (u64 alignment)
//   offset 8:  data: [u64; 8]           = 64 bytes
//   total: 72 bytes

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

// ── Syscall wrappers ──────────────────────────────────────────────────────────

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

/// Send a 2-word message — used for uart-drv byte output.
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

/// Receive one word — used for uart-drv byte input.
///
/// Returns `u64::MAX` if no message within timeout.
#[inline]
pub fn recv(timeout_ticks: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      4u64,
            in("rdi")      timeout_ticks,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
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

/// Receive a full Message into `buf`.
///
/// Returns `true` if a message was received, `false` on timeout.
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

/// Send a full Message from `msg`.
///
/// The kernel stamps `msg.sender` — user cannot forge source PID.
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
