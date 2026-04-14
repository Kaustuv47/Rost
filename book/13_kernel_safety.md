# Chapter 13 — Kernel Safety & Integrity

## 13.1 The IEC 61508 Framework

IEC 61508 is the international standard for functional safety of electrical,
electronic, and programmable electronic safety-related systems.  SIL 4 (Safety
Integrity Level 4) is the highest level, with a target probability of dangerous
failures of less than 10^-8 per hour.

For software, IEC 61508 SIL 4 requires:

| Requirement | Reference | Rost Implementation |
|-------------|-----------|---------------------|
| Formal methods for design | §7.4.2 | TLA+ models for scheduler and IPC |
| Memory partitioning | §7.4.3 | Per-process PML4, NX, CR0.WP |
| Temporal partitioning | §7.4.1 | CPU budget quotas per process |
| Resource partitioning | §7.4.5 | Memory quotas, PCB frame tracking |
| Bounded blocking | §7.4.4 | Deadlock detection, IPC timeouts |
| Watchdog supervision | §7.4.9 | iB700 hardware watchdog |
| Post-mortem data | §7.4.6 | Persistent crash log |
| Fault injection testing | §7.4.7 | SYS_INJECT_FAULT feature |
| Chain-of-trust boot | §7.2.1 | UEFI Secure Boot + ExitBootServices |
| Hardware exclusivity | §7.2.1 | ExitBootServices removes firmware |
| DMA confinement | §7.4.3 | Intel VT-d IOMMU (passthrough mode) |
| Reproducible builds | §7.4.8 | SOURCE_DATE_EPOCH + path remapping |

## 13.2 Memory Partitioning

Memory partitioning ensures that a bug in one process cannot corrupt another's
memory.  In Rost this is implemented at multiple levels:

### 13.2.1 Virtual Memory Isolation

Every ring-3 process has its own PML4 (page table root).  When a process is
scheduled, `CR3` is loaded with that process's PML4 physical address.  The
MMU's hardware translation then enforces that only mapped pages are accessible.

An attempt to access an unmapped page raises a #PF.  In ring-3, the kernel
terminates the process; in ring-0, it halts the system.

### 13.2.2 SMEP (Supervisor Mode Execution Prevention)

SMEP (CR4, bit 20) prevents ring-0 code from executing at any virtual address
with `PTE_USER` set.  This closes the return-oriented programming vector where
an attacker overwrites a kernel function pointer to redirect execution to
user-space shellcode.

### 13.2.3 SMAP (Supervisor Mode Access Prevention)

SMAP (CR4, bit 21) prevents ring-0 code from reading or writing user-space memory
without explicit STAC/CLAC bracketing.  This prevents the kernel from accidentally
dereferencing a user-space pointer that has been passed directly without validation.

### 13.2.4 W^X (Write XOR Execute)

All writable pages are non-executable (NX bit set) and all executable pages are
non-writable (no WRITABLE bit).  This is enforced by:
- EFER.NXE = 1 (enables the NX/XD bit)
- CR0.WP = 1 (kernel cannot write to read-only pages)
- `apply_section_flags()` setting appropriate flags on each section

## 13.3 Temporal Partitioning

IEC 61508 §7.4.1 requires that safety-critical processes are guaranteed a minimum
CPU allocation.  Rost implements three mechanisms:

### 13.3.1 Preemptive Scheduling

The timer ISR fires at 100 Hz and can preempt any process.  No process can run
for more than one time slice (10 ticks = 100 ms) without being preempted.

### 13.3.2 CPU Budget Quotas

Per-process `cpu_budget_ticks` limits how many scheduler ticks a process can
consume in a 10-second frame.  A process with `cpu_budget_ticks = 200` can run
for at most 2 seconds out of every 10, regardless of what other processes are doing.

### 13.3.3 EDF Real-Time Scheduling

For processes with hard timing requirements, `SYS_SETRT` enables EDF scheduling.
EDF guarantees meeting all deadlines as long as the total utilization doesn't
exceed 100%.

## 13.4 The Crash Log

The persistent crash log is one of the most important safety features.  In a
deployed system, if a kernel fault occurs, the maintainer needs to know:
- Which exception fired
- At what RIP
- With what register state
- At what time (tick count)
- For which process

This information would be lost without the crash log if the system immediately
resets (as the watchdog causes).

### 13.4.1 Physical Address Selection

The crash log lives at physical address `0x4000` — a conventional location in
the first 640 KB of memory ("conventional memory") that BIOS/UEFI firmware
generally does not overwrite across warm resets.  This is the same region used
by BIOS interrupt vectors (0x0000–0x03FF), BIOS data area (0x0400–0x04FF), and
DOS data (0x0500–0x07FF).

Starting at 0x4000 is safely above all of these and below the EBDA (Extended BIOS
Data Area) which typically starts at 0x9E000 or higher.

