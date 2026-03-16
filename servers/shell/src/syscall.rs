//! Raw syscall wrappers.
//!
//! Calling convention — mirrors Linux x86_64 / Rost `dispatch_syscall`:
//!
//! | Register | Role |
//! |----------|------|
//! | rax | syscall number (in) / return value (out) |
//! | rdi | arg 0 |
//! | rsi | arg 1 |
//! | rdx | arg 2 |
//! | rcx | saved user RIP (clobbered by SYSCALL instruction) |
//! | r11 | saved user RFLAGS (clobbered by SYSCALL instruction) |
//!
//! Syscall table (must match arch-x86_64/src/cpu/syscall.rs):
//!
//! | # | Name    |
//! |---|---------|
//! | 0 | yield   |
//! | 1 | exit    |
//! | 2 | getpid  |
//! | 3 | send    |
//! | 4 | recv    |
//! | 5 | notify  |

/// Voluntarily give up the CPU slice.  The scheduler picks the next
/// ready process; we resume when our turn comes around again.
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

/// Terminate this process with an exit code.  Does not return.
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

/// Return the calling process's PID.
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

/// Send a message to `to_pid`.
///
/// `word0` and `word1` are the first two payload words.
/// The kernel stamps `msg.sender` — the target always sees the real PID.
///
/// Returns 0 on success, non-zero on error (EINVAL / EPERM).
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

/// Try to receive a message.
///
/// `timeout_ticks` controls blocking behaviour:
/// - `0`        → non-blocking (returns `u64::MAX` immediately if nothing waiting)
/// - `u64::MAX` → block forever until a message arrives
/// - other      → block up to N ticks, then return `u64::MAX`
///
/// Returns the first payload word of the received message, or `u64::MAX`
/// if no message was available within the timeout.
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

/// Post a notification word (bitmask) to `to_pid`'s pending_notification.
///
/// Cheaper than a full `send` — no mailbox slot consumed, just ORed into
/// a single u64.  The receiver calls `poll_notification()` to consume it.
/// If `to_pid` was blocked, it is unblocked immediately.
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
