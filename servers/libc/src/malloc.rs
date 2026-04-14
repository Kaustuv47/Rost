//! Heap allocator — `malloc` / `free` / `calloc` / `realloc`.
//!
//! # Design
//!
//! The heap occupies virtual addresses starting at `HEAP_START` (8 MB) and
//! grows upward in 4 KB increments via `SYS_MAP(vaddr, paddr=0, flags=W|U)`.
//! A linear bump pointer (`HEAP_BUMP`) tracks the next unallocated byte.
//!
//! Every allocation is preceded by a 16-byte `BlockHdr`:
//!
//! ```text
//! [BlockHdr: 16 B][user data: alloc_size B]
//! ```
//!
//! `BlockHdr.total` is the total span including the header, rounded up to 16 B.
//! When a block is free its user-data region holds a `*mut BlockHdr` pointing
//! to the next free block of the same size class (free list).
//!
//! # Size classes
//!
//! | Class | User payload ≤ |
//! |-------|----------------|
//! | 0     | 8 B            |
//! | 1     | 16 B           |
//! | 2     | 32 B           |
//! | 3     | 64 B           |
//! | 4     | 128 B          |
//! | 5     | 256 B          |
//! | 6     | 512 B          |
//! | 7     | 1024 B         |
//! | 8     | 2048 B         |
//! | 9     | large (> 2048) |
//!
//! Large allocations are carved directly from the bump pointer with no free
//! list.  They can be freed (marked unused) but are not reused by subsequent
//! `malloc` calls — acceptable for a first-generation compatibility layer.

use core::sync::atomic::{AtomicUsize, Ordering};
use crate::syscall::sys_map;

// ── Constants ─────────────────────────────────────────────────────────────────

/// First virtual address of the managed heap.
/// Must be above all ELF load segments (servers stay well below 8 MB).
const HEAP_START: usize = 0x0080_0000;

/// SYS_MAP flags: bit 0 = writable, bit 1 = user-mode accessible.
const MAP_FLAGS_WU: u64 = 0x3;

/// Header alignment and minimum size.
const HDR_SIZE: usize  = 16;
const HDR_ALIGN: usize = 16;

/// Number of small size classes.
const NUM_CLASSES: usize = 9;

/// User payload limits for size classes 0..8.
const CLASS_SIZES: [usize; NUM_CLASSES] = [8, 16, 32, 64, 128, 256, 512, 1024, 2048];

// ── Block header ──────────────────────────────────────────────────────────────

/// Prefix stored before every allocation.
///
/// Layout is `#[repr(C)]` to ensure the field order and padding are fixed.
#[repr(C)]
struct BlockHdr {
    /// Total bytes including this header, rounded up to `HDR_ALIGN`.
    total: u32,
    /// 0 = free, 1 = in use.
    flags: u32,
    /// Unused — reserved for future metadata.
    _res: u64,
}

impl BlockHdr {
    const FLAG_USED: u32 = 1;

    #[inline]
    fn is_free(&self) -> bool { self.flags & Self::FLAG_USED == 0 }

    /// Pointer to the start of user data immediately following this header.
    #[inline]
    unsafe fn user_ptr(&mut self) -> *mut u8 {
        (self as *mut BlockHdr as *mut u8).add(HDR_SIZE)
    }

    /// Cast a user-data pointer back to its `BlockHdr`.
    ///
    /// SAFETY: `ptr` must originate from `user_ptr()` of a live `BlockHdr`.
    #[inline]
    unsafe fn from_user(ptr: *mut u8) -> *mut BlockHdr {
        ptr.sub(HDR_SIZE) as *mut BlockHdr
    }
}

// ── Free lists (one per size class) ──────────────────────────────────────────

/// Per-class free-list head stored as raw pointer-as-usize (0 = empty).
///
/// When a small block is freed its user-data region holds the `*mut BlockHdr`
/// of the next free block in the same class, allowing O(1) allocation.
static FREE_HEADS: [AtomicUsize; NUM_CLASSES] = {
    // AtomicUsize::new(0) × NUM_CLASSES
    [
        AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0),
        AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0),
        AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0),
    ]
};

// ── Bump pointer ──────────────────────────────────────────────────────────────

/// Next byte to allocate from (initially `HEAP_START`).
static HEAP_BUMP: AtomicUsize = AtomicUsize::new(HEAP_START);

/// Highest virtual address that has been mapped so far.
/// Starts at `HEAP_START - 4096` (one page below the heap) so the first
/// allocation triggers a `SYS_MAP`.
static HEAP_MAP_TOP: AtomicUsize = AtomicUsize::new(HEAP_START);

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Size class index for a requested user payload of `user_size` bytes.
fn size_class(user_size: usize) -> Option<usize> {
    for (i, &limit) in CLASS_SIZES.iter().enumerate() {
        if user_size <= limit { return Some(i); }
    }
    None // large
}

/// Total block size (header + user data), rounded up to `HDR_ALIGN`.
fn total_for(user_size: usize) -> usize {
    let raw = HDR_SIZE + user_size;
    (raw + HDR_ALIGN - 1) & !(HDR_ALIGN - 1)
}

