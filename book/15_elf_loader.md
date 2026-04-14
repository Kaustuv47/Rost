# Chapter 15 — The ELF Loader

## 15.1 Overview

The ELF loader (`crates/kernel/src/elf.rs`) is the bridge between a blob of bytes
and a running ring-3 process.  It performs four tasks:

1. **Parse** the ELF64 header and program header table
2. **Allocate** physical frames for each loadable segment and the user-mode stack
3. **Map** those frames into a fresh PML4 address space
4. **Register** the process with the scheduler and hand the frame list to the PCB

All server ELFs are embedded in the kernel binary at compile time via
`include_bytes!`, so the loader never needs to read from disk.

## 15.2 ELF64 Format

An ELF64 binary consists of:

```
┌─────────────────────────────────┐
│    ELF Header (64 bytes)        │  e_ident, e_type, e_machine, e_entry,
│                                 │  e_phoff, e_phnum, e_phentsize
├─────────────────────────────────┤
│  Program Header Table           │  e_phnum × 56-byte Elf64Phdr entries
│  (at offset e_phoff)            │  Each entry describes one segment
├─────────────────────────────────┤
│  Segment data (PT_LOAD × N)     │  Code, data, rodata, BSS
└─────────────────────────────────┘
```

Rost's servers are compiled as static executables (`-C relocation-model=static`,
`ET_EXEC`) targeting `x86_64-unknown-none`.  This guarantees:
- No dynamic linker needed
- Fixed virtual addresses for all segments
- All segments fit below 2 MB (PD[0] in the virtual address space)

### 15.2.1 Supported Subset

```rust
const ELFCLASS64:  u8  = 2;       // 64-bit
const ELFDATA2LSB: u8  = 1;       // little-endian
const EM_X86_64:   u16 = 0x3E;    // x86-64 machine
const PT_LOAD:     u32 = 1;       // loadable segment type
```

Any ELF that deviates from these constants — wrong class, big-endian, wrong
architecture, or zero program headers — is rejected before any frame allocation.

## 15.3 The `Elf64Ehdr` and `Elf64Phdr` Structures

```rust
#[repr(C)]
struct Elf64Ehdr {
    e_ident:     [u8; 16],   // magic + class + data encoding
    e_type:      u16,
    e_machine:   u16,
    e_version:   u32,
    e_entry:     u64,        // virtual entry point
    e_phoff:     u64,        // offset of program header table
    e_shoff:     u64,
    e_flags:     u32,
    e_ehsize:    u16,
    e_phentsize: u16,        // size of one program header (must be 56)
    e_phnum:     u16,        // number of program headers
    e_shentsize: u16,
    e_shnum:     u16,
    e_shstrndx:  u16,
}

#[repr(C)]
struct Elf64Phdr {
    p_type:   u32,           // PT_LOAD (1) is the only type we act on
    p_flags:  u32,           // PF_EXEC (1), PF_WRITE (2), PF_READ (4)
    p_offset: u64,           // byte offset of segment data in file
    p_vaddr:  u64,           // target virtual address
    p_paddr:  u64,           // physical address (ignored)
    p_filesz: u64,           // bytes in file image (may be < p_memsz for BSS)
    p_memsz:  u64,           // bytes to allocate in memory
    p_align:  u64,
}
```

**`read_unaligned` is mandatory**: `include_bytes!` returns a `&[u8]` with only
1-byte alignment.  Casting directly to `*const Elf64Ehdr` and dereferencing
would be undefined behaviour in Rust's debug builds (which check alignment).
The loader uses `core::ptr::read_unaligned` throughout.

## 15.4 The `load()` Function

```rust
pub fn load(data: &[u8], pml4: Option<&mut PageTable>) -> Option<LoadedElf>
```

`load()` is the core function.  It returns a `LoadedElf` on success or `None`
on any error.

### 15.4.1 Step 1: Header Validation

```rust
if data.len() < core::mem::size_of::<Elf64Ehdr>() { return None; }

let ehdr = unsafe { core::ptr::read_unaligned(data.as_ptr() as *const Elf64Ehdr) };

// Magic bytes
if ehdr.e_ident[0] != 0x7f || &ehdr.e_ident[1..4] != b"ELF" { return None; }

// Class, endianness, machine
if ehdr.e_ident[4] != ELFCLASS64  { return None; }
if ehdr.e_ident[5] != ELFDATA2LSB { return None; }
if ehdr.e_machine  != EM_X86_64   { return None; }

// Program header size must be exactly 56 bytes
if ehdr.e_phentsize != core::mem::size_of::<Elf64Phdr>() as u16 { return None; }
if ehdr.e_phnum     == 0 { return None; }
```

### 15.4.2 Step 2: PML4 Allocation

If the caller passes `None`, a new PML4 frame is allocated:

```rust
let (pml4_ref, pml4_phys) = match pml4 {
    Some(t) => {
        let phys = t as *mut PageTable as u64;
        (t, phys)
    }
    None => {
        let phys = global_alloc_4k()?;
        unsafe { core::ptr::write_bytes(phys as *mut u8, 0, 4096); }
        track!(phys);   // record for reclaim
        let r = unsafe { &mut *(phys as *mut PageTable) };
        (r, phys)
    }
};
```

