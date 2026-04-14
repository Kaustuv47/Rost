# Chapter 3 — Physical Memory Management

## 3.1 The Memory Management Problem

When the kernel starts, it has no allocator.  Before you can run any Rust code
that touches a heap, you need a heap.  Before you can build page tables, you need
physical frames.  Before you can spawn a process, you need a kernel stack.

Everything in a kernel's initialization sequence has this chicken-and-egg quality.
Rost resolves it with a two-level memory management architecture:

1. **BumpAllocator** — a trivially simple allocation-only (no free) allocator for
   the kernel's own Rust heap.  It lives inside the kernel binary itself as a 1 MB
   `static` array.  This is used by the `alloc` crate for kernel data structures
   like `Vec`, `Box`, and `String`.

2. **Physical Frame Allocator** — a bitmap-based allocator for 4 KB physical frames.
   Used for page tables, kernel stacks, user-space stacks, and ELF segment pages.

## 3.2 The Kernel Heap: `BumpAllocator`

```rust
pub struct BumpAllocator {
    heap: [u8; 0x100000], // 1 MB static array in BSS
    offset: AtomicUsize,
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let offset = self.offset.load(Ordering::Relaxed);
        let aligned = (offset + layout.align() - 1) & !(layout.align() - 1);
        let new_offset = aligned + layout.size();
        if new_offset >= self.heap.len() {
            hal::uart::print_str("\n[OOM] Kernel heap exhausted — system halted.\n");
            loop { core::arch::asm!("cli", "hlt", options(nostack, nomem)); }
        }
        self.offset.store(new_offset, Ordering::Relaxed);
        self.heap.as_ptr().add(aligned) as *mut u8
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    heap: [0; 0x100000],
    offset: AtomicUsize::new(0),
};
```

### 3.2.1 Why a Bump Allocator?

The bump allocator is the simplest possible allocator.  To allocate N bytes,
align the current offset to the requested alignment and advance it by N.  There
is no free list, no coalescing, no fragmentation — just a monotone pointer
marching through a fixed buffer.

For a kernel that uses the heap only during initialization (building static data
structures that live for the entire kernel lifetime), a bump allocator is
entirely adequate.  The kernel heap holds things like:

- The scheduler's process table
- The IPC audit log
- Service registry entries
- Transient boot-time strings

None of these are freed at runtime.  A bump allocator is correct for this usage
pattern.

### 3.2.2 OOM Handling

If the bump allocator runs out of space, it logs a message over serial and enters
an infinite `cli`/`hlt` loop.  This is the kernel's "safe state" for OOM.

IEC 61508 §7.4.5 requires that a safety-critical system must reach a defined safe
state when it cannot continue operating correctly.  For a kernel that has exhausted
its heap, the correct safe state is halt (after logging diagnostics for the
maintainer).  The hardware watchdog will then reset the system after its timeout.

### 3.2.3 Why Not a Free-List Allocator?

Adding free-list support would introduce complexity — coalescing logic, fragmentation
tracking, worst-case allocation time analysis.  Since the kernel heap is genuinely
not freed at runtime, this complexity has no benefit.  The ring-3 servers each have
their own independent heaps (via the libc malloc implementation in Chapter 22).

## 3.3 The Physical Frame Allocator

The physical frame allocator manages the pool of free physical pages.  In Rost,
"physical page" and "physical frame" are used interchangeably; each is 4 KB
(4096 bytes), aligned to a 4 KB boundary.

### 3.3.1 Initialization

During Stage 1, the kernel finds the largest contiguous `CONVENTIONAL_MEMORY`
region in the UEFI memory map and initializes the physical allocator:

```rust
pub fn init_global_allocator(base: u64, size: u64) {
    // base must be 4 KB-aligned; size rounded down to 4 KB boundary
    let base = (base + 0xFFF) & !0xFFF;
    let frames = size as usize / 4096;
    // Initialize the FREE_BITMAP: all frames start as free (bit = 1)
    // Initialize FRAME_TAGS: all frames start as Free (0)
}
```

The base address is typically around `0x1780000` on QEMU — above the kernel binary
itself, above the ACPI tables, and safely clear of the low conventional memory
region used by the BIOS.

### 3.3.2 The Frame Bitmap

```rust
/// 65,536 bits covering 256 MB at 4 KB/frame.
/// Bit i = 1 means frame i is free; 0 means allocated.
static mut FREE_BITMAP: [u64; 1024] = [0u64; 1024];
```

Each `u64` in `FREE_BITMAP` represents 64 consecutive frames.  To check if frame
`i` is free: `FREE_BITMAP[i / 64] & (1 << (i % 64)) != 0`.

