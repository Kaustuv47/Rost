# Chapter 4 — Virtual Memory & Paging

## 4.1 Why Virtual Memory?

Virtual memory provides the fundamental mechanism by which the kernel enforces
spatial isolation.  Without it, any process could read or write any physical
address — there would be no boundary between the kernel and user space, and no
boundary between different user processes.

On x86-64, virtual memory is implemented via a four-level page table hierarchy.
The CPU's Memory Management Unit (MMU) walks the hierarchy on every memory access,
translating a 64-bit virtual address into a physical address.  If the walk fails
(the virtual address is not mapped), the CPU raises a #PF (page fault) exception.

## 4.2 The x86-64 Four-Level Page Table

x86-64 uses a 48-bit virtual address space (with 4-level paging).  The address
is split into five fields:

```
  Bits [63:48]  Sign extension (must equal bit 47 — "canonical" addresses)
  Bits [47:39]  PML4 index (9 bits → 512 entries)
  Bits [38:30]  PDPT index (9 bits → 512 entries)
  Bits [29:21]  PD   index (9 bits → 512 entries)
  Bits [20:12]  PT   index (9 bits → 512 entries)
  Bits [11: 0]  Page offset (12 bits → 4 KB page)
```

Each table (PML4, PDPT, PD, PT) is exactly 4 KB in size (512 × 8-byte entries).
Each entry contains a physical address and flags.

### 4.2.1 Page Table Entry Flags

```rust
pub const PTE_PRESENT:    u64 = 1 << 0;   // page is valid and mapped
pub const PTE_WRITABLE:   u64 = 1 << 1;   // allow writes
pub const PTE_USER:       u64 = 1 << 2;   // ring-3 access allowed
pub const PTE_HUGE_PAGE:  u64 = 1 << 7;   // PD-level: 2 MB page
pub const PTE_NO_EXECUTE: u64 = 1 << 63;  // XD/NX bit (requires EFER.NXE)
pub const PTE_ADDR_MASK:  u64 = 0x000F_FFFF_FFFF_F000; // physical address bits
```

The `PTE_NO_EXECUTE` bit (also called NX or XD) prevents code execution from a
page.  It is critical for W^X (Write XOR Execute) security: data pages must not
be executable, and code pages must not be writable.

## 4.3 The `PageTable` Type

All four levels of the page table hierarchy use the same structure:

```rust
/// 4 KB-aligned, 512-entry page table.
///
/// This type is used for all four levels (PML4, PDPT, PD, PT).
/// The #[repr(C, align(4096))] ensures the structure is exactly 4 KB
/// and starts at a 4 KB page boundary.
#[repr(C, align(4096))]
pub struct PageTable {
    pub entries: [u64; 512],
}

impl PageTable {
    pub const fn new() -> Self {
        PageTable { entries: [0u64; 512] }
    }
}
```

The alignment requirement is critical.  When the CPU reads a PTE at any level,
the physical address stored in the entry must point to the next-level table.
x86-64 requires all page-table structures to be 4 KB-aligned; the low-order 12
bits of the address in a PTE are repurposed as flag bits.  If a `PageTable` were
not 4 KB-aligned, the physical address extracted from a PTE by `& PTE_ADDR_MASK`
would be wrong.

## 4.4 Core Mapping Operations

### 4.4.1 `map_page()`

```rust
pub fn map_page(
    pml4: &mut PageTable,
    virt: u64,
    phys: u64,
    flags: u64,
    alloc: &mut impl FnMut() -> Option<u64>,
) -> bool
```

This function maps a single 4 KB virtual page to a physical frame.  It walks
the four levels of the page table hierarchy, allocating intermediate tables as
needed using the caller-supplied `alloc` closure.

The walk:
1. Extract PML4 index from `virt[47:39]`; look up or allocate a PDPT
2. Extract PDPT index from `virt[38:30]`; look up or allocate a PD
3. Extract PD index from `virt[29:21]`; look up or allocate a PT
4. Extract PT index from `virt[20:12]`; write the PTE: `phys | flags`

