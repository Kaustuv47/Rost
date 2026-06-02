//! Ring-3 ELF64 loader for rost-exec.
//!
//! Parses PT_LOAD segments and builds a new address space entirely from
//! ring-3 using the four exec-server syscalls:
//!   SYS_ALLOC_PHYS (34)   — allocate a 4 KB physical frame
//!   SYS_CREATE_VAS (35)   — allocate a PML4 with kernel pages merged in
//!   SYS_MAP_INTO_VAS (36) — map a frame into the new address space
//!   SYS_SPAWN_WITH_VAS (37) — create the process (kernel allocates stack)
//!   SYS_REGISTER_FRAMES (38) — hand segment frames to the new PCB
//!
//! The ELF bytes are supplied as a raw pointer + length pointing into exec
//! server's own virtual address space (caller has already mapped them there
//! via SYS_MAP before calling this function).

use crate::syscall;

// ── ELF constants ─────────────────────────────────────────────────────────────

const ELFCLASS64:  u8  = 2;
const ELFDATA2LSB: u8  = 1;
const EM_X86_64:   u16 = 0x3E;
const PT_LOAD:     u32 = 1;
const PF_EXEC:     u32 = 0x1;
const PF_WRITE:    u32 = 0x2;

// ── ELF structures ─────────────────────────────────────────────────────────────

#[repr(C)]
struct Elf64Ehdr {
    e_ident:     [u8; 16],
    e_type:      u16,
    e_machine:   u16,
    e_version:   u32,
    e_entry:     u64,
    e_phoff:     u64,
    _e_shoff:    u64,
    _e_flags:    u32,
    _e_ehsize:   u16,
    e_phentsize: u16,
    e_phnum:     u16,
    _e_shentsize: u16,
    _e_shnum:    u16,
    _e_shstrndx: u16,
}

#[repr(C)]
struct Elf64Phdr {
    p_type:   u32,
    p_flags:  u32,
    p_offset: u64,
    p_vaddr:  u64,
    _p_paddr: u64,
    p_filesz: u64,
    p_memsz:  u64,
    _p_align: u64,
}

// ── Staging area ───────────────────────────────────────────────────────────────

// Growing VA cursor for exec server's staging area.
// Each SYS_MAP call uses a fresh 4 KB VA — never recycled, avoiding TLB issues.
// With 128 TB of user address space this will never overflow in practice.
static mut STAGING_VA: u64 = 0x0000_4000_0000; // start at 1 GB

fn next_staging_va() -> u64 {
    // SAFETY: exec server is single-threaded; no concurrent access.
    let va = unsafe { STAGING_VA };
    unsafe { STAGING_VA = STAGING_VA.wrapping_add(4096); }
    va
}

// MAP flags for SYS_MAP (into exec server's own VAS):
const MAP_USER_RW: u64 = 0b11; // user-accessible, writable

// MAP_INTO_VAS flags for target process PML4 (bit 0=WRITABLE, bit 1=USER, bit 2=NO_EXECUTE):
const VAS_RX:  u64 = 0b010; // read + execute (code segment)
const VAS_RW:  u64 = 0b011; // read + write (data segment, set NX below)

// Maximum physical frames tracked per ELF load.
const MAX_FRAMES: usize = 128;

fn print(s: &str) {
    for b in s.bytes() { syscall::uart_write(b); }
}