Allocation uses a Count-Trailing-Zeros scan: iterate over `FREE_BITMAP` words
until a non-zero word is found, then use `u64::trailing_zeros()` to find the first
set bit in that word.  This is O(N/64) in the worst case but typically O(1) for
a system with many free frames.

```rust
fn bitmap_find_free() -> Option<usize> {
    unsafe {
        for (word_idx, &word) in FREE_BITMAP.iter().enumerate() {
            if word != 0 {
                let bit = word.trailing_zeros() as usize;
                return Some(word_idx * 64 + bit);
            }
        }
        None
    }
}
```

### 3.3.3 Allocation and Freeing

```rust
/// Allocate one 4 KB physical frame.
/// Returns its physical address or None if exhausted.
pub fn global_alloc_4k() -> Option<u64> {
    let frame_idx = bitmap_find_free()?;
    // Clear the bit (mark as allocated)
    unsafe { FREE_BITMAP[frame_idx / 64] &= !(1 << (frame_idx % 64)); }
    frame_tag(phys, FrameKind::KernelData);
    Some(BASE_ADDR.load(Ordering::Relaxed) + frame_idx as u64 * 4096)
}

/// Return a frame to the pool.
pub fn global_free_4k(phys: u64) {
    let frame_idx = /* compute from phys - BASE_ADDR */;
    unsafe { FREE_BITMAP[frame_idx / 64] |= 1 << (frame_idx % 64); }
    frame_tag(phys, FrameKind::Free);
}
```

The allocator silently ignores out-of-range addresses in `global_free_4k`.  This
is defensive programming: if a PCB was created without a tracked PML4 frame
(e.g., a ring-0 process sharing the kernel PML4), the free in its Drop impl
is a no-op rather than a corruption.

## 3.4 Frame Tags

```rust
pub enum FrameKind {
    Free       = 0,
    KernelData = 1,
    UserOwned  = 2,
    Guard      = 3,
    Mmio       = 4,
}

/// 65,536-byte BSS array.  One byte per 4 KB frame, covering 256 MB.
static mut FRAME_TAGS: [u8; 65536] = [0u8; 65536];
```

Every allocated frame carries a `FrameKind` tag.  Tags serve several purposes:

1. **Debugging** — `frame_stats()` returns counts of Free/KernelData/UserOwned/Guard/Mmio
   frames, which is printed at boot and is available to the shell's `mem` command.

2. **Safety** — when a frame is passed to the page table mapper, the caller can
   verify it is in the expected state.

3. **Reclaim tracking** — `global_free_4k` resets the tag to `Free` when a frame
   is returned.

The tags are maintained alongside the bitmap: every `global_alloc_4k()` call sets
the tag to `KernelData`; the ELF loader sets it to `UserOwned`; guard page setup
sets it to `Guard`.

## 3.5 Per-Type Object Pools

On the hot path (inside the timer ISR), the allocator must never be called.
If the timer ISR needed to allocate a page table frame for a context switch, the
allocation could fail or take an arbitrarily long time — violating the real-time
scheduling guarantee.

Rost solves this with pre-allocated pools.

### 3.5.1 Page Table Frame Pool

```rust
/// LIFO stack of pre-allocated page-table frames.
/// 512 × 4 KB = 2 MB of PT frames pre-allocated at boot.
static mut PT_POOL: [u64; 512] = [0u64; 512];
static mut PT_POOL_TOP: usize = 0;
```

At boot, `pool_init(n)` draws `n` frames from the global bitmap allocator and
fills the pool.  During operation:

- `pool_alloc_pt()` pops from the LIFO stack in O(1).  Falls back to
  `global_alloc_4k()` on pool miss (acceptable during boot; not expected at runtime).
- `pool_free_pt(phys)` pushes back in O(1).  Spills to `global_free_4k()` if
  the pool is full.

All `map_page_global()` calls use `pool_alloc_pt()` for intermediate page-table
nodes.  This guarantees O(1) physical frame acquisition on the hot path.

### 3.5.2 Kernel Stack Pool

Process Control Blocks (PCBs) are pre-allocated in a fixed-size array of 32 slots.
When a process terminates, its kernel stack slot is returned to a LIFO reclaim pool:

```rust
static mut STACK_RECLAIM: [u8; 32] = [0u8; 32];
static mut STACK_RECLAIM_TOP: usize = 0;
```

`alloc_kernel_stack()` checks this pool first; `free_kernel_stack()` pushes to it.
This ensures that after MAX_KERNEL_STACKS process lifetimes, slots are reused
rather than exhausted.