```
Physical Address Layout (low memory):
  0x0000_0000 – 0x0000_03FF   IVT (Real Mode, 256 vectors × 4 bytes)
  0x0000_0400 – 0x0000_04FF   BIOS Data Area
  0x0000_0500 – 0x0000_3FFF   Free / Extended BIOS Data
  0x0000_4000 – 0x0000_4FFF   ROST CRASH LOG  ← here
  0x0000_5000 – ...           Available
```

### 13.4.2 Log Structure

```rust
/// Header magic: confirms the log is initialized and not overwritten by firmware.
const HEADER_MAGIC: u64 = 0xC0FFEE_DEADBEEF_u64;

/// 16 records of 64 bytes each, plus 64-byte header.
const LOG_SIZE: usize = 64 + 16 * 64;

#[repr(C)]
struct CrashLogHeader {
    magic:       u64,    // HEADER_MAGIC
    write_index: u64,    // next slot to write (wraps at 16)
    _pad:        [u8; 48],
}

#[repr(C)]
pub struct ErrorRecord {
    magic:      u64,    // per-record magic
    vector:     u8,
    _pad:       [u8; 7],
    tick:       u64,
    pid:        u32,
    _pad2:      u32,
    rip:        u64,
    rflags:     u64,
    cr2:        u64,    // page fault address (0 for non-PF faults)
}
```

### 13.4.3 Drain on Boot

Stage 1 calls `crash_log::drain()` with a callback function that prints each
record over the serial port.  The callback pattern keeps `core-kernel` (which
implements the crash log) free of HAL dependencies:

```rust
pub fn drain(printer: impl Fn(&ErrorRecord)) {
    let base = 0x4000 as *mut u8;
    // Read header
    let header = unsafe { &*(base as *const CrashLogHeader) };
    if header.magic != HEADER_MAGIC { return; }

    for i in 0..16 {
        let record = unsafe { &*((base as usize + 64 + i * 64) as *const ErrorRecord) };
        if record.magic != 0 {
            printer(record);
        }
    }
    // Zero the region after draining
    unsafe { write_bytes(base, 0, LOG_SIZE); }
    // Re-initialize the header
    init();
}
```

## 13.5 Stack Canaries

Stack canaries detect kernel stack corruption (buffer overflows that overwrite
the return address).

```rust
// crates/core-kernel/src/stack_guard.rs

/// The stack canary value placed between locals and the saved return address.
///
/// Using a fixed compile-time value (rather than a runtime-random value)
/// avoids the initialization race: efi_main's prologue runs before any
/// init() code could write a runtime value.  A random value would mismatch
/// the compile-time guard in the epilogue, causing a false-positive abort.
#[no_mangle]
pub static __security_cookie: u64 = 0x595F_D4A3_E8C7_2B16;

/// Called by the compiler-generated epilogue when the canary is mismatched.
#[no_mangle]
pub extern "C" fn __security_check_cookie(cookie: u64) {
    if cookie != __security_cookie {
        // Stack corruption detected — halt immediately
        unsafe { core::arch::asm!("cli", "hlt", options(nostack, nomem)); }
        loop {}
    }
}
```

The `-Z stack-protector=strong` compiler flag adds canary setup and check to
every function with a non-trivial stack frame.

The fixed canary value is a limitation: it provides protection against accidental
overflows but not against an attacker who knows the constant.  For a higher
security level, a runtime-random value (seeded from RDRAND) would be needed.
That is a documented future improvement.

## 13.6 CPU Feature Checks

Before activating hardware protection, `check_cpu_features()` verifies that
the required CPU features are present:

```rust
pub fn check_cpu_features(boot_info: &BootInfo) {
    // SMEP required: protects against ring-0 executing user code
    if !boot_info.cpu.features.smep {
        hal::uart::print_str("FATAL: CPU lacks SMEP\n");
        loop { halt(); }
    }

    // MSR required: for EFER, STAR, LSTAR, SFMASK
    if !boot_info.cpu.features.msr {
        hal::uart::print_str("FATAL: CPU lacks MSR\n");
        loop { halt(); }
    }

    // Single-core enforcement (IEC 61508 §7.4.10)
    if boot_info.cpu.max_logical_cpus > 1 {
        hal::uart::print_str("FATAL: multi-core CPU detected\n");
        hal::uart::print_str("       Rost requires single-core (SIL-4 constraint)\n");
        loop { halt(); }
    }

    // Secure Boot enforcement
    match boot_info.secure_boot {
        SecureBootState::Enabled => {} // good
        _ => {
            #[cfg(feature = "safety-mode")]
            {
                hal::uart::print_str("FATAL: Secure Boot not enabled\n");
                loop { halt(); }
            }
            #[cfg(not(feature = "safety-mode"))]
            hal::uart::print_str("WARN: Secure Boot not enabled\n");
        }
    }
}
```

### 13.6.1 Single-Core Enforcement

The most unconventional check is the single-core enforcement.  Why would a
modern kernel require single-core?

Multi-core execution introduces:
- **Data races** — shared kernel data structures need spinlocks on every access
- **Spinlock complexity** — lockdep analysis, priority inversion analysis, ABBA deadlock prevention
- **TLB coherency** — IPI-based TLB shootdown on every page table change
- **Cache coherency** — MESI protocol overhead for shared data

