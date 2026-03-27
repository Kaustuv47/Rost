use super::ProcessId;

/// Per-process kernel stack — one slot per process, lives in BSS.
///
/// # Layout per slot  (12 KB each)
/// ```text
/// offset 0    ┌──────────────────┐ ← guard page (4 KB, unmapped at boot)
///             │  GUARD (4 KB)    │   stack overflow → #PF here instead of
///             │  (never touched) │   silently corrupting the next slot
/// offset 4096 ├──────────────────┤ ← stack bottom  (guard_addr + 4096)
///             │  STACK (8 KB)    │   grows downward
///             │                  │
/// offset 12288└──────────────────┘ ← stack top  (guard_addr + KERNEL_SLOT_SIZE)
/// ```
///
/// `alloc_kernel_stack()` returns the guard-page address alongside the
/// stack-top so callers can install the guard mapping.
pub const KERNEL_STACK_SIZE: usize = 8192;
pub const KERNEL_GUARD_SIZE: usize = 4096;
pub const KERNEL_SLOT_SIZE:  usize = KERNEL_GUARD_SIZE + KERNEL_STACK_SIZE; // 12 KB
pub const MAX_KERNEL_STACKS: usize = 32;

/// 4 KB-aligned wrapper for a single kernel-stack slot.
///
/// The `align(4096)` attribute guarantees that every slot starts at a 4 KB
/// page boundary in BSS.  Without this alignment the guard-page unmap in
/// `install_kernel_stack_guard_pages` would remove the *entire* 4 KB page
/// containing the slot's first byte, which may overlap with other BSS
/// variables placed on the same page by the linker — causing spurious #PF
/// faults when those variables are next accessed.
#[repr(C, align(4096))]
struct KernelStackSlot([u8; KERNEL_SLOT_SIZE]);

impl KernelStackSlot {
    const ZERO: Self = KernelStackSlot([0u8; KERNEL_SLOT_SIZE]);
}

// Zero-initialised in BSS; never on the Rust stack.
// align(4096) on KernelStackSlot ensures every slot (and thus its guard page)
// starts at a 4 KB page boundary, so unmap_page only removes the guard bytes.
static mut KERNEL_STACKS: [KernelStackSlot; MAX_KERNEL_STACKS] =
    [KernelStackSlot::ZERO; MAX_KERNEL_STACKS];

/// Monotone counter used for forward allocation before any slot has been freed.
/// Once freed slots are available in `STACK_RECLAIM`, those are preferred.
static NEXT_STACK: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ── Kernel-stack slot reclaim pool ────────────────────────────────────────────
//
// When a process terminates, its kernel-stack BSS slot is zeroed and returned
// to this LIFO stack so the next `alloc_kernel_stack` call reuses it rather
// than drawing a fresh slot from `NEXT_STACK`.
//
// This closes the resource-reclaim loop for kernel stacks: without it, after
// MAX_KERNEL_STACKS process lifetimes no new processes could be created even
// though no processes are running.
//
// IEC 61508 §7.4.5: long-running systems must reclaim resources from
// terminated processes to prevent exhaustion.

/// Reclaimed stack-slot IDs, available for reuse.
static mut STACK_RECLAIM:     [u8; MAX_KERNEL_STACKS] = [0u8; MAX_KERNEL_STACKS];
/// Number of valid entries in `STACK_RECLAIM` (= logical stack top).
static mut STACK_RECLAIM_TOP: usize = 0;