**Important safety detail**: the stack is zeroed in `alloc_kernel_stack()` (when
the slot is popped from the pool and handed to a new process), not in
`free_kernel_stack()`.  This is because `free_kernel_stack()` is called from the
PCB's `Drop` impl, which may fire while the CPU is still executing on that stack
(in the exception handler → `terminate_faulting_process` path).  Zeroing a live
stack corrupts all saved return addresses and causes a triple fault.

## 3.6 User Process Physical Frame Reclaim

When a ring-3 process terminates, its physical frames must be returned to the pool.
As of the current implementation, this is tracked via the PCB:

```rust
pub struct ProcessControlBlock {
    // ...
    /// Physical frames owned by this process (ELF segments, user stack, PML4).
    pub user_frames:      [u64; USER_FRAMES_MAX],  // USER_FRAMES_MAX = 96
    pub user_frame_count: usize,
    /// True if this process owns its PML4 frame.
    pub pml4_owned: bool,
    // ...
}
```

When the ELF loader (`spawn_elf`) creates a new process, it records every
`global_alloc_4k()` call in a local `frames[]` array and then passes it to
`sched.register_user_frames(pid, &frames[..frame_count])`.  This stores the
frame addresses in the PCB.

When the PCB is dropped (process terminates, `*slot = None`), the `Drop` impl
calls `global_free_4k` on every tracked frame and, if `pml4_owned`, also frees
the PML4 frame:

```rust
impl Drop for ProcessControlBlock {
    fn drop(&mut self) {
        free_kernel_stack(self.kernel_stack_id);
        #[cfg(not(test))]
        {
            for i in 0..self.user_frame_count {
                crate::memory::global_free_4k(self.user_frames[i]);
            }
            if self.pml4_owned && self.page_table_base != 0 {
                crate::memory::global_free_4k(self.page_table_base);
            }
        }
    }
}
```

**Note on intermediate page table frames**: the PDPT, PD, and PT frames allocated
by `map_page_global()` for intermediate page-table levels are not yet individually
tracked in the PCB.  They are correctly tagged as `KernelData` in the frame bitmap,
but are not freed when the process terminates.  This is a known minor resource leak
that will be addressed by a future PML4-walk reclaim pass.

## 3.7 Memory Layout at Boot

After Stage 1, the physical memory is laid out as follows:

```
0x0000_0000 – 0x0000_0FFF   Interrupt Vector Table (real-mode legacy)
0x0000_4000 – 0x0000_4FFF   Persistent crash log (64-byte header + 16×64B records)
0x0001_0000 – ~0x013_FFFF   BIOS / legacy area (not touched)
~0x01_4000_0000             UEFI runtime services (identity-mapped)
~0x01_78_0000               Physical frame pool base (largest free region)
   ...                      Kernel frame pool (bitmap-tracked, 256 MB window)
   ...                      Kernel binary image (identity-mapped .text/.rodata/.data/.bss)
   ...                      GOP framebuffer (MMIO, identity-mapped in Stage 4)
   ...                      LAPIC MMIO (4 KB, mapped in Stage 3)
   ...                      IOAPIC MMIO (4 KB, mapped in Stage 3)
   ...                      HPET MMIO (1 KB, mapped in Stage 4)
   ...                      IOMMU MMIO (per unit, mapped in Stage 4)
```

## 3.8 IEC 61508 Compliance

Physical memory management maps directly to IEC 61508 §7.4.5
("Software Resource Allocation and Deallocation"):

- **Fixed-size pools** — all critical allocations (page tables, kernel stacks, PCBs)
  use pre-allocated pools with known worst-case sizes.  No dynamic allocation on the
  scheduling hot path.
- **OOM safe state** — the bump allocator halts on exhaustion (defined safe state).
- **Resource reclaim** — every physical frame allocated to a terminated process is
  returned to the pool before the PCB slot is cleared.
- **Memory quotas** — per-process `memory_quota_pages` limits how many physical
  frames a ring-3 process can map.  Enforced at every `SYS_MAP` and `SYS_MAP_SHARE`
  call.

## 3.9 Summary

Rost's physical memory management uses a layered approach:

1. A 1 MB bump allocator for the kernel's own static data structures
2. A bitmap-based 4 KB frame allocator for page tables, stacks, and user segments
3. Pre-allocated LIFO pools (PT frames, kernel stack slots) for O(1) hot-path access
4. Per-PCB frame tracking for complete reclaim on process termination

This architecture provides deterministic allocation time on the scheduling hot path,
predictable memory consumption, and correct resource reclaim — all requirements for
a SIL-4 safety profile.
