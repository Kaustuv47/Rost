//! ELF64 loader for ring-3 server binaries.
//!
//! Parses a statically-linked ELF64 image and maps all `PT_LOAD` segments into
//! a new or existing PML4 address space.  Intermediate page-table levels are
//! allocated from the global physical bump allocator.
//!
//! # Supported format
//! - 64-bit ELF (e_ident[EI_CLASS] = 2)
//! - Little-endian (e_ident[EI_DATA] = 1)
//! - x86-64 machine (e_machine = 0x3E)
//! - Static executable or shared object
//!
//! # Usage
//! ```rust
//! if let Some(loaded) = elf::load(data, pml4) {
//!     let pid = SCHEDULER.add_process(loaded.entry, 0, loaded.pml4_phys);
//! }
//! ```

use core_kernel::memory::{
    PageTable, map_page_global, global_alloc_4k,
    PTE_PRESENT, PTE_WRITABLE, PTE_USER, PTE_NO_EXECUTE, PTE_ADDR_MASK,
    frame_tag, FrameKind,
};

// ── ELF constants ─────────────────────────────────────────────────────────────

#[allow(dead_code)]
const ELFMAG:     [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8      = 2;
const ELFDATA2LSB: u8     = 1;  // little-endian
const EM_X86_64:  u16     = 0x3E;

const PT_LOAD:    u32     = 1;   // loadable segment

// Segment flags (p_flags)
const PF_EXEC:  u32 = 0x1;
const PF_WRITE: u32 = 0x2;
#[allow(dead_code)]
const PF_READ:  u32 = 0x4;

// ── ELF structures ────────────────────────────────────────────────────────────

/// ELF64 file header (64 bytes).
#[repr(C)]
struct Elf64Ehdr {
    e_ident:     [u8; 16],
    e_type:      u16,
    e_machine:   u16,
    e_version:   u32,
    e_entry:     u64,   // virtual entry point
    e_phoff:     u64,   // program header table offset
    e_shoff:     u64,
    e_flags:     u32,
    e_ehsize:    u16,
    e_phentsize: u16,   // size of one program header
    e_phnum:     u16,   // number of program headers
    e_shentsize: u16,
    e_shnum:     u16,
    e_shstrndx:  u16,
}

/// ELF64 program header (56 bytes).
#[repr(C)]
struct Elf64Phdr {
    p_type:   u32,
    p_flags:  u32,
    p_offset: u64,   // offset in file
    p_vaddr:  u64,   // virtual address in memory
    p_paddr:  u64,   // physical address (ignored)
    p_filesz: u64,   // bytes in file image
    p_memsz:  u64,   // bytes in memory (≥ p_filesz; zero-fill the gap)
    p_align:  u64,
}

// ── Public result ─────────────────────────────────────────────────────────────

/// Maximum physical frames tracked per ELF load (segments + PML4).
/// Separate from the per-PCB cap — this is just the loader's local collection.
const LOAD_FRAMES_MAX: usize = 64;

/// Result of a successful ELF load.
pub struct LoadedElf {
    /// Virtual entry point address.
    pub entry: u64,
    /// Physical address of the PML4 used for this process.
    pub pml4_phys: u64,
    /// Physical frames allocated for this process (segment pages + PML4).
    /// Passed to `Scheduler::register_user_frames` after the process is created.
    pub frames: [u64; LOAD_FRAMES_MAX],
    pub frame_count: usize,
}

// ── Loader ────────────────────────────────────────────────────────────────────

/// Load an ELF64 binary from `data` into `pml4`.
///
/// - If `pml4` is `None`, a new PML4 is allocated from the global page allocator.
///   The kernel image is **not** automatically mapped into the new address space;
///   the caller must arrange identity-mapping or higher-half mapping separately.
/// - All `PT_LOAD` segments are mapped at their requested virtual addresses.
/// - Pages are allocated from the global bump allocator.
///
/// Returns `None` on any parse error or allocation failure.
pub fn load(data: &[u8], pml4: Option<&mut PageTable>) -> Option<LoadedElf> {
    hal::uart::print_str("      [ELF] load enter len=");
    hal::uart::print_hex(data.len() as u64);
    hal::uart::print_str("\n");

    // ── 1. Parse and validate ELF header ──────────────────────────────────────

    if data.len() < core::mem::size_of::<Elf64Ehdr>() {
        hal::uart::print_str("      [ELF] fail: too small\n"); return None;
    }
    // Use read_unaligned: include_bytes! gives 1-byte aligned data but
    // Elf64Ehdr requires 8-byte alignment; Rust debug builds check this.
    let ehdr: Elf64Ehdr = unsafe { core::ptr::read_unaligned(data.as_ptr() as *const Elf64Ehdr) };
    let ehdr = &ehdr;
    hal::uart::print_str("      [ELF] ehdr read ok\n");

    if ehdr.e_ident[0] != 0x7f || &ehdr.e_ident[1..4] != b"ELF" {
        hal::uart::print_str("      [ELF] fail: magic\n"); return None;
    }
    if ehdr.e_ident[4]   != ELFCLASS64      {
        hal::uart::print_str("      [ELF] fail: class\n"); return None;
    }
    if ehdr.e_ident[5]   != ELFDATA2LSB     {
        hal::uart::print_str("      [ELF] fail: endian\n"); return None;
    }
    if ehdr.e_machine    != EM_X86_64       {
        hal::uart::print_str("      [ELF] fail: machine\n"); return None;
    }
    if ehdr.e_phentsize  != core::mem::size_of::<Elf64Phdr>() as u16 {
        hal::uart::print_str("      [ELF] fail: phentsize\n"); return None;
    }
    if ehdr.e_phnum      == 0               {
        hal::uart::print_str("      [ELF] fail: phnum=0\n"); return None;
    }
    hal::uart::print_str("      [ELF] header ok phnum=");
    hal::uart::print_hex(ehdr.e_phnum as u64);
    hal::uart::print_str(" entry=");
    hal::uart::print_hex(ehdr.e_entry);
    hal::uart::print_str("\n");

    // ── 2. Obtain or create the target PML4 ───────────────────────────────────

    let mut frames       = [0u64; LOAD_FRAMES_MAX];
    let mut frame_count  = 0usize;

    // Helper: record a frame for later reclaim (silently drops extras).
    macro_rules! track {
        ($phys:expr) => {{
            if frame_count < LOAD_FRAMES_MAX {
                frames[frame_count] = $phys;
                frame_count += 1;
            }
        }};
    }

    let (pml4_ref, pml4_phys) = match pml4 {
        Some(t) => {
            let phys = t as *mut PageTable as u64;
            (t, phys)
        }
        None => {
            let phys = global_alloc_4k()?;
            unsafe { core::ptr::write_bytes(phys as *mut u8, 0, 4096); }
            // Track the PML4 frame so spawn_elf can pass it to register_user_frames.
            track!(phys);
            let r = unsafe { &mut *(phys as *mut PageTable) };
            (r, phys)
        }
    };

    // ── 3. Map each PT_LOAD segment ───────────────────────────────────────────

    let ph_off = ehdr.e_phoff as usize;
    let ph_cnt = ehdr.e_phnum as usize;

    for i in 0..ph_cnt {
        let off = ph_off + i * core::mem::size_of::<Elf64Phdr>();
        if off + core::mem::size_of::<Elf64Phdr>() > data.len() { return None; }

        let phdr: Elf64Phdr = unsafe { core::ptr::read_unaligned(data[off..].as_ptr() as *const Elf64Phdr) };
        let phdr = &phdr;
        if phdr.p_type != PT_LOAD { continue; }

        let file_end = (phdr.p_offset as usize).checked_add(phdr.p_filesz as usize)?;
        if file_end > data.len() { return None; }
        let seg_data = &data[phdr.p_offset as usize..file_end];

        let virt_base = phdr.p_vaddr;
        let mem_size  = phdr.p_memsz as usize;
        if mem_size == 0 { continue; }

        // Build PTE flags from segment permissions.
        let mut flags = PTE_PRESENT | PTE_USER;
        if phdr.p_flags & PF_WRITE != 0 { flags |= PTE_WRITABLE; }
        if phdr.p_flags & PF_EXEC  == 0 { flags |= PTE_NO_EXECUTE; }

        // Handle non-page-aligned p_vaddr (common in PIE/ET_DYN binaries).
        // The segment may start at e.g. vaddr=0x1890, which belongs to the
        // physical page mapped at virtual 0x1000.  We must copy seg_data[0]
        // to phys_page[0x890], not phys_page[0].
        let page_align_offset = (virt_base & 0xFFF) as usize;
        let virt_page_base    = virt_base & !0xFFF; // page-aligned start
        let total_pages       = (page_align_offset + mem_size + 4095) / 4096;

        for page_idx in 0..total_pages {
            let phys_page = global_alloc_4k()?;
            frame_tag(phys_page, FrameKind::UserOwned);
            // Track every segment page for reclaim on process termination.
            track!(phys_page);
            unsafe { core::ptr::write_bytes(phys_page as *mut u8, 0, 4096); }

            // Offset within seg_data that corresponds to the start of this page.
            // First page: data starts at seg_data[0], placed at phys_page[page_align_offset].
            // Later pages: data starts at seg_data[page_idx*4096 - page_align_offset], at phys_page[0].
            let (seg_start, phys_offset) = if page_idx == 0 {
                (0usize, page_align_offset)
            } else {
                (page_idx * 4096 - page_align_offset, 0usize)
            };

            if seg_start < seg_data.len() {
                let n = core::cmp::min(
                    seg_data.len() - seg_start,
                    4096 - phys_offset,
                );
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        seg_data[seg_start..].as_ptr(),
                        (phys_page as *mut u8).add(phys_offset),
                        n,
                    );
                }
            }

            let virt_page = virt_page_base + (page_idx * 4096) as u64;
            if !map_page_global(pml4_ref, virt_page, phys_page, flags) {
                return None;
            }
        }
    }

    Some(LoadedElf { entry: ehdr.e_entry, pml4_phys, frames, frame_count })
}

