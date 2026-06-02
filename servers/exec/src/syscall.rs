//! Syscall wrappers for rost-exec.
//!
//! | # | Name              | Used |
//! |---|-------------------|------|
//! |  1 | exit             | panic handler |
//! |  2 | getpid           | own PID at startup |
//! |  6 | recv_msg         | receive OP_EXEC requests |
//! |  7 | send_msg         | reply to callers |
//! |  9 | map              | map physical pages into exec's staging area |
//! | 10 | register         | register as "exec" |
//! | 12 | uart_write       | debug serial output |
//! | 34 | alloc_phys       | allocate a 4 KB physical frame |
//! | 35 | create_vas       | allocate PML4 with kernel pages merged |
//! | 36 | map_into_vas     | map a frame into an arbitrary PML4 |
//! | 37 | spawn_with_vas   | create ring-3 process from pre-built PML4 |
//! | 38 | register_frames  | register ELF segment frames with a PID's PCB |

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

#[allow(dead_code)]
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

/// Map a physical frame into exec server's own VAS.
///
/// `flags`: bit 0 = writable, bit 1 = user-accessible (must be set for ring-3 access).
#[inline]
pub fn map(vaddr: u64, paddr: u64, flags: u64) -> bool {
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

/// Allocate a zeroed 4 KB physical frame.
/// Returns the physical address, or 0 on OOM.
#[inline]
pub fn alloc_phys() -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      34u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

/// Allocate a new PML4 with kernel pages already merged in.
/// Returns pml4_paddr, or 0 on OOM.
#[inline]
pub fn create_vas() -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      35u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

/// Map `paddr` at `vaddr` in the PML4 at `pml4_paddr`.
///
/// `flags`: bit 0=WRITABLE, bit 1=USER, bit 2=NO_EXECUTE.
/// Returns true on success.
#[inline]
pub fn map_into_vas(pml4_paddr: u64, vaddr: u64, paddr: u64, flags: u64) -> bool {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      36u64,
            in("rdi")      pml4_paddr,
            in("rsi")      vaddr,
            in("rdx")      paddr,
            in("r10")      flags,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret == 0
}

/// Create a ring-3 process from a pre-built PML4.
///
/// The kernel allocates and maps the user stack automatically.
/// Returns new PID on success, u32::MAX on failure.
#[inline]
pub fn spawn_with_vas(pml4_paddr: u64, entry: u64, priority: u8) -> Option<u32> {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      37u64,
            in("rdi")      pml4_paddr,
            in("rsi")      entry,
            in("rdx")      priority as u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    if ret >= u64::MAX - 7 { None } else { Some(ret as u32) }
}

/// Register physical frames with a PID's PCB for reclaim on exit.
///
/// `frames` must be a slice of 4 KB-aligned physical addresses.
/// Returns true on success.
#[inline]
pub fn register_frames(pid: u32, frames: &[u64]) -> bool {
    if frames.is_empty() { return true; }
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax")      38u64,
            in("rdi")      pid as u64,
            in("rsi")      frames.as_ptr() as u64,
            in("rdx")      frames.len() as u64,
            lateout("rax") ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret == 0
}
