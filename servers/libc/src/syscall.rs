//! Raw Rost syscall wrappers used internally by rost-libc.
//!
//! Syscall numbers must match `arch-x86_64/src/cpu/syscall.rs`.

#![allow(dead_code)] // Several wrappers are API surface, not all used by the library itself.

// ── Message struct (core_kernel::ipc::Message layout) ────────────────────────
//   offset 0:  sender: ProcessId(u32)  = 4 bytes
//   offset 4:  _pad                    = 4 bytes
//   offset 8:  data: [u64; 8]          = 64 bytes
//   total: 72 bytes

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Msg {
    pub sender: u32,
    pub _pad:   u32,
    pub data:   [u64; 8],
}

impl Msg {
    #[inline]
    pub const fn zeroed() -> Self {
        Msg { sender: 0, _pad: 0, data: [0u64; 8] }
    }
}

// ── Syscall wrappers ──────────────────────────────────────────────────────────

#[inline]
pub fn yield_() {
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      0u64,
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
pub fn notify(to_pid: u32, word: u64) -> bool {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      5u64,
            in("rdi")      to_pid as u64,
            in("rsi")      word,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret == 0
}

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

/// SYS_MAP — map a 4 KB virtual page.
///
/// `paddr = 0` allocates a fresh physical page from the kernel.
/// flags: bit 0 = writable, bit 1 = user-mode accessible.
#[inline]
pub fn sys_map(vaddr: u64, paddr: u64, flags: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      9u64,
            in("rdi")      vaddr,
            in("rsi")      paddr,
            in("rdx")      flags,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

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

#[inline]
pub fn uart_write(byte: u8) {
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      12u64,
            in("rdi")      byte as u64,
            lateout("rax") _,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
}

#[inline]
pub fn uart_read() -> Option<u8> {
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
    if ret == u64::MAX { None } else { Some(ret as u8) }
}

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

/// SYS_CALL — synchronous send + block until reply.
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
