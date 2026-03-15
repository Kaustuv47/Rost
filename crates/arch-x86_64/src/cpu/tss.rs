/// x86_64 Task State Segment (TSS).
///
/// The CPU uses the TSS for two purposes in long mode:
///   1. **RSP0** — loaded into RSP on any ring-3 → ring-0 transition (syscall,
///      interrupt, exception while CPL=3).  Must point to the top of the current
///      process's kernel stack; updated by the scheduler on every context switch.
///   2. **IST entries** — unconditional stack switches for critical exceptions
///      (#DF, #NMI, #MC) that must run on a dedicated stack regardless of what
///      the interrupted task was doing.
///
/// # Selector
/// The TSS occupies GDT slots 5 and 6 (two 8-byte system-descriptor words).
/// Selector = 5 × 8 = **0x28**.
///
/// Layout reference: Intel SDM Vol. 3A §7.2.3 (64-Bit Mode TSS Format).
#[repr(C, packed)]
pub struct TaskStateSegment {
    _reserved0:  u32,
    /// Ring-0 RSP — kernel stack pointer for ring-3 → ring-0 transitions.
    pub rsp0:    u64,
    /// Ring-1/2 RSPs (unused in Rost; zeroed).
    pub rsp1:    u64,
    pub rsp2:    u64,
    _reserved1:  u64,
    /// IST1 — dedicated stack for #DF (double fault).  Never overlaps RSP0.
    pub ist1:    u64,
    /// IST2–IST7 (reserved for future use — #NMI, #MC, etc.).
    pub ist2:    u64,
    pub ist3:    u64,
    pub ist4:    u64,
    pub ist5:    u64,
    pub ist6:    u64,
    pub ist7:    u64,
    _reserved2:  u64,
    _reserved3:  u16,
    /// I/O permission bitmap offset.  Set to `sizeof(TSS)` = no IOPB present.
    pub iopb:    u16,
}

const _TSS_SIZE_CHECK: () = assert!(core::mem::size_of::<TaskStateSegment>() == 104);

/// Size of each IST stack (8 KB — two pages).
pub const IST_STACK_SIZE: usize = 8192;

/// Dedicated stack for #DF ISR (IST1).  Lives in BSS; never grows below `IST1_STACK[0]`.
static mut IST1_STACK: [u8; IST_STACK_SIZE] = [0u8; IST_STACK_SIZE];

/// Dedicated stack for #NMI ISR (IST2).
static mut IST2_STACK: [u8; IST_STACK_SIZE] = [0u8; IST_STACK_SIZE];

/// The single kernel TSS.
pub static mut TSS: TaskStateSegment = TaskStateSegment {
    _reserved0: 0,
    rsp0: 0, rsp1: 0, rsp2: 0,
    _reserved1: 0,
    ist1: 0, ist2: 0, ist3: 0, ist4: 0, ist5: 0, ist6: 0, ist7: 0,
    _reserved2: 0, _reserved3: 0,
    iopb: core::mem::size_of::<TaskStateSegment>() as u16,
};

/// Initialise the TSS IST stacks and return a pointer to the TSS.
///
/// Must be called before `ltr` and before any ring-3 code or user-fault-path
/// exception can fire.
pub fn init_tss() -> *mut TaskStateSegment {
    unsafe {
        // IST1 = top of double-fault stack (stack grows downward).
        TSS.ist1 = core::ptr::addr_of!(IST1_STACK) as u64 + IST_STACK_SIZE as u64;
        // IST2 = top of NMI stack.
        TSS.ist2 = core::ptr::addr_of!(IST2_STACK) as u64 + IST_STACK_SIZE as u64;
        core::ptr::addr_of_mut!(TSS)
    }
}

/// Update TSS.RSP0 to the given kernel-stack top.
///
/// Must be called on every context switch so that a subsequent syscall or
/// interrupt-while-ring-3 lands on the correct kernel stack.
///
/// # Safety
/// `rsp0` must be a valid kernel stack top (8-byte aligned, mapped writable).
#[inline]
pub unsafe fn set_rsp0(rsp0: u64) {
    core::ptr::addr_of_mut!(TSS).as_mut().unwrap().rsp0 = rsp0;
}

/// Load the TSS selector (0x28) into the Task Register.
///
/// # Safety
/// The TSS descriptor at GDT index 5/6 must be correctly initialised with the
/// TSS physical address before this is called.
pub unsafe fn load_tss() {
    core::arch::asm!(
        "ltr {0:x}",
        in(reg) 0x28u16,
        options(nostack, nomem),
    );
}