Each intermediate table entry is filled with `phys_of_next_level | PTE_PRESENT | PTE_WRITABLE`.
Note that intermediate entries do NOT get `PTE_USER`; that flag only appears in the
final leaf PTE.  This is important for SMAP enforcement (see §4.7).

### 4.4.2 `map_page_global()`

```rust
pub fn map_page_global(
    pml4: &mut PageTable,
    virt: u64,
    phys: u64,
    flags: u64,
) -> bool
```

A convenience wrapper that uses `pool_alloc_pt()` for intermediate table allocation.
This is the function used by the ELF loader and `SYS_MAP` — it draws from the
pre-allocated pool rather than requiring a closure.

### 4.4.3 `identity_map_region()`

Maps a physical region to the same virtual address using 2 MB huge pages where
possible:

```rust
pub fn identity_map_region(
    pml4: &mut PageTable,
    base: u64,
    size: u64,
    flags: u64,
    alloc: &mut impl FnMut() -> Option<u64>,
)
```

At Stage 1, the kernel identity-maps all UEFI memory regions.  This is necessary
because the CPU starts executing at the physical address where UEFI loaded the
kernel binary, and after activating the new PML4, virtual addresses must still
translate to the same physical addresses.  2 MB huge pages cover 2 MB per PDPT
entry instead of 4 KB per PT entry, so a 4 GB physical map requires only 2048
PD entries instead of 1,048,576 PT entries.

### 4.4.4 `translate_address()`

```rust
pub fn translate_address(pml4: &PageTable, virt: u64) -> Option<u64>
```

Walks the four-level hierarchy for a virtual address and returns the physical
address, or `None` if any level is not present.  This is used by `SYS_PHYS_ADDR`
(syscall 31) to let ring-3 drivers translate virtual DMA buffer addresses to
physical addresses for device programming.

### 4.4.5 `unmap_page()`

```rust
pub fn unmap_page(pml4: &mut PageTable, virt: u64) -> bool
```

Clears the leaf PTE for a virtual address.  Used to install guard pages on
kernel stacks (see §4.9).  Returns `false` if the page was not mapped or if
a huge page is present (splitting huge pages requires `split_huge_page_global()`
first).

### 4.4.6 `remap_page_flags()`

```rust
pub fn remap_page_flags(pml4: &mut PageTable, virt: u64, new_flags: u64) -> bool
```

Changes the flags on an existing 4 KB PTE without changing the physical address.
Used by `apply_section_flags()` to make kernel `.text` pages read-only and `.data`
pages non-executable after the page tables are live.

## 4.5 2 MB Huge Pages and Splitting

The kernel's initial identity map uses 2 MB huge pages for efficiency.  However,
when per-page attributes must differ within a 2 MB region (e.g., the kernel `.text`
section), the huge page must be split into 512 × 4 KB pages:

```rust
pub fn split_huge_page_global(pml4: &mut PageTable, virt: u64) -> bool
```

`split_huge_page_global()` finds the PD entry for `virt`, verifies it is a huge
page, allocates a new PT, populates all 512 entries with the appropriate physical
addresses and flags derived from the original huge-page entry, and replaces the
PD entry with a regular entry pointing to the new PT.

`apply_section_flags()` calls `split_huge_page_global()` for every 2 MB region
that spans the kernel `.text`, `.rodata`, `.data`, and `.bss` sections, then
calls `remap_page_flags()` on individual 4 KB pages within each section to
apply per-section protection.

## 4.6 The Kernel PML4

The kernel maintains a single top-level page table for all ring-0 code:

```rust
static mut KERNEL_PML4: PageTable = PageTable::new();
```

This is a `static` variable (not dynamically allocated) for two reasons:
1. It must exist before the physical frame allocator is available
2. Its physical address must be known at compile time for KERNEL_PML4_PHYS

