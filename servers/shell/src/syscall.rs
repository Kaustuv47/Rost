//! Raw syscall wrappers.
//!
//! Syscall table (must match arch-x86_64/src/cpu/syscall.rs):
//!
//! |  # | Name        |
//! |----|-------------|
//! |  0 | yield       |
//! |  1 | exit        |
//! |  2 | getpid      |
//! |  3 | send        | 2-word shorthand (uart-drv byte output)
//! |  4 | recv        | returns word0 only (uart-drv byte input)
//! |  5 | notify      |
//! |  6 | recv_msg    | receive full 8-word Message into user buffer
//! |  7 | send_msg    | send full 8-word Message from user buffer
//! | 10 | register    | register this process under a name
//! | 11 | lookup      | look up PID by service name
//! | 14 | clock       | monotonic nanoseconds since boot (100 Hz → 10 ms resolution)
//! | 15 | setprio     | set scheduling priority (0=self, 0–255)
//! | 16 | setrt       | set real-time period (0=self, period_ticks; 0=disable)
//! | 17 | call        | synchronous call: send + block until reply
//! | 18 | cap_grant   | grant capability slot to another process
//! | 19 | setquota    | set resource quotas (memory pages, CPU budget, IPC rate)

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

/// Register this process under `name` in the kernel service registry.
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

/// Return nanoseconds since boot (100 Hz resolution → 10 ms granularity).
///
/// Suitable for uptime display and coarse timeout calculations.
/// Use `val / 1_000_000_000` for seconds, `val / 1_000_000` for milliseconds.
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

/// Set the scheduling priority of a process.
///
/// `pid = 0` sets the calling process's own priority.
/// Lower numbers = higher urgency; 0 = maximum, 255 = minimum (idle).
#[inline]
pub fn setprio(pid: u64, priority: u8) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      15u64,
            in("rdi")      pid,
            in("rsi")      priority as u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

/// Assign a real-time period to a process, enabling EDF scheduling.
///
/// `pid = 0` targets the calling process.
/// `period_ticks = 0` reverts to best-effort (priority-based) scheduling.
/// While active, the process preempts all best-effort processes.
#[inline]
pub fn setrt(pid: u64, period_ticks: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      16u64,
            in("rdi")      pid,
            in("rsi")      period_ticks,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

/// Synchronous call: send `req` to `to_pid`, block until a reply arrives.
///
/// `timeout_ticks = 0` means wait forever.
/// Returns `true` if a reply was received; the reply is written into `reply`.
/// Returns `false` on timeout (ETIMEDOUT) or if the target's mailbox is full.
///
/// The reply sender PID is available in `reply.sender` after a successful call.
#[inline]
pub fn call(to_pid: u64, req: &Msg, reply: &mut Msg, timeout_ticks: u64) -> bool {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      17u64,
            in("rdi")      to_pid,
            in("rsi")      req   as *const Msg as u64,
            in("rdx")      reply as *mut   Msg as u64,
            in("r10")      timeout_ticks,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret == 0
}

/// Grant the capability at `slot_idx` in the calling process's table to `to_pid`.
///
/// The capability must have the `CAP_G` (grant) right.
/// Returns `Ok(new_slot_idx)` on success.
/// Returns `Err(ret)` on failure: EPERM (no grant right), EINVAL (bad slot /
/// target table full), or ENOSYS.
#[inline]
pub fn cap_grant(slot_idx: usize, to_pid: u32) -> Result<usize, u64> {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      18u64,
            in("rdi")      slot_idx as u64,
            in("rsi")      to_pid   as u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    // Error codes are all near u64::MAX; valid slot indices are < CAP_TABLE_SIZE (64).
    if ret < 256 { Ok(ret as usize) } else { Err(ret) }
}

/// Set resource quotas for `pid` (0 = calling process).
///
/// All zero values mean "unlimited" for that field:
/// - `memory_pages`: max physical 4 KB pages the process may map (SYS_MAP)
/// - `cpu_budget`:   max scheduler ticks per frame before forced preemption
/// - `ipc_rate`:     max IPC sends per 100-tick window
///
/// Returns 0 on success.
#[inline]
pub fn setquota(pid: u32, memory_pages: u32, cpu_budget: u32, ipc_rate: u16) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      19u64,
            in("rdi")      pid          as u64,
            in("rsi")      memory_pages as u64,
            in("rdx")      cpu_budget   as u64,
            in("r10")      ipc_rate     as u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

