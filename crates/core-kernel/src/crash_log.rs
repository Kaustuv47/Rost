/// Persistent crash log — survives warm reset.
///
/// # Physical layout
/// A 4 KB page at [`CRASH_LOG_PHYS`] (16 KB mark in conventional memory) holds
/// a header followed by up to [`MAX_RECORDS`] 64-byte [`ErrorRecord`] entries in
/// a ring buffer.  The region is identity-mapped after Stage 1 and is never
/// reclaimed by the physical allocator.
///
/// # Lifecycle
/// 1. `check_and_print()` — called after page tables are active (Stage 1).
///    Reads any valid records, prints them over serial, then clears the region.
/// 2. `init()` — called immediately after, stamping the region header magic so
///    subsequent `write()` calls know the region is ready.
/// 3. `write(record)` — called from fatal exception handlers (ring-0 #PF/#GP/#DF/#MC).
///    Writes a record into the ring; safe to call with IF=0 (single-core, no lock).
///
/// # Warm-reset survival
/// QEMU and real hardware preserve DRAM contents across ACPI warm resets
/// (port 0xCF9 or ACPI RESET_REG).  The `HEADER_MAGIC` cookie distinguishes
/// a valid log from uninitialized RAM (all-zeros or all-ones after power cycle).
///
/// # IEC 61508 §7.4.6
/// Post-mortem data (crash records) are required for any SIL-3/SIL-4 system
/// to support root-cause analysis after an unplanned safe-state transition.

// ── Constants ─────────────────────────────────────────────────────────────────

/// Physical base address of the crash log region (4 KB page, conventional memory).
/// Must be identity-mapped before any read/write.
pub const CRASH_LOG_PHYS: u64 = 0x0000_4000;

/// Written to the `header_magic` field to distinguish a live log from garbage.
const HEADER_MAGIC: u64 = 0xC0DE_CAFE_DEAD_BEEFu64;

/// Written to `ErrorRecord.magic` to mark an entry as valid.
const RECORD_MAGIC: u64 = 0xE440_DEAD_CAFE_0001u64;

/// Maximum number of records in the ring buffer.
pub const MAX_RECORDS: usize = 16;

// ── ErrorRecord ───────────────────────────────────────────────────────────────

/// A single crash record.  64 bytes, naturally aligned.
///
/// Fields mirror the ExceptionFrame available at the point of fault plus the
/// scheduler tick and current PID for correlation with the IPC audit log.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct ErrorRecord {
    /// `RECORD_MAGIC` when the slot is valid; 0 when empty.
    pub magic:      u64,
    /// Exception vector or error code (e.g. 0x0D = #GP, 0x0E = #PF, 0x08 = #DF).
    pub vector:     u64,
    /// Scheduler tick at the time of the fault (`TICK_COUNT`).
    pub tick:       u64,
    /// PID of the process that was running when the fault occurred.
    pub pid:        u64,
    /// Faulting instruction pointer (RIP from ExceptionFrame).
    pub rip:        u64,
    /// Stack pointer at the time of the fault (RSP from ExceptionFrame).
    pub rsp:        u64,
    /// RFLAGS at the time of the fault.
    pub rflags:     u64,
    /// Faulting virtual address for #PF (CR2); 0 for all other exceptions.
    pub cr2:        u64,
}

const _RECORD_SIZE_CHECK: () = assert!(core::mem::size_of::<ErrorRecord>() == 64);

// ── CrashLogRegion ────────────────────────────────────────────────────────────

/// In-memory layout of the 4 KB crash log region.
#[repr(C)]
struct CrashLogRegion {
    /// `HEADER_MAGIC` when the region has been initialised by this kernel.
    header_magic: u64,
    /// Index of the next slot to write (wraps modulo MAX_RECORDS).
    write_index:  u64,
    /// Total records written since `init()` (used to detect wrap-around).
    count:        u64,
    _pad:         [u64; 5],   // pad header to 64 bytes
    records:      [ErrorRecord; MAX_RECORDS],
}

// Header is 64 bytes, records are 16 × 64 = 1024 bytes. Total = 1088 < 4096.
const _REGION_SIZE_CHECK: () = assert!(core::mem::size_of::<CrashLogRegion>() == 1088);

// ── Safe access helper ────────────────────────────────────────────────────────

/// Return a mutable reference to the crash log region.
///
/// # Safety
/// Must only be called after the kernel PML4 is active (Stage 1) so that the
/// identity mapping at `CRASH_LOG_PHYS` is valid.  The caller must ensure
/// exclusive access (IF=0 on single-core suffices).
#[inline]
unsafe fn region() -> &'static mut CrashLogRegion {
    &mut *(CRASH_LOG_PHYS as *mut CrashLogRegion)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Drain the persistent crash log: call `f` for every valid record (oldest first),
/// then zero the region.
///
/// Returns the number of records passed to `f` (0 on a clean first boot).
///
/// Call this in Stage 1, **after** `activate_page_table()` and **before** `init()`.
/// The caller is responsible for printing or otherwise handling each record.
pub fn drain(mut f: impl FnMut(usize, usize, &ErrorRecord)) -> usize {
    unsafe {
        let r = region();
        if r.header_magic != HEADER_MAGIC {
            return 0;
        }
        let count = r.count.min(MAX_RECORDS as u64) as usize;
        if count == 0 {
            return 0;
        }

        let total = r.count as usize;
        let start = r.write_index as usize;
        let mut seen = 0usize;
        for i in 0..count {
            let idx = start.wrapping_add(MAX_RECORDS).wrapping_sub(count)
                .wrapping_add(i) % MAX_RECORDS;
            let rec = &r.records[idx];
            if rec.magic != RECORD_MAGIC { continue; }
            f(seen, total, rec);
            seen += 1;
        }

        // Zero the region so stale records don't re-appear on the next boot.
        core::ptr::write_bytes(r as *mut CrashLogRegion, 0, 1);
        seen
    }
}

/// Stamp the region header magic so `write()` knows the region is ready.
///
/// Call this in Stage 1, after `check_and_print()` (which zeros the region).
pub fn init() {
    unsafe {
        let r = region();
        r.header_magic = HEADER_MAGIC;
        r.write_index  = 0;
        r.count        = 0;
    }
}

/// Write a crash record to the ring buffer.
///
/// Safe to call from any exception handler running with IF=0.  On a single-core
/// kernel this gives exclusive access; no lock is needed.
///
/// If called before `init()` (e.g., very early exception during Stage 1 before
/// `init()` runs) the magic check in `check_and_print()` will reject the record
/// on the next boot — acceptable for extremely early crashes.
pub fn write(record: ErrorRecord) {
    unsafe {
        let r = region();
        // Write even if header_magic is not set — the record will still be
        // readable on next boot if the header was written first.
        let idx = (r.write_index as usize) % MAX_RECORDS;
        r.records[idx] = record;
        r.records[idx].magic = RECORD_MAGIC;
        r.write_index = r.write_index.wrapping_add(1) % MAX_RECORDS as u64;
        r.count = r.count.saturating_add(1);
        // Ensure the header magic is stamped so the next boot can find this record.
        r.header_magic = HEADER_MAGIC;
    }
}
