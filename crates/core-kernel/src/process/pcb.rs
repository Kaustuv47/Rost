use super::ProcessId;

/// Per-process kernel stack — one slot per process, lives in BSS.
pub const KERNEL_STACK_SIZE: usize = 8192;
pub const MAX_KERNEL_STACKS: usize = 32;

// Zero-initialised in BSS; never on the Rust stack.
static mut KERNEL_STACKS: [[u8; KERNEL_STACK_SIZE]; MAX_KERNEL_STACKS] =
    [[0u8; KERNEL_STACK_SIZE]; MAX_KERNEL_STACKS];

static NEXT_STACK: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// Allocate the next free kernel stack slot.
/// Returns `(stack_id, stack_top_address)`.
pub fn alloc_kernel_stack() -> Option<(usize, u64)> {
    let id = NEXT_STACK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    if id >= MAX_KERNEL_STACKS { return None; }
    let top = unsafe { KERNEL_STACKS[id].as_ptr() as u64 + KERNEL_STACK_SIZE as u64 };
    Some((id, top))
}

// ── Process state ─────────────────────────────────────────────────────────────

#[derive(Copy, Clone, Debug)]
pub enum ProcessState {
    Ready,
    Running,
    Blocked,
    Terminated,
}

// ── Saved CPU context ─────────────────────────────────────────────────────────

/// All registers saved/restored by a voluntary context switch.
///
/// **Layout is load-bearing** — `arch_x86_64::context::switch_context` indexes
/// into this struct using hard-coded byte offsets. Keep fields in this exact
/// order and do not insert padding (guaranteed by `#[repr(C)]` + all-`u64`).
///
/// Field offsets (each field is 8 bytes):
/// ```text
///  0  rbx    8  rbp   16  r12   24  r13   32  r14   40  r15
/// 48  rax   56  rcx   64  rdx   72  rsi   80  rdi
/// 88  r8    96  r9   104  r10  112  r11
/// 120 rsp  128 rip   136 rflags
/// ```
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TaskContext {
    // ── Callee-saved (System V AMD64 ABI) ────────────────────────────────────
    pub rbx:    u64,  //   0
    pub rbp:    u64,  //   8
    pub r12:    u64,  //  16
    pub r13:    u64,  //  24
    pub r14:    u64,  //  32
    pub r15:    u64,  //  40
    // ── Caller-saved (populated by full preemptive save; zero for voluntary) ─
    pub rax:    u64,  //  48
    pub rcx:    u64,  //  56
    pub rdx:    u64,  //  64
    pub rsi:    u64,  //  72
    pub rdi:    u64,  //  80
    pub r8:     u64,  //  88
    pub r9:     u64,  //  96
    pub r10:    u64,  // 104
    pub r11:    u64,  // 112
    // ── Key state registers ───────────────────────────────────────────────────
    pub rsp:    u64,  // 120
    pub rip:    u64,  // 128
    pub rflags: u64,  // 136
}

impl TaskContext {
    pub const fn zero() -> Self {
        TaskContext {
            rbx: 0, rbp: 0, r12: 0, r13: 0, r14: 0, r15: 0,
            rax: 0, rcx: 0, rdx: 0, rsi: 0, rdi: 0,
            r8: 0, r9: 0, r10: 0, r11: 0,
            rsp: 0, rip: 0, rflags: 0x202, // IF set
        }
    }
}

// ── Process Control Block ─────────────────────────────────────────────────────

pub struct ProcessControlBlock {
    pub pid:              ProcessId,
    pub state:            ProcessState,
    /// Scheduling priority (0 = highest, 255 = lowest).  Default: 128.
    pub priority:         u8,

    // Saved CPU context (restored by `switch_context` on next run)
    pub context:          TaskContext,

    // Kernel stack for this process
    pub kernel_stack_id:  usize,
    pub kernel_rsp:       u64,   // current top of kernel stack

    pub page_table_base:  u64,
    pub time_slice:       u32,   // ticks per quantum
    pub cpu_time:         u32,   // ticks consumed in current quantum

    // ── Resource quotas ───────────────────────────────────────────────────────
    /// Maximum physical pages this process may map.  0 = unlimited (kernel).
    pub memory_quota_pages: u32,
    /// Per-major-frame CPU budget in ticks (temporal partitioning).  0 = unlimited.
    pub cpu_budget_ticks:   u32,
    /// Budget consumed so far in the current frame.
    pub cpu_budget_used:    u32,
    /// IPC message rate limit — max messages per 100-tick window.  0 = unlimited.
    pub ipc_rate_limit:     u16,
    /// Messages sent in the current 100-tick window.
    pub ipc_rate_used:      u16,

    // ── Timing ────────────────────────────────────────────────────────────────
    /// Total lifetime ticks this process has consumed (for `ps`/accounting).
    pub total_cpu_ticks:    u64,
    /// If `state == Blocked`, tick at which the process should be unblocked.
    /// `u64::MAX` means no timeout.
    pub blocked_deadline:   u64,

    // Per-process message mailbox
    pub mailbox:          crate::ipc::MessageQueue,
}

impl ProcessControlBlock {
    /// Create a new PCB.  `entry_point` is where the process starts executing.
    /// `user_stack_top` is the initial RSP for the process's user/kernel context.
    pub fn new(pid: ProcessId, entry_point: u64, _user_stack_top: u64, page_table_base: u64) -> Option<Self> {
        let (stack_id, kern_stack_top) = alloc_kernel_stack()?;

        // The kernel stack starts empty; rsp sits 8 bytes below the top so the
        // entry function sees a properly-aligned stack (ABI: rsp % 16 == 8 at entry,
        // as if a `call` had just pushed a return address). The 8-byte slot at
        // [kernel_stack_top - 8] is zero-initialised (BSS) — acts as a sentinel
        // return address should the process ever return from its entry function.
        let kern_rsp = kern_stack_top - 8;

        // Install entry_point as the fake "return address" at the top of the kernel
        // stack.  switch_context restores rsp to kern_rsp and executes `ret`, which
        // pops this value and transfers control to entry_point.
        unsafe { *(kern_rsp as *mut u64) = entry_point; }

        let mut ctx = TaskContext::zero();
        ctx.rsp    = kern_rsp;
        ctx.rip    = entry_point;    // informational — control flow uses the stack
        ctx.rflags = 0x202;          // IF=1, IOPL=0

        Some(ProcessControlBlock {
            pid,
            state:              ProcessState::Ready,
            priority:           128,
            context:            ctx,
            kernel_stack_id:    stack_id,
            kernel_rsp:         kern_rsp,
            page_table_base,
            time_slice:         10,
            cpu_time:           0,
            memory_quota_pages: 0,
            cpu_budget_ticks:   0,
            cpu_budget_used:    0,
            ipc_rate_limit:     0,
            ipc_rate_used:      0,
            total_cpu_ticks:    0,
            blocked_deadline:   u64::MAX,
            mailbox:            crate::ipc::MessageQueue::new(),
        })
    }
}