After Stage 1, `KERNEL_PML4` contains identity mappings for all physical memory
regions described by UEFI, plus explicit mappings for LAPIC, IOAPIC, HPET, IOMMU,
and GOP MMIO regions.

The physical address of `KERNEL_PML4` is stored in `KERNEL_PML4_PHYS`
(`AtomicU64` in `core_kernel::scheduler`) so that the ELF loader and `SYS_MAP`
can access it without a crate-circular dependency.

## 4.7 Memory Protection Bits

After the kernel PML4 is installed, Stage 5 enables several CPU protection features
via `init_protection()` in `arch_x86_64::cpu`:

### 4.7.1 EFER.NXE (No-Execute Enable)

```asm
mov  ecx, 0xC000_0080   ; EFER MSR
rdmsr
or   eax, 0x800          ; set bit 11 (NXE)
wrmsr
```

Setting `EFER.NXE` activates the NX/XD bit in page table entries.  Without it,
the `PTE_NO_EXECUTE` bit is ignored and all mapped pages are executable.

### 4.7.2 CR0.WP (Write Protect)

Setting `CR0.WP` means that even ring-0 code cannot write to a page that does not
have `PTE_WRITABLE` set.  Without this bit, the kernel could silently write to
read-only pages — potentially corrupting its own code.

After Stage 5, the kernel's `.text` pages have only `PTE_PRESENT` (no WRITABLE);
any write from ring-0 to kernel code raises a #PF.

### 4.7.3 CR4.SMEP (Supervisor Mode Execution Prevention)

SMEP prevents the CPU in ring-0 from executing code at a page with `PTE_USER` set.
This closes a class of privilege escalation attack where an attacker tricks the
kernel into executing attacker-controlled user-space code.

### 4.7.4 CR4.SMAP (Supervisor Mode Access Prevention)

SMAP prevents ring-0 code from reading or writing user-space memory without
explicitly bracketing the access with `STAC`/`CLAC` instructions.  In Rost, the
syscall dispatcher uses a `SmapGuard` RAII type:

```rust
struct SmapGuard;

impl SmapGuard {
    fn new() -> Self {
        unsafe { core::arch::asm!("stac", options(nostack, nomem)); }
        SmapGuard
    }
}

impl Drop for SmapGuard {
    fn drop(&mut self) {
        unsafe { core::arch::asm!("clac", options(nostack, nomem)); }
    }
}
```

Every syscall handler that reads from a user-space pointer creates a `SmapGuard`
at the start.  The guard's `drop` is called automatically when the function
returns, even if it returns early.  This is a textbook use of Rust's RAII pattern
for maintaining CPU register invariants.

### 4.7.5 CR4.FSGSBASE

Setting `CR4.FSGSBASE` enables the `RDFSBASE`, `WRFSBASE`, `RDGSBASE`, `WRGSBASE`
instructions for ring-3 code.  These allow user-space to update its own FS and GS
base registers without a syscall, which is the standard mechanism for thread-local
storage in System V ABI programs.

## 4.8 Per-Process Address Spaces

Every ring-3 process has its own PML4.  This is the fundamental mechanism for
spatial isolation between user processes.

When the ELF loader spawns a process:
1. It allocates a fresh 4 KB frame and zeroes it (the new PML4)
2. It maps each ELF `PT_LOAD` segment at its requested virtual address in the new PML4
3. It maps a 128 KB user stack
4. It calls `merge_kernel_into_user_pml4()` to copy kernel page-table entries

### 4.8.1 `merge_kernel_into_user_pml4()`

When a syscall fires from ring-3, the CPU does NOT switch CR3 — the hardware just
changes privilege level.  This means the syscall handler executes with the user's
CR3, and must be able to access kernel code, kernel data structures, and kernel stacks.

`merge_kernel_into_user_pml4()` solves this by copying kernel PTE entries into
the user PML4:

- PML4 entries [1..512] (virtual > 512 GB — upper half of the address space)
- PDPT[0] entries [1..512] (virtual 1 GB – 512 GB)
- PD[0] entries [1..512] (virtual 2 MB – 1 GB)

PD[0] entry 0 (0–2 MB) is deliberately NOT copied — this is where user ELF
segments live.  The kernel's own code and data are at addresses above 2 MB where
the firmware loaded them, so there is no overlap.

Critically, kernel entries are copied WITHOUT `PTE_USER` — so SMAP prevents
ring-3 code from directly accessing kernel memory, even though the mappings exist
in the user PML4.

## 4.9 Guard Pages

Each kernel stack slot is 12 KB: a 4 KB guard page at the base, followed by 8 KB
of usable stack.  The guard page is explicitly unmapped by `install_kernel_stack_guard_pages()`:

```rust
fn install_kernel_stack_guard_pages() {
    for id in 0..NEXT_STACK.load(Ordering::Relaxed) {
        if let Some(guard_addr) = kernel_stack_guard_addr(id) {
            // Split the 2 MB huge page that covers this address, then unmap it
            split_huge_page_global(&mut KERNEL_PML4, guard_addr & !0x1FFFFF);
            unmap_page(&mut KERNEL_PML4, guard_addr);
        }
    }
}
```

When a kernel stack overflows into the guard page, the MMU raises a #PF instead
of silently corrupting the adjacent stack slot.  This is critical for debugging
stack overflow bugs, which are otherwise extremely difficult to diagnose.

## 4.10 Section Protection

After splitting huge pages, `remap_kernel_sections()` applies per-section
protection to the kernel binary:

| Section | Flags | Rationale |
|---------|-------|-----------|
| `.text` | `PRESENT` only | Code: readable + executable, NOT writable |
| `.rodata` | `PRESENT \| NX` | Read-only data: readable, NOT writable, NOT executable |
| `.data` | `PRESENT \| WRITABLE \| NX` | Mutable data: read-write, NOT executable |
| `.bss` | `PRESENT \| WRITABLE \| NX` | Zero-initialized data: same as .data |

A PE32+ section table parser (`pecoff.rs`) reads the section headers from the
kernel image's in-memory PE32+ header.  The section virtual addresses and sizes
are used to determine which 4 KB pages to remap.

IEC 61508 §7.4.3: kernel code segments must be read-only.  A write to `.text`
from ring-0 will now raise a #PF (CR0.WP is set), giving the crash log a chance
to record the fault before halting.

## 4.11 TLB Management

When a page table entry is changed (page mapped, unmapped, or remapped), the
CPU's Translation Lookaside Buffer (TLB) must be invalidated for that virtual
address:

```rust
#[inline(always)]
pub fn invlpg(virt: u64) {
    unsafe { core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack)); }
}
```

Rost calls `invlpg()` after every `unmap_page()` and `remap_page_flags()` call
while the affected PML4 is in CR3.  For the initial setup (before `activate_page_table()`),
no TLB invalidation is needed because the new PML4 is not yet active.

For SMP systems, TLB shootdown (sending an IPI to all other CPUs to invalidate
their TLBs) would be required.  Since Rost enforces single-core operation, a
local `invlpg` is sufficient.

## 4.12 Summary

Rost's virtual memory subsystem provides:

- **Spatial isolation** — each ring-3 process has its own PML4; no process can
  access another's memory
- **W^X enforcement** — data pages have NX set; code pages have no WRITABLE
- **SMAP protection** — ring-0 cannot accidentally dereference user pointers without
  explicit STAC/CLAC
- **SMEP protection** — ring-0 cannot execute user-space code
- **Stack overflow detection** — guard pages below each kernel stack
- **Section-level protection** — kernel .text is read-only; .rodata is non-executable

Together these protections ensure that a bug in one process (or even in the kernel)
is caught at the memory level rather than propagating silently.