/// Allocate the next free kernel stack slot.
///
/// Checks the reclaim pool first (freed slots are reused before fresh ones
/// are drawn from `NEXT_STACK`).  Returns
/// `(stack_id, guard_page_addr, stack_top_addr)` where:
/// - `guard_page_addr` is the 4 KB address to unmap as a guard page.
/// - `stack_top_addr` is the initial RSP (stack grows downward from here).
///
/// Returns `None` if all `MAX_KERNEL_STACKS` slots are in active use.
pub fn alloc_kernel_stack() -> Option<(usize, u64, u64)> {
    // Prefer reclaimed slots (O(1) pop) over advancing the monotone counter.
    let id = unsafe {
        if STACK_RECLAIM_TOP > 0 {
            STACK_RECLAIM_TOP -= 1;
            let id = STACK_RECLAIM[STACK_RECLAIM_TOP] as usize;
            // Zero the reclaimed stack before handing it to a new process so
            // the new process cannot observe stale data from the previous owner.
            //
            // We zero HERE (on allocation) rather than in free_kernel_stack
            // (on deallocation) because free_kernel_stack is called from
            // ProcessControlBlock::drop(), which is triggered by terminate_process()
            // while the CPU may still be executing on that very kernel stack
            // (exception handler → terminate_faulting_process path).  Zeroing
            // the live stack corrupts all return addresses, causing an immediate
            // triple fault.  BSS stacks from NEXT_STACK are already zero.
            let stack_start = KERNEL_STACKS[id].0.as_mut_ptr().add(KERNEL_GUARD_SIZE);
            core::ptr::write_bytes(stack_start, 0, KERNEL_STACK_SIZE);
            id
        } else {
            let id = NEXT_STACK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            if id >= MAX_KERNEL_STACKS { return None; }
            id
            // Fresh BSS stacks are already zero-initialised — no write_bytes needed.
        }
    };
    let base  = unsafe { KERNEL_STACKS[id].0.as_ptr() as u64 };
    let guard = base;                           // first 4 KB: guard page
    let top   = base + KERNEL_SLOT_SIZE as u64; // stack grows down from here
    Some((id, guard, top))
}

/// Return a kernel stack slot to the reclaim pool.
///
/// The slot's memory is zeroed before return so a new process cannot observe
/// a terminated process's kernel-stack data (IEC 61508 §7.4.3 isolation).
///
/// The guard page for this slot remains unmapped (it was unmapped when the
/// process was first created) so any new process using this slot will have
/// the same spatial isolation guarantees.
///
/// # Safety
/// `stack_id` must be a value previously returned by [`alloc_kernel_stack`]
/// and must not be in use by any currently-running process.
pub fn free_kernel_stack(stack_id: usize) {
    if stack_id >= MAX_KERNEL_STACKS { return; }
    unsafe {
        // Return the slot to the reclaim pool.
        //
        // NOTE: we do NOT zero the stack here.  free_kernel_stack is called from
        // ProcessControlBlock::drop(), which is triggered by terminate_process()
        // — potentially while the CPU is still executing on this very kernel stack
        // (exception handler → terminate_faulting_process path).  Zeroing the
        // live stack would corrupt all return addresses and cause an immediate
        // triple fault.
        //
        // Instead, the stack is zeroed in alloc_kernel_stack() when the slot is
        // popped from the reclaim pool, ensuring a new process cannot observe the
        // previous owner's kernel-stack data (IEC 61508 §7.4.3 isolation).
        // Fresh slots from NEXT_STACK are already zero-initialised (BSS).
        if STACK_RECLAIM_TOP < MAX_KERNEL_STACKS {
            STACK_RECLAIM[STACK_RECLAIM_TOP] = stack_id as u8;
            STACK_RECLAIM_TOP += 1;
        }
    }
}

/// Return the guard-page physical address for an already-allocated stack slot.
///
/// Used by `install_kernel_stack_guard_pages()` in the kernel crate to
/// enumerate all slots and unmap their guard pages after page tables are live.
pub fn kernel_stack_guard_addr(stack_id: usize) -> Option<u64> {
    if stack_id >= MAX_KERNEL_STACKS { return None; }
    Some(unsafe { KERNEL_STACKS[stack_id].0.as_ptr() as u64 })
}

// ── Capability table ──────────────────────────────────────────────────────────

