//! Raw syscall wrappers for rost-net.
//!
//! Syscall table (must match arch-x86_64/src/cpu/syscall.rs):
//!
//! |  # | Name          |
//! |----|---------------|
//! |  0 | yield         |
//! |  1 | exit          |
//! |  2 | getpid        |
//! |  6 | recv_msg      |
//! |  7 | send_msg      |
//! | 10 | register      |
//! | 11 | lookup        |
//! | 12 | uart_write    |
//! | 14 | clock         |
//! | 17 | call          |
//! | 29 | ioport_out    |
//! | 30 | ioport_in     |
//! | 31 | phys_addr     |
//! | 32 | irq_register  |

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

/// Cooperative yield — give up the CPU slice voluntarily.
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

/// Terminate the calling process.
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

/// Receive a full Message into `buf`.
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

/// Send a full Message. Returns `true` on success.
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

/// Look up a PID by service name. Returns `u64::MAX` if not found.
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

/// Write one byte directly to COM1 via SYS_UART_WRITE.
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

/// Return monotonic nanoseconds since boot (100 Hz resolution = 10 ms).
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

/// Synchronous call: send `req` to `to_pid`, block until reply.
/// `timeout_ticks = 0` means wait forever.
#[inline]
pub fn call_msg(to: u64, send: &Msg, recv: &mut Msg, timeout: u64) -> bool {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      17u64,
            in("rdi")      to,
            in("rsi")      send as *const Msg as u64,
            in("rdx")      recv as *mut   Msg as u64,
            in("r10")      timeout,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret == 0
}

/// Write `val` (width bytes) to I/O port.
/// width: 1 = byte, 2 = word, 4 = dword.
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

/// Read `width` bytes from I/O port. Returns value.
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

/// Register this process as the interrupt handler for PCI GSI `gsi` (8–15).
///
/// The kernel will route the IOAPIC GSI → IDT vector (32+GSI) and, on each
/// interrupt, read `isr_port` (to de-assert the virtio line) then deliver an
/// IPC message with `data[0] = 0xFFFF_0000 | gsi`.
///
/// Returns `true` on success.
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

/// Translate a virtual address to its physical address.
/// Returns `None` if the mapping does not exist.
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
    // Kernel returns u64::MAX or u64::MAX-small_offset on error
    if ret >= u64::MAX - 255 { None } else { Some(ret) }
}

// ── Print helpers ─────────────────────────────────────────────────────────────

/// Print a byte string to COM1.
pub fn print(s: &[u8]) {
    for &b in s { uart_write(b); }
}

/// Print a decimal u64 to COM1.
pub fn print_dec(mut n: u64) {
    if n == 0 { uart_write(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut i = 20usize;
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    for &b in &buf[i..] { uart_write(b); }
}

/// Print a hex u64 (with 0x prefix) to COM1.
pub fn print_hex(n: u64) {
    uart_write(b'0');
    uart_write(b'x');
    let hex = b"0123456789abcdef";
    for shift in (0..16).rev() {
        uart_write(hex[((n >> (shift * 4)) & 0xF) as usize]);
    }
}