The `track!` macro appends `phys` to a local frame array:

```rust
macro_rules! track {
    ($phys:expr) => {{
        if frame_count < LOAD_FRAMES_MAX {
            frames[frame_count] = $phys;
            frame_count += 1;
        }
    }};
}
```

`LOAD_FRAMES_MAX = 64` accommodates typical server ELFs which have 3–5 PT_LOAD
segments, each needing 1–3 pages, plus the PML4 itself.

### 15.4.3 Step 3: Segment Mapping

For each `PT_LOAD` segment:

```rust
// Permission flags from ELF segment flags
let mut flags = PTE_PRESENT | PTE_USER;
if phdr.p_flags & PF_WRITE != 0 { flags |= PTE_WRITABLE; }
if phdr.p_flags & PF_EXEC  == 0 { flags |= PTE_NO_EXECUTE; }
```

The W^X policy is enforced naturally: executable segments (code) have `PF_EXEC`
set and `PF_WRITE` clear, so the PTE gets `PTE_PRESENT | PTE_USER` (no NX, no
WRITABLE).  Data segments have `PF_WRITE` set and `PF_EXEC` clear, so they get
`PTE_WRITABLE | PTE_NO_EXECUTE`.

**Non-page-aligned `p_vaddr`**: Some linker scripts produce segments that start
at addresses like `0x1890` (not a multiple of 4096).  The loader handles this:

```rust
let page_align_offset = (virt_base & 0xFFF) as usize;
let virt_page_base    = virt_base & !0xFFF;
let total_pages       = (page_align_offset + mem_size + 4095) / 4096;

for page_idx in 0..total_pages {
    let phys_page = global_alloc_4k()?;
    frame_tag(phys_page, FrameKind::UserOwned);
    track!(phys_page);
    unsafe { core::ptr::write_bytes(phys_page as *mut u8, 0, 4096); }

    // Copy the right slice of seg_data into the right offset in phys_page.
    let (seg_start, phys_offset) = if page_idx == 0 {
        (0usize, page_align_offset)     // first page: data at phys_page[page_align_offset]
    } else {
        (page_idx * 4096 - page_align_offset, 0usize)  // later pages: data at phys_page[0]
    };
    // ... copy_nonoverlapping ...

    let virt_page = virt_page_base + (page_idx * 4096) as u64;
    map_page_global(pml4_ref, virt_page, phys_page, flags);
}
```

Zero-filling before copying handles `p_memsz > p_filesz` (the BSS region):
the physical page is zeroed, then `p_filesz` bytes are copied — the remaining
bytes in the page stay zero.

### 15.4.4 Return Value

```rust
pub struct LoadedElf {
    pub entry:       u64,                      // virtual entry point
    pub pml4_phys:   u64,                      // physical PML4 for this process
    pub frames:      [u64; LOAD_FRAMES_MAX],   // all allocated frames
    pub frame_count: usize,
}
```

`frames` and `frame_count` are passed to `Scheduler::register_user_frames` after
the process is created, so the PCB's `Drop` impl can free them on termination.

## 15.5 The `spawn_elf()` Function

```rust
pub fn spawn_elf(data: &[u8], priority: u8)
    -> Option<core_kernel::process::ProcessId>
```

`spawn_elf` is the high-level entry point.  It:

1. Calls `load(data, None)` to parse the ELF and allocate/map segments
2. Allocates a 128 KB user-mode stack (32 × 4 KB pages)
3. Calls `merge_kernel_into_user_pml4` to make kernel code visible from the user PML4
4. Calls `add_ring3_process` on the global scheduler
5. Registers all physical frames with the PCB via `register_user_frames`

### 15.5.1 User-Mode Stack Layout

```rust
const USER_STACK_PAGES: usize = 32;  // 128 KB

let stack_top_virt:  u64 = 0x0000_7FFF_FFFF_F000;
let stack_size            = USER_STACK_PAGES * 4096;
let stack_base_virt       = stack_top_virt - stack_size as u64;
```

The stack top `0x0000_7FFF_FFFF_F000` is near the top of canonical user-space
virtual memory.  It is below the kernel higher-half and below SMAP's effective
boundary.  RSP is initialized to `stack_top_virt`, so the first push goes to
address `0x0000_7FFF_FFFF_EFF8` (RSP - 8).

Stack pages are tracked in `loaded.frames` and registered with the PCB so they
are freed when the process terminates.

### 15.5.2 Ring-3 Entry via IRETQ Trampoline

```rust
let trampoline = arch_x86_64::context::ring3_entry_trampoline as *const () as u64;
let pid = sched.add_ring3_process(loaded.entry, stack_top_virt,
                                   loaded.pml4_phys, trampoline)?;
```

The trampoline address is stored as the initial kernel-stack return address.
When the scheduler first switches to the new process, `switch_context` executes
`ret`, which pops the trampoline address and jumps there.

The trampoline then constructs a five-word IRETQ frame and executes `iretq` to
atomically enter ring-3:

```asm
ring3_entry_trampoline:
    ; r12 = user RIP,  r13 = user RSP  (placed there by add_ring3_process)
    push 0x1B          ; SS  = ring-3 data selector (0x18 | CPL=3)
    push r13           ; RSP = user stack top
    pushfq             ; RFLAGS
    or   qword ptr [rsp], 0x200   ; set IF=1 (enable interrupts in ring-3)
    push 0x23          ; CS  = ring-3 code selector (0x20 | CPL=3)
    push r12           ; RIP = user entry point
    iretq
```

After `iretq`:
- CPL changes from 0 to 3
- CR3 remains unchanged (the user PML4 was set during `switch_context`)
- The user process begins executing at its `_start` symbol

## 15.6 Merging Kernel Page Tables

A process's own PML4 maps only its ELF segments and stack.  When the process
makes a syscall via `SYSCALL`, the hardware does **not** switch CR3 — the CPU
remains in the user's address space.  The syscall handler needs to access kernel
code, data, and stacks.  Without kernel entries in the user PML4, the first
kernel memory access would triple-fault.

`merge_kernel_into_user_pml4` copies kernel page-table entries into the user
PML4 for ranges the user does not occupy:

```
Virtual Space Layout:
  0x0000_0000_0000_0000 – 0x0000_0000_0020_0000   User ELF (PD[0], 0–2 MB)
  0x0000_0000_0020_0000 – 0x0000_0000_1600_0000   Kernel code/data (PD[1..11])
                                                   ← copied from kernel PML4
  0x0000_7FFF_FF00_0000 – 0x0000_7FFF_FFFF_F000   User stack
```

The merge is performed at three levels:

| Level | Range copied | Why |
|-------|-------------|-----|
| PML4[1..512] | Virtual > 512 GB | Kernel higher-half or future use |
| PDPT[1..512] inside PML4[0] | Virtual 1 GB–512 GB | Kernel above 1 GB |
| PD[1..512] inside PDPT[0] | Virtual 2 MB–1 GB | Kernel code at ~22 MB |

PD[0] (0–2 MB) is deliberately skipped — the user ELF lives there, and those
entries are already present in the user PML4.

Kernel PTEs are copied **without** `PTE_USER`.  Ring-3 code cannot access them
(hardware enforces user bit), and SMAP prevents the kernel from accidentally
dereferencing user pointers.

## 15.7 Frame Tracking and Reclaim

Every physical frame allocated during an ELF load is recorded:

```
Source          Tracked by        Stored in
──────────────  ────────────────  ─────────────────────
PML4 frame      track!(phys)      LoadedElf::frames[0]
ELF seg pages   track!(phys_page) LoadedElf::frames[1..N]
User stack      manual push       LoadedElf::frames[N+1..]
```

After `add_ring3_process` returns the new PID:

```rust
sched.register_user_frames(pid, &loaded.frames[..loaded.frame_count]);
```

This call iterates the frame list and calls `pcb.add_user_frame(frame)` +
sets `pcb.pml4_owned = true` on the new PCB.

When the process terminates (exception, `SYS_EXIT`, or `terminate_process`), the
PCB's `Drop` impl fires:

```rust
impl Drop for ProcessControlBlock {
    fn drop(&mut self) {
        free_kernel_stack(self.kernel_stack_id);  // return kernel stack to pool
        #[cfg(not(test))]
        {
            for i in 0..self.user_frame_count {
                crate::memory::global_free_4k(self.user_frames[i]); // return all user frames
            }
            if self.pml4_owned && self.page_table_base != 0 {
                crate::memory::global_free_4k(self.page_table_base); // return PML4 frame
            }
        }
    }
}
```

This ensures no physical frame leaks when a server crashes or exits.

## 15.8 Runtime ELF Loading via `SYS_SPAWN_ELF`

In addition to boot-time spawning, the shell can execute arbitrary ELF binaries
at runtime via `SYS_SPAWN_ELF` (syscall 26):

```
Shell: exec /bin/hello-c
  → read ELF from VFS into EXEC_BUF (512 KB static buffer)
  → SYS_SPAWN_ELF(buf_ptr, buf_len, priority=128)
  → kernel calls spawn_elf(data, priority)
  → new process starts running
```

The kernel validates the user pointer with `validate_user_ptr(buf_ptr, len, 1)`
before reading, then calls `spawn_elf` with the buffer slice.

## 15.9 Summary

The ELF loader provides:

- **ELF64 parsing** — magic/class/endian/machine validation, `read_unaligned` for alignment safety
- **Segment mapping** — W^X permission mapping, non-aligned `p_vaddr` support, BSS zero-fill
- **Stack allocation** — 128 KB at `0x7FFF_FFFF_F000`, mapped with `NX | WRITABLE | USER`
- **Kernel merge** — `merge_kernel_into_user_pml4` makes syscall handler accessible
- **IRETQ trampoline** — `ring3_entry_trampoline` transitions CPL 0 → 3 on first schedule
- **Frame tracking** — all allocated frames registered with PCB for leak-free termination
- **Runtime loading** — `SYS_SPAWN_ELF` enables shell `exec` of user-provided ELF binaries