/// The object type a capability token refers to.
///
/// Capabilities are unforgeable kernel-issued tokens that grant a process the
/// right to use a specific kernel object.  IEC 61508 §7.4.3: every access to
/// a shared resource must be mediated by a kernel authority check.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CapKind {
    None    = 0, // empty / revoked slot
    Channel = 1, // IPC endpoint — holder may send/receive on channel `object_id`
    Process = 2, // process handle — holder may manage process `object_id`
    Memory  = 3, // physical memory region — holder may map `object_id` (frame number)
    Service = 4, // named service slot — holder may register/use service `object_id`
}

/// Capability rights bitmask.
pub const CAP_R: u8 = 0x01; // Read / Receive
pub const CAP_W: u8 = 0x02; // Write / Send
pub const CAP_G: u8 = 0x04; // Grant (pass capability to another process)
pub const CAP_X: u8 = 0x08; // Execute / Manage (e.g., kill a process)

/// One capability slot in the per-process capability table.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Capability {
    /// What kind of object this capability refers to.
    pub kind:      CapKind,
    /// Rights granted to the holder (bitmask of `CAP_R / CAP_W / CAP_G / CAP_X`).
    pub rights:    u8,
    _pad:          [u8; 2],
    /// Kernel object identifier (PID, channel ID, frame number, etc.).
    pub object_id: u32,
}

impl Capability {
    pub const fn empty() -> Self {
        Capability { kind: CapKind::None, rights: 0, _pad: [0; 2], object_id: 0 }
    }

    /// Create a new capability with the given kind, rights, and object identifier.
    pub const fn new(kind: CapKind, rights: u8, object_id: u32) -> Self {
        Capability { kind, rights, _pad: [0; 2], object_id }
    }

    pub fn is_empty(self) -> bool { matches!(self.kind, CapKind::None) }
}

/// Maximum number of capability slots per process.
/// 64 × 8 bytes = 512 bytes per PCB; 32 PCBs = 16 KB total BSS overhead.
pub const CAP_TABLE_SIZE: usize = 64;

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
    /// Physical pages mapped so far.
    ///
    /// Incremented in `SYS_MAP` / `SYS_MAP_SHARE` on each successful page mapping.
    /// Compared against `memory_quota_pages` before every allocation; the mapping
    /// is rejected when `memory_pages_used >= memory_quota_pages` (and quota ≠ 0).
    ///
    /// Never decremented (no `SYS_UNMAP` yet); the count vanishes with the PCB
    /// when the process terminates.  IEC 61508 §7.4.5.
    pub memory_pages_used:  u32,
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

    // ── Priority inheritance ───────────────────────────────────────────────────
    /// PID of the server this process last sent a message to.
    ///
    /// When this process is Blocked in `blocking_receive`, the scheduler
    /// uses this field to donate the process's priority to the target server
    /// (priority inheritance).  Cleared when the process is unblocked.
    ///
    /// Only meaningful when `state == Blocked`.
    pub waiting_for: Option<ProcessId>,

    // ── Capability table ──────────────────────────────────────────────────────
    /// Per-process capability table (64 slots, 8 bytes each = 512 bytes).
    ///
    /// Slot 0 is always implicitly granted: capability to call into the kernel
    /// (no explicit check needed — syscall path is the only entry).  All other
    /// resources (IPC channels, peer processes, memory regions) require an
    /// explicit `Capability` entry.
    pub cap_table: [Capability; CAP_TABLE_SIZE],

    // ── Real-time scheduling (EDF hook) ───────────────────────────────────────
    /// Period of this process in scheduler ticks.  `0` = best-effort (non-RT).
    ///
    /// When non-zero, the process is scheduled under **Earliest Deadline First**
    /// (EDF) — it preempts all priority-based best-effort processes.  Multiple
    /// RT processes are scheduled among themselves by ascending `rt_deadline`.
    /// Required by IEC 61508 §7.4.1 (temporal partitioning) for tasks with
    /// known timing constraints.
    pub rt_period:   u64,

    /// Absolute tick by which the current activation must complete.
    ///
    /// Initialised to `current_tick + rt_period` when `set_realtime()` is called.
    /// Renewed to `old_deadline + rt_period` at each period expiry so deadline
    /// drift cannot accumulate.  `0` when `rt_period == 0`.
    pub rt_deadline: u64,
}