// ── Convenience: spawn an ELF as a new ring-3 process ────────────────────────

/// Number of 4 KB pages to allocate for a process's user-mode stack.
const USER_STACK_PAGES: usize = 32; // 128 KB — Shell struct ~20 KB + deep call frames

/// Copy kernel page-table entries into a user-process PML4.
///
/// When SYSCALL fires from ring-3, CR3 stays as the user's PML4.  Kernel
/// code, data, and stacks are not mapped there by default, causing a page
/// fault on the first kernel memory access in the syscall handler.
///
/// This function copies kernel intermediate-table entries into the user PML4
/// for entries the user does not already own:
///   * PML4 entries [1..512]    — virtual >512 GB (likely empty, defensive)
///   * PDPT[0] entries [1..512] — virtual 1 GB–512 GB inside PML4[0]
///   * PD[0]   entries [1..512] — virtual 2 MB–1 GB inside PDPT[0]
///
/// PD[0] (0–2 MB) is skipped here; the crash-log page at physical 0x4000
/// is mapped via a dedicated `map_page_global` call in `spawn_elf`.
///
/// Kernel entries are copied *without* `PTE_USER`, so ring-3 code cannot
/// reach them and SMAP enforces the boundary.
unsafe fn merge_kernel_into_user_pml4(user_pml4_phys: u64) {
    let kernel_pml4_phys = core_kernel::scheduler::KERNEL_PML4_PHYS
        .load(core::sync::atomic::Ordering::Relaxed);
    if kernel_pml4_phys == 0 {
        hal::uart::print_str("      [ELF] merge: no kernel PML4 recorded\n");
        return;
    }

    let kern_pml4 = &*(kernel_pml4_phys as *const PageTable);
    let user_pml4 = &mut *(user_pml4_phys as *mut PageTable);

    // ── Step 1: PML4 entries [1..512] (virtual >512 GB) ─────────────────────
    for i in 1..512usize {
        if kern_pml4.entries[i] & PTE_PRESENT != 0 && user_pml4.entries[i] == 0 {
            user_pml4.entries[i] = kern_pml4.entries[i];
        }
    }

    // ── Step 2: PDPT entries [1..512] inside PML4[0] ────────────────────────
    if kern_pml4.entries[0] & PTE_PRESENT == 0 { return; }
    let kern_pdpt = &*(( kern_pml4.entries[0] & PTE_ADDR_MASK) as *const PageTable);

    if user_pml4.entries[0] & PTE_PRESENT == 0 {
        let p = match core_kernel::memory::global_alloc_4k() { Some(a) => a, None => return };
        core::ptr::write_bytes(p as *mut u8, 0, 4096);
        user_pml4.entries[0] = p | PTE_PRESENT | PTE_WRITABLE | PTE_USER;
    }
    let user_pdpt = &mut *((user_pml4.entries[0] & PTE_ADDR_MASK) as *mut PageTable);

    for i in 1..512usize {
        if kern_pdpt.entries[i] & PTE_PRESENT != 0 && user_pdpt.entries[i] == 0 {
            user_pdpt.entries[i] = kern_pdpt.entries[i];
        }
    }

    // ── Step 3: PD entries [1..512] inside PDPT[0] ──────────────────────────
    if kern_pdpt.entries[0] & PTE_PRESENT == 0 { return; }
    let kern_pd = &*(( kern_pdpt.entries[0] & PTE_ADDR_MASK) as *const PageTable);

    if user_pdpt.entries[0] & PTE_PRESENT == 0 {
        let p = match core_kernel::memory::global_alloc_4k() { Some(a) => a, None => return };
        core::ptr::write_bytes(p as *mut u8, 0, 4096);
        user_pdpt.entries[0] = p | PTE_PRESENT | PTE_WRITABLE | PTE_USER;
    }
    let user_pd = &mut *((user_pdpt.entries[0] & PTE_ADDR_MASK) as *mut PageTable);

    // Skip PD[0] (0–2 MB) — user ELF segments live there.
    // The crash-log page at 0x4000 is mapped explicitly below.
    for i in 1..512usize {
        if kern_pd.entries[i] & PTE_PRESENT != 0 && user_pd.entries[i] == 0 {
            user_pd.entries[i] = kern_pd.entries[i];
        }
    }
    hal::uart::print_str("      [ELF] merge_kernel_into_user_pml4 done\n");
}