/// Load an ELF64 binary from a virtual address already mapped in exec server's
/// address space, and spawn it as a new ring-3 process.
///
/// `elf_va`   — virtual address of the first byte of the ELF image in exec's VAS.
/// `elf_len`  — byte length of the image.
/// `priority` — scheduling priority (0 = default 128).
///
/// Returns `Some(pid)` on success, `None` on any parse or allocation failure.
pub fn load_and_spawn(elf_va: u64, elf_len: usize, priority: u8) -> Option<u32> {
    if elf_len < core::mem::size_of::<Elf64Ehdr>() { return None; }

    // SAFETY: caller mapped elf_va..elf_va+elf_len before calling us.
    let data = unsafe { core::slice::from_raw_parts(elf_va as *const u8, elf_len) };

    // ── Validate ELF header ────────────────────────────────────────────────────
    let ehdr: Elf64Ehdr = unsafe {
        core::ptr::read_unaligned(data.as_ptr() as *const Elf64Ehdr)
    };
    if ehdr.e_ident[0] != 0x7f || &ehdr.e_ident[1..4] != b"ELF" { return None; }
    if ehdr.e_ident[4] != ELFCLASS64  { return None; }
    if ehdr.e_ident[5] != ELFDATA2LSB { return None; }
    if ehdr.e_machine  != EM_X86_64   { return None; }
    if ehdr.e_phentsize != core::mem::size_of::<Elf64Phdr>() as u16 { return None; }
    if ehdr.e_phnum == 0 { return None; }

    // ── Create a fresh PML4 with kernel entries already merged ─────────────────
    let pml4_paddr = syscall::create_vas();
    if pml4_paddr == 0 {
        print("[exec] OOM: create_vas\n");
        return None;
    }

    let mut frames = [0u64; MAX_FRAMES];
    let mut frame_count = 0usize;

    // ── Map each PT_LOAD segment ───────────────────────────────────────────────
    let ph_off = ehdr.e_phoff as usize;
    let ph_cnt = ehdr.e_phnum as usize;

    for i in 0..ph_cnt {
        let off = ph_off + i * core::mem::size_of::<Elf64Phdr>();
        if off + core::mem::size_of::<Elf64Phdr>() > elf_len { return None; }

        let phdr: Elf64Phdr = unsafe {
            core::ptr::read_unaligned(data[off..].as_ptr() as *const Elf64Phdr)
        };
        if phdr.p_type != PT_LOAD { continue; }

        let file_end = (phdr.p_offset as usize).checked_add(phdr.p_filesz as usize)?;
        if file_end > elf_len { return None; }
        let seg_data = &data[phdr.p_offset as usize..file_end];

        let mem_size = phdr.p_memsz as usize;
        if mem_size == 0 { continue; }

        // Determine PTE flags from ELF segment permissions.
        let vas_flags: u64 = if phdr.p_flags & PF_EXEC != 0 {
            if phdr.p_flags & PF_WRITE != 0 { VAS_RW } else { VAS_RX }
        } else {
            VAS_RW | 4 // data/BSS: writable + NO_EXECUTE
        };

        let page_align_offset = (phdr.p_vaddr & 0xFFF) as usize;
        let virt_page_base    = phdr.p_vaddr & !0xFFF;
        let total_pages       = (page_align_offset + mem_size + 4095) / 4096;

        for page_idx in 0..total_pages {
            // Allocate a fresh physical frame.
            let paddr = syscall::alloc_phys();
            if paddr == 0 {
                print("[exec] OOM: alloc_phys\n");
                return None;
            }

            // Map the frame into exec server's staging area for writing.
            let write_va = next_staging_va();
            if !syscall::map(write_va, paddr, MAP_USER_RW) {
                print("[exec] map staging failed\n");
                return None;
            }

            // Copy ELF data (frame is already zeroed by SYS_ALLOC_PHYS).
            let (seg_start, phys_offset) = if page_idx == 0 {
                (0usize, page_align_offset)
            } else {
                (page_idx * 4096 - page_align_offset, 0usize)
            };
            if seg_start < seg_data.len() {
                let n = core::cmp::min(seg_data.len() - seg_start, 4096 - phys_offset);
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        seg_data[seg_start..].as_ptr(),
                        (write_va as *mut u8).add(phys_offset),
                        n,
                    );
                }
            }

            // Map the same frame into the new process's PML4.
            let target_va = virt_page_base + (page_idx as u64 * 4096);
            if !syscall::map_into_vas(pml4_paddr, target_va, paddr, vas_flags) {
                print("[exec] map_into_vas failed\n");
                return None;
            }

            // Record frame for reclaim registration.
            if frame_count < MAX_FRAMES {
                frames[frame_count] = paddr;
                frame_count += 1;
            }
        }
    }

    // ── Spawn the process (kernel allocates + maps the user stack) ─────────────
    let pid = syscall::spawn_with_vas(pml4_paddr, ehdr.e_entry, priority)?;

    // ── Register ELF segment frames for reclaim on process exit ───────────────
    syscall::register_frames(pid, &frames[..frame_count]);

    Some(pid)
}