All of these add complexity that is hostile to IEC 61508 certification.  On a
single core, the combination of `cli`/`sti` and `RefCell` is sufficient for mutual
exclusion.  The kernel has no spinlocks, no MESI concerns, and no TLB shootdown.

For embedded safety systems, single-core is often a requirement anyway.

## 13.7 IOMMU Initialization (VT-d)

Direct Memory Access (DMA) is a potential safety/security vector: a device with
DMA access can write to any physical memory address.  A buggy or malicious DMA
transfer could corrupt kernel code or process data.

The Intel VT-d IOMMU provides DMA remapping: every DMA transfer is translated
through IOMMU page tables, preventing unauthorized access.

Rost initializes the IOMMU in passthrough mode (all DMA addresses translate
directly to the same physical address), which enables the hardware but does not
yet restrict any device.  This proves the hardware initialization path for the
future step of adding per-device restrictions.

The IOMMU initialization code uses bounded spin loops per IEC 61508 §7.4.1:

```rust
const MAX_ITER: u32 = 100_000;

fn wait_for_gsts_bit(base: u64, bit: u32) -> bool {
    for _ in 0..MAX_ITER {
        if gsts_read(base) & (1 << bit) != 0 { return true; }
    }
    // Bounded: log failure and return false instead of spinning forever
    hal::uart::print_str("WARN: IOMMU GSTS timeout\n");
    false
}
```

This is in contrast to the unbounded PIT calibration spin — DMA remapping
activation is a complex hardware operation that could theoretically fail,
so it has a timeout.

## 13.8 Reproducible Builds

Safety certification requires bit-for-bit identical builds from identical sources.
This is implemented in `scripts/build.sh`:

```bash
# Deterministic timestamp: use git commit time if SOURCE_DATE_EPOCH not set
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --pretty=%ct)}

# Strip absolute paths from debug info so builds on different machines produce
# identical binaries
export RUSTFLAGS="--remap-path-prefix=${ROOT}=/rost $RUSTFLAGS"

# Build and print the SHA-256 of the output EFI
cargo build --target x86_64-unknown-uefi -p kernel
sha256sum target/x86_64-unknown-uefi/debug/Rost.efi
```

The CI pipeline builds the kernel twice from a clean state and compares SHA-256
hashes.  If they differ, the build is non-reproducible — a CI failure.

## 13.9 The Health Monitor IPC Channel

Ring-3 fault handlers notify PID 1 (init) about every fault:

```rust
fn terminate_faulting_process(fault_code: u64) {
    // Send fault notification to init (PID 1)
    let mut msg = Message::zero();
    msg.data[0] = fault_code;        // 0xDE=#DE, 0x0D=#GP, 0x0E=#PF
    msg.data[1] = current_pid as u64;
    sched.send_message(current_pid, ProcessId::new(1), msg);
    // Terminate and reschedule
    sched.terminate_process(current_pid);
    tick_scheduler_isr();
}
```

Init's fault handling logic (Chapter 15) decides whether to restart the crashed
server or initiate an ordered shutdown.  This is the "fault → notify → decide →
act" loop that makes Rost self-healing.

## 13.10 Kernel Invariant Assertions

In debug builds, the scheduler includes runtime invariant checks:

```rust
#[cfg(debug_assertions)]
fn check_invariants(&self) {
    let ptable = self.process_table.borrow();
    for slot in ptable.processes.iter() {
        if let Some(pcb) = slot {
            // Every PID in the table must be in a valid range
            assert!(pcb.pid.as_u32() < MAX_PROCESSES as u32);
            // Entry-point addresses must not be in kernel-private range
            // (guard against corrupted PCB)
            assert!(pcb.context.rip < 0xFFFF_0000_0000_0000 ||
                    pcb.context.rip == IDLE_ENTRY);
        }
    }
}
```

These assertions don't run in production (`#[cfg(debug_assertions)]` is false in
release builds) but catch bugs during development that would otherwise manifest
as mysterious memory corruption.

## 13.11 Summary

Rost's safety and integrity mechanisms provide a comprehensive defense-in-depth:

| Layer | Mechanism | What it catches |
|-------|-----------|-----------------|
| CPU hardware | SMEP + SMAP + NXE + CR0.WP | Ring-3 privilege escalation, kernel pointer bugs |
| MMU | Per-process PML4, guard pages | Memory corruption, stack overflow |
| Compiler | Stack canaries | Buffer overflow return address corruption |
| Kernel logic | Memory/CPU/IPC quotas | Resource exhaustion attacks |
| Runtime | Deadlock detection | Livelock from circular IPC |
| Firmware | UEFI Secure Boot | Boot-time code injection |
| Hardware | watchdog | Scheduler/interrupt failure |
| Persistence | Crash log at 0x4000 | Post-mortem fault analysis |
| Formal | TLA+ models | Scheduling + IPC invariant violations |