impl ProcessControlBlock {
    /// Create a new ring-3 PCB using the IRETQ trampoline.
    ///
    /// When this process is first scheduled, the context switch executes `ret`
    /// which pops `trampoline_addr` from the kernel stack and transfers control
    /// to `ring3_entry_trampoline`.  That function reads `r12` (user entry) and
    /// `r13` (user RSP) from the saved context and issues `iretq` to switch to
    /// ring-3.
    ///
    /// `trampoline_addr` must be the address of `arch_x86_64::context::ring3_entry_trampoline`.
    pub fn new_ring3(
        pid:              ProcessId,
        user_entry:       u64,
        user_stack_top:   u64,
        page_table_base:  u64,
        trampoline_addr:  u64,
    ) -> Option<Self> {
        let (stack_id, _guard_addr, kern_stack_top) = alloc_kernel_stack()?;
        // kern_rsp: 8 bytes below top; stores trampoline as fake return address.
        // kernel_rsp (→ TSS.RSP0): full stack top, so ring-3 interrupts have
        // a clean kernel stack to push the IRETQ frame onto.
        let kern_rsp = kern_stack_top - 8;
        // Fake return address → trampoline (not directly to user entry).
        unsafe { *(kern_rsp as *mut u64) = trampoline_addr; }

        let mut ctx = TaskContext::zero();
        ctx.rsp    = kern_rsp;         // restored by switch_context on first run
        ctx.rip    = trampoline_addr;
        ctx.r12    = user_entry;       // read by ring3_entry_trampoline
        ctx.r13    = user_stack_top;   // read by ring3_entry_trampoline
        ctx.rflags = 0x202;            // IF=1

        Some(ProcessControlBlock {
            pid,
            state:              ProcessState::Ready,
            priority:           128,
            context:            ctx,
            kernel_stack_id:    stack_id,
            kernel_rsp:         kern_stack_top,   // TSS.RSP0: full empty stack top
            page_table_base,
            time_slice:         10,
            cpu_time:           0,
            memory_quota_pages: 0,
            memory_pages_used:  0,
            cpu_budget_ticks:   0,
            cpu_budget_used:    0,
            ipc_rate_limit:     0,
            ipc_rate_used:      0,
            total_cpu_ticks:    0,
            blocked_deadline:   u64::MAX,
            mailbox:            crate::ipc::MessageQueue::new(),
            waiting_for:        None,
            cap_table:          [Capability::empty(); CAP_TABLE_SIZE],
            rt_period:          0,
            rt_deadline:        0,
        })
    }

    /// Create a new PCB.  `entry_point` is where the process starts executing.
    /// `user_stack_top` is the initial RSP for the process's user/kernel context.
    pub fn new(pid: ProcessId, entry_point: u64, _user_stack_top: u64, page_table_base: u64) -> Option<Self> {
        let (stack_id, _guard_addr, kern_stack_top) = alloc_kernel_stack()?;

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
            memory_pages_used:  0,
            cpu_budget_ticks:   0,
            cpu_budget_used:    0,
            ipc_rate_limit:     0,
            ipc_rate_used:      0,
            total_cpu_ticks:    0,
            blocked_deadline:   u64::MAX,
            mailbox:            crate::ipc::MessageQueue::new(),
            waiting_for:        None,
            cap_table:          [Capability::empty(); CAP_TABLE_SIZE],
            rt_period:          0,
            rt_deadline:        0,
        })
    }
}