/// Ensure [addr, addr+size) is backed by mapped pages, requesting new pages
/// from the kernel as needed.  Returns `false` if mapping fails.
fn ensure_mapped(addr: usize, size: usize) -> bool {
    let end = addr + size;
    let mut top = HEAP_MAP_TOP.load(Ordering::Relaxed);
    while top < end {
        let page = top & !0xFFF; // 4 KB aligned
        let ret = sys_map(page as u64, 0, MAP_FLAGS_WU);
        if ret != 0 { return false; } // EINVAL / ENOMEM
        top += 0x1000;
        HEAP_MAP_TOP.store(top, Ordering::Relaxed);
    }
    true
}

/// Allocate `total` bytes from the bump allocator, mapping pages as needed.
/// Returns a pointer to the start (i.e. `BlockHdr`), or null on failure.
fn bump_alloc(total: usize) -> *mut BlockHdr {
    let addr = HEAP_BUMP.load(Ordering::Relaxed);
    // Align to HDR_ALIGN.
    let aligned = (addr + HDR_ALIGN - 1) & !(HDR_ALIGN - 1);
    if !ensure_mapped(aligned, total) { return core::ptr::null_mut(); }
    HEAP_BUMP.store(aligned + total, Ordering::Relaxed);
    aligned as *mut BlockHdr
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Allocate at least `size` bytes.  Returns a pointer aligned to 16 bytes,
/// or null if `size == 0` or the allocation fails.
///
/// The returned memory is **not** zeroed.  Use `calloc` for zeroed memory.
#[no_mangle]
pub unsafe extern "C" fn malloc(size: usize) -> *mut u8 {
    if size == 0 { return core::ptr::null_mut(); }

    let class = size_class(size);
    let total  = total_for(if class.is_some() { CLASS_SIZES[class.unwrap()] } else { size });

    // Try free list for small classes.
    if let Some(ci) = class {
        let head = FREE_HEADS[ci].load(Ordering::Relaxed);
        if head != 0 {
            let hdr = head as *mut BlockHdr;
            // Pop the free-list chain stored in user data.
            let next_ptr = *((*hdr).user_ptr() as *const usize);
            FREE_HEADS[ci].store(next_ptr, Ordering::Relaxed);
            (*hdr).flags = BlockHdr::FLAG_USED;
            return (*hdr).user_ptr();
        }
    }

    // Carve from bump.
    let hdr = bump_alloc(total);
    if hdr.is_null() { return core::ptr::null_mut(); }
    (*hdr).total = total as u32;
    (*hdr).flags = BlockHdr::FLAG_USED;
    (*hdr)._res  = 0;
    (*hdr).user_ptr()
}

/// Free memory previously returned by `malloc`, `calloc`, or `realloc`.
/// Passing `null` is a no-op.
#[no_mangle]
pub unsafe extern "C" fn free(ptr: *mut u8) {
    if ptr.is_null() { return; }
    let hdr = BlockHdr::from_user(ptr);
    if (*hdr).is_free() { return; } // double-free guard
    (*hdr).flags = 0;

    let user_bytes = (*hdr).total as usize - HDR_SIZE;
    if let Some(ci) = size_class(user_bytes) {
        // Push onto the free list: store old head in the block's user data.
        let old_head = FREE_HEADS[ci].load(Ordering::Relaxed);
        *(ptr as *mut usize) = old_head;
        FREE_HEADS[ci].store(hdr as usize, Ordering::Relaxed);
    }
    // Large blocks are marked free but not recycled.
}

/// Allocate `nmemb * size` bytes, zeroed.
/// Returns null if either argument is zero or overflow occurs.
#[no_mangle]
pub unsafe extern "C" fn calloc(nmemb: usize, size: usize) -> *mut u8 {
    let total = match nmemb.checked_mul(size) {
        Some(n) if n > 0 => n,
        _ => return core::ptr::null_mut(),
    };
    let ptr = malloc(total);
    if !ptr.is_null() {
        core::ptr::write_bytes(ptr, 0, total);
    }
    ptr
}

/// Return the current heap bump pointer (used by `sbrk` to query the break).
#[inline]
pub fn heap_bump_export() -> usize {
    HEAP_BUMP.load(Ordering::Relaxed)
}

/// Resize an allocation.  Behaviour matches standard `realloc`:
/// * `ptr == null` → equivalent to `malloc(new_size)`
/// * `new_size == 0` → equivalent to `free(ptr)`, returns null
/// * Otherwise: allocate `new_size`, copy `min(old, new)` bytes, free old.
#[no_mangle]
pub unsafe extern "C" fn realloc(ptr: *mut u8, new_size: usize) -> *mut u8 {
    if ptr.is_null() { return malloc(new_size); }
    if new_size == 0 { free(ptr); return core::ptr::null_mut(); }

    let hdr      = BlockHdr::from_user(ptr);
    let old_user = (*hdr).total as usize - HDR_SIZE;
    if new_size <= old_user { return ptr; } // fits in existing block

    let new_ptr = malloc(new_size);
    if new_ptr.is_null() { return core::ptr::null_mut(); }
    core::ptr::copy_nonoverlapping(ptr, new_ptr, old_user);
    free(ptr);
    new_ptr
}
