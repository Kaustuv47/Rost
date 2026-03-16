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
    PTE_PRESENT, PTE_WRITABLE, PTE_USER, PTE_NO_EXECUTE,
};

// ── ELF constants ─────────────────────────────────────────────────────────────

const ELFMAG:     [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8      = 2;
const ELFDATA2LSB: u8     = 1;  // little-endian
const EM_X86_64:  u16     = 0x3E;

const PT_LOAD:    u32     = 1;   // loadable segment

// Segment flags (p_flags)
const PF_EXEC:  u32 = 0x1;
const PF_WRITE: u32 = 0x2;
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

/// Result of a successful ELF load.
pub struct LoadedElf {
    /// Virtual entry point address.
    pub entry: u64,
    /// Physical address of the PML4 used for this process.
    pub pml4_phys: u64,
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
    // ── 1. Parse and validate ELF header ──────────────────────────────────────

    if data.len() < core::mem::size_of::<Elf64Ehdr>() { return None; }
    let ehdr = unsafe { &*(data.as_ptr() as *const Elf64Ehdr) };

    if ehdr.e_ident[..4] != ELFMAG          { return None; }
    if ehdr.e_ident[4]   != ELFCLASS64      { return None; }
    if ehdr.e_ident[5]   != ELFDATA2LSB     { return None; }
    if ehdr.e_machine    != EM_X86_64       { return None; }
    if ehdr.e_phentsize  != core::mem::size_of::<Elf64Phdr>() as u16 { return None; }
    if ehdr.e_phnum      == 0               { return None; }

    // ── 2. Obtain or create the target PML4 ───────────────────────────────────

    let (pml4_ref, pml4_phys) = match pml4 {
        Some(t) => {
            let phys = t as *mut PageTable as u64;
            (t, phys)
        }
        None => {
            let phys = global_alloc_4k()?;
            unsafe { core::ptr::write_bytes(phys as *mut u8, 0, 4096); }
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

        let phdr = unsafe { &*(data[off..].as_ptr() as *const Elf64Phdr) };
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

        // Map and fill pages in 4 KB chunks.
        let mut page_off: usize = 0;
        while page_off < mem_size {
            let phys_page = global_alloc_4k()?;
            unsafe { core::ptr::write_bytes(phys_page as *mut u8, 0, 4096); }

            // Copy file data into this page (up to 4 KB or remaining bytes).
            let copy_start = page_off;
            let copy_end   = core::cmp::min(page_off + 4096, seg_data.len());
            if copy_start < seg_data.len() {
                let n = copy_end - copy_start;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        seg_data[copy_start..].as_ptr(),
                        phys_page as *mut u8,
                        n,
                    );
                }
            }

            let virt_page = virt_base + page_off as u64;
            if !map_page_global(pml4_ref, virt_page, phys_page, flags) {
                return None;
            }

            page_off += 4096;
        }
    }

    Some(LoadedElf { entry: ehdr.e_entry, pml4_phys })
}

// ── Convenience: spawn an ELF as a new process ───────────────────────────────

/// Load `data` and register the resulting process with the global scheduler.
///
/// `priority` 0 means "use default (128)".  Returns the new PID, or `None` on
/// any failure.
pub fn spawn_elf(data: &[u8], priority: u8) -> Option<core_kernel::process::ProcessId> {
    let loaded = load(data, None)?;
    let sched  = core_kernel::scheduler::get_global()?;
    let pid    = sched.add_process(loaded.entry, 0, loaded.pml4_phys)?;
    if priority > 0 { sched.set_priority(pid, priority); }
    hal::uart::print_str("[ELF] loaded entry=");
    hal::uart::print_hex(loaded.entry);
    hal::uart::print_str(" pml4=");
    hal::uart::print_hex(loaded.pml4_phys);
    hal::uart::print_str(" pid=");
    hal::uart::print_hex(pid.as_u32() as u64);
    hal::uart::print_str("\n");
    Some(pid)
}