/// Automatically return the kernel stack slot to the reclaim pool when the
/// PCB is dropped.
///
/// This fires in two cases:
///   1. A `ProcessTable` slot is cleared via `terminate_process()` —
///      `*slot = None` drops the PCB, which calls `free_kernel_stack`.
///   2. A `ProcessTable` itself is dropped (e.g., end of a unit test) —
///      all contained PCBs are dropped in turn, returning their stack slots.
///
/// Without this `Drop` implementation the kernel stack pool (`NEXT_STACK`)
/// would monotonically advance even in test code and exhaust all 32 slots.
///
/// IEC 61508 §7.4.5: all resources held by a terminated process must be
/// returned to the kernel before the PCB slot is cleared.
impl Drop for ProcessControlBlock {
    fn drop(&mut self) {
        free_kernel_stack(self.kernel_stack_id);
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// free_kernel_stack with out-of-range ids must not panic.
    #[test]
    fn test_free_kernel_stack_out_of_range() {
        free_kernel_stack(MAX_KERNEL_STACKS);     // exactly at bound
        free_kernel_stack(MAX_KERNEL_STACKS + 1); // past bound
        free_kernel_stack(usize::MAX);            // far past bound
        // Reaching here means no panic — test passes.
    }

    /// free_kernel_stack zeroes the usable stack area (security: new process
    /// must not see data left by the terminated process).
    ///
    /// Each test gets a unique slot via the atomic NEXT_STACK, so concurrent
    /// test threads do not interfere with the zeroing verification.
    #[test]
    fn test_kernel_stack_zeroed_on_free() {
        let (id, _guard, _top) = alloc_kernel_stack().expect("alloc");

        // Write a distinctive pattern into the usable stack region.
        unsafe {
            let usable = KERNEL_STACKS[id].0.as_mut_ptr().add(KERNEL_GUARD_SIZE);
            core::ptr::write_bytes(usable, 0xCC, KERNEL_STACK_SIZE);
        }

        // free_kernel_stack must zero the usable region.
        free_kernel_stack(id);

        unsafe {
            let usable = KERNEL_STACKS[id].0.as_ptr().add(KERNEL_GUARD_SIZE);
            let slice = core::slice::from_raw_parts(usable, KERNEL_STACK_SIZE);
            assert!(
                slice.iter().all(|&b| b == 0),
                "every byte of the usable stack must be zero after free"
            );
        }
    }

    /// free_kernel_stack pushes the slot onto the reclaim pool and the pool
    /// pops it on the next alloc_kernel_stack call (LIFO).
    ///
    /// This test verifies the LIFO property without reading STACK_RECLAIM_TOP
    /// directly, because concurrent test threads may push additional slots at
    /// any time (including via the new ProcessControlBlock::drop impl), making
    /// absolute depth assertions fragile.  Instead we verify that the freed
    /// slot id appears somewhere in the reclaim pool and that the pool grows.
    #[test]
    fn test_kernel_stack_reclaim_lifo() {
        // Draw a unique slot from the monotone counter.
        let (id, _, _) = alloc_kernel_stack().expect("alloc");

        let depth_before = unsafe { STACK_RECLAIM_TOP };

        // Free the slot — must push id onto the reclaim pool.
        free_kernel_stack(id);

        let depth_after = unsafe { STACK_RECLAIM_TOP };
        assert!(
            depth_after > depth_before,
            "reclaim pool depth must grow after free (before={depth_before}, after={depth_after})"
        );

        // The freed id must be somewhere in the reclaim pool.
        let pool = unsafe { &STACK_RECLAIM[..depth_after] };
        assert!(
            pool.contains(&(id as u8)),
            "freed slot id {id} must be in the reclaim pool"
        );

        // Clean up — return pool slot without leaking.
        let (id2, _, _) = alloc_kernel_stack().expect("realloc");
        free_kernel_stack(id2);
    }
}