/// Load `data` and register the resulting ring-3 process with the global scheduler.
///
/// Allocates and maps a user-mode stack into the new process's address space.
/// The stack is placed at a conventional top address (0x0000_7FFF_FFFF_F000
/// downward) and the initial RSP is set to the top of this region.
///
/// Uses the IRETQ trampoline (`arch_x86_64::context::ring3_entry_trampoline`)
/// so the process starts in ring-3 (CPL=3) when first scheduled.
///
/// `priority` 0 means "use default (128)".  Returns the new PID, or `None` on
/// any failure.
pub fn spawn_elf(data: &[u8], priority: u8) -> Option<core_kernel::process::ProcessId> {
    hal::uart::print_str("      [DBG] elf size=");
    hal::uart::print_hex(data.len() as u64);
    hal::uart::print_str("\n");
    let mut loaded = load(data, None)?;
    hal::uart::print_str("      [DBG] load ok, entry=");
    hal::uart::print_hex(loaded.entry);
    hal::uart::print_str("\n");
    let sched  = core_kernel::scheduler::get_global()?;

    // Allocate and map a user-mode stack.
    let stack_top_virt: u64 = 0x0000_7FFF_FFFF_F000;
    let stack_size = USER_STACK_PAGES * 4096;
    let stack_base_virt = stack_top_virt - stack_size as u64;

    let stack_flags = core_kernel::memory::PTE_PRESENT
        | core_kernel::memory::PTE_WRITABLE
        | core_kernel::memory::PTE_USER
        | core_kernel::memory::PTE_NO_EXECUTE;

    let pml4 = unsafe { &mut *(loaded.pml4_phys as *mut core_kernel::memory::PageTable) };

    for page in 0..USER_STACK_PAGES {
        let phys = core_kernel::memory::global_alloc_4k()?;
        unsafe { core::ptr::write_bytes(phys as *mut u8, 0, 4096); }
        // Track stack frames for reclaim on process termination.
        if loaded.frame_count < LOAD_FRAMES_MAX {
            loaded.frames[loaded.frame_count] = phys;
            loaded.frame_count += 1;
        }
        let virt = stack_base_virt + (page * 4096) as u64;
        if !core_kernel::memory::map_page_global(pml4, virt, phys, stack_flags) {
            return None;
        }
    }

    // Merge kernel page tables into user PML4 so that the syscall handler
    // can access kernel code/data/stacks when CR3 is the user's PML4.
    unsafe { merge_kernel_into_user_pml4(loaded.pml4_phys); }

    // Map the crash-log page into the user PML4 so exception handlers can
    // write to it when CR3 = user PML4.  Supervisor-only (no PTE_USER).
    core_kernel::memory::map_crash_log_page(pml4);

    // Spawn the process using the ring-3 IRETQ trampoline.
    let trampoline = arch_x86_64::context::ring3_entry_trampoline as *const () as u64;
    let pid = sched.add_ring3_process(loaded.entry, stack_top_virt, loaded.pml4_phys, trampoline)?;
    if priority > 0 { sched.set_priority(pid, priority); }

    // Register all collected physical frames (ELF segments + stack + PML4) with
    // the PCB so they are freed when the process terminates.
    sched.register_user_frames(pid, &loaded.frames[..loaded.frame_count]);

    hal::uart::print_str("[ELF] entry=");
    hal::uart::print_hex(loaded.entry);
    hal::uart::print_str(" pml4=");
    hal::uart::print_hex(loaded.pml4_phys);
    hal::uart::print_str(" stack=");
    hal::uart::print_hex(stack_top_virt);
    hal::uart::print_str(" pid=");
    hal::uart::print_hex(pid.as_u32() as u64);
    hal::uart::print_str(" frames=");
    hal::uart::print_hex(loaded.frame_count as u64);
    hal::uart::print_str("\n");
    Some(pid)
}
