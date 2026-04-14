//! Raw syscall wrappers for rost-pci-bus.
//!
//! Syscall numbers must match `arch-x86_64/src/cpu/syscall.rs`.

#![allow(dead_code)]

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

/// Synchronous call: send `req` to `to_pid`, block until reply.
/// Uses same buffer for send and receive (VFS pattern: a2=a3=&mut msg).
#[inline]
pub fn call(to_pid: u64, msg: &mut Msg) -> bool {
    let ret: u64;
    let ptr = msg as *mut Msg as u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      17u64,
            in("rdi")      to_pid,
            in("rsi")      ptr,
            in("rdx")      ptr,
            in("r10")      200u64, // 200-tick timeout
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret == 0
}

/// Write `val` (width bytes) to I/O port.  width: 1=byte, 2=word, 4=dword.
#[inline]
pub fn ioport_out(port: u16, val: u32, width: u8) {
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      29u64,
            in("rdi")      port  as u64,
            in("rsi")      val   as u64,
            in("rdx")      width as u64,
            lateout("rax") _,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
}

/// Read `width` bytes from I/O port.  Returns value.
#[inline]
pub fn ioport_in(port: u16, width: u8) -> u32 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      30u64,
            in("rdi")      port  as u64,
            in("rsi")      width as u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret as u32
}

/// Translate virtual → physical address.
#[inline]
pub fn phys_addr(virt: u64) -> Option<u64> {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      31u64,
            in("rdi")      virt,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    if ret >= u64::MAX - 255 { None } else { Some(ret) }
}

/// Register this process as the IRQ owner for GSI `gsi` (8–15).
#[inline]
pub fn irq_register(gsi: u8, isr_port: u16) -> bool {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      32u64,
            in("rdi")      gsi      as u64,
            in("rsi")      isr_port as u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret == 0
}

/// Map a 4 KB page at `vaddr` backed by `paddr` (0 = alloc fresh page).
/// flags: bit0=writable, bit1=user-mode.
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

/// Get primary GOP framebuffer info into a 32-byte user struct.
/// Returns 0 on success, non-zero if no framebuffer.
#[inline]
pub fn get_framebuf(out: &mut FbQueryResult) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      33u64,
            in("rdi")      out as *mut FbQueryResult as u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

/// Result struct for `SYS_GET_FRAMEBUF` (33).  Must be 32 bytes, 8-byte aligned.
#[repr(C, align(8))]
#[derive(Default)]
pub struct FbQueryResult {
    pub base:   u64,
    pub size:   u64,
    pub width:  u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
}

pub fn print(s: &[u8]) {
    for &b in s { uart_write(b); }
}

pub fn print_dec(mut n: u64) {
    if n == 0 { uart_write(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut i = 20usize;
    while n > 0 { i -= 1; buf[i] = b'0' + (n % 10) as u8; n /= 10; }
    for &b in &buf[i..] { uart_write(b); }
}

pub fn print_hex(n: u64) {
    uart_write(b'0'); uart_write(b'x');
    let hex = b"0123456789abcdef";
    for shift in (0..16).rev() { uart_write(hex[((n >> (shift * 4)) & 0xF) as usize]); }
}
