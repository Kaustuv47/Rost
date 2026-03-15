# Rost — Roadmap

Items are grouped by the boundary they live on.
**Part I — Kernel** covers everything that must execute in ring 0 and that a
SIL-4 / formally-verified microkernel is required to provide.
**Part II — User Space** covers every server, driver, library, and application
that runs in ring 3 and communicates with the kernel exclusively through IPC.

Status markers:
- `[x]` — implemented and compiles cleanly
- `[~]` — skeleton / partial — structure exists but the hard part is missing
- `[ ]` — not started

---

## Part I — Kernel

The kernel must do exactly these things and nothing more.
Any feature not in this list belongs in Part II.

---

### 1  Boot & UEFI Hardware Discovery

The kernel binary IS the UEFI application — no separate bootloader.
All hardware information is captured while UEFI boot services are live and
stored in a `static BootInfo` that every subsystem reads for its entire lifetime.

```
[x] UEFI entry point  (#[entry] fn efi_main)
[x] Serial console    (COM1, 38 400 baud 8N1 via port I/O — works before any driver)
[x] Firmware info     (vendor string, UEFI revision)
[x] CPUID collection  (vendor, brand, family/model/stepping, address bits, feature flags)
[x] Physical memory map  (all UEFI MemoryType regions → MemoryKind)
[x] GOP framebuffer info  (base, size, resolution, stride, pixel format — up to 4 outputs)
[x] ACPI RSDP address   (v1 fallback, v2 preferred)
[x] SMBIOS entry point  (32-bit v2 and 64-bit v3)
[x] Secure Boot state   (Enabled / Disabled / SetupMode / Unknown)
[x] Boot-time wall clock
[x] Kernel command-line (load options, UCS-2 → ASCII)
[ ] Call ExitBootServices() — take exclusive hardware ownership, end UEFI involvement
[ ] Validate Secure Boot state; halt or warn when not Enabled (safety-mode build flag)
[ ] Parse ACPI MADT — discover Local APIC and I/O APIC addresses
[ ] Parse ACPI DMAR — discover IOMMU (Intel VT-d) units
[ ] Parse ACPI FADT — discover watchdog timer, RESET_REG
```

---

### 2  Physical Memory Management

Owns every byte of RAM from the moment ExitBootServices is called.

```
[x] Bump allocator    (4 KB-aligned; seeded from largest usable UEFI region)
[x] Allocate          (rounds up to 4 KB, decrements heap_remaining)
[ ] Free-list / slab allocator
      — IEC 61508 forbids non-deallocating allocators in long-running safety software
      — required before process termination can reclaim stacks and page tables
[ ] Per-type object pools
      — fixed-size pools for PCBs, PageTables, Channels so the allocator is never
        called on a hot path (provable WCET)
[ ] OOM handler (alloc_error_handler)
      — rustc returns null on OOM; must be overridden with a defined safe-state response
[ ] Physical frame tracker
      — each 4 KB frame tagged as: Free / KernelCode / KernelData / UserOwned / MMIO
      — needed for capability-based mmap and IOMMU mapping
[ ] Persistent error log region
      — reserve N pages that survive warm reset for crash records
```

---

### 3  Virtual Memory & Paging

Provides isolated virtual address spaces; the mechanism by which the kernel
enforces spatial isolation between processes.

```
[x] PageTable struct          (#[repr(C, align(4096))], 512 × u64 entries)
[x] 4-level walk: PML4→PDPT→PD→PT
[x] map_page(pml4, virt, phys, writable, alloc)  — allocates missing intermediate tables
[x] translate_address(pml4, virt)                — full 4-level walk
[x] PTE flags: PRESENT, WRITABLE, USER, ADDR_MASK, HUGE_PAGE (bit 7), NO_EXECUTE (bit 63)
[x] PTE_HUGE_ADDR_MASK (bits[51:21]) for 2 MB PD entries
[x] activate_page_table(pml4_phys)              — writes CR3
[x] identity_map_region(pml4, base, size, flags, alloc)
      — maps a physical region using 2 MB huge pages (PD-level PS bit)
      — skips already-present entries; rounds to 2 MB boundaries
[x] KERNEL_PML4 static (BSS, 4 KB-aligned)
      — all UEFI memory regions identity-mapped at boot via identity_map_region
[x] CR3 loaded at boot (Stage 1 — after mapping all UEFI regions)
[x] EFER.NXE = 1   — No-Execute bit globally enabled (init_protection)
[x] CR0.WP = 1     — kernel cannot write to read-only pages (init_protection)
[x] CR4.SMEP = 1   — supervisor cannot execute user-mode pages (init_protection)
[~] CR4.SMAP = 1   — bit set in init_protection; no stac/clac brackets yet
      — SMAP is active but deliberate user-memory access paths are not yet guarded
[x] CR3 reload on context switch
      — switch_context(old, new, new_pml4) — third arg triggers mov cr3,rdx when non-zero
      — kernel processes pass kernel_pml4_phys; test rdx/jz skips redundant TLB flush
[~] PTE_NO_EXECUTE applied to data pages
      — flag constant exists and NXE is enabled; data pages are not yet mapped NX
      — requires linker-script section splitting + per-region PTE flags
[~] Per-process PML4
      — page_table_base threaded through create_process → PCB → timer_tick → switch_context
      — all current processes share the kernel PML4; per-process allocation deferred to ELF loader
[ ] Kernel guard pages
      — one unmapped 4 KB page immediately below each kernel stack
      — stack overflow → #PF instead of silent adjacent-memory corruption
[ ] Kernel .text mapped read-only
      — requires linker script sections + per-section PTE flags at boot
[ ] TLB shootdown stub  (single-core: invlpg; SMP: IPI path reserved)
[ ] Huge pages for kernel .text / .data split  — NX on data, X on .text only
```

---

### 4  CPU Structures & Privilege Levels

Descriptor tables and MSRs that define the hardware security boundary between
ring 0 and ring 3.

```
[x] GDT — 5 entries
      null / ring-0 code (0x08) / ring-0 data (0x10) /
      ring-3 data (0x18) / ring-3 code (0x20)
[x] GDT load  (lgdt + far-ret to reload CS, mov to DS/ES/SS)
[x] IDT — 256 gates, all interrupt-gate type, loaded from static
[x] enable_interrupts / disable_interrupts / halt
[x] rdmsr / wrmsr
[x] read_cr2
[x] activate_page_table  (writes CR3)

[x] Task State Segment (TSS)
      — tss.rs: TaskStateSegment (#[repr(C,packed)], 104 bytes), IST1/IST2/IST3 stacks
      — GDT extended to 7 entries; 16-byte system descriptor at slots 5/6 (selector 0x28)
      — init_tss() fills IST stacks; install_tss() encodes the TSS descriptor into GDT
      — ltr 0x28 loads the Task Register after GDT.load()
[x] TSS.RSP0 update on every context switch
      — tick_scheduler() calls set_rsp0(kernel_rsp) before switch_context()
      — kernel_rsp is the 4th element returned by timer_tick()
[x] CR0.WP = 1           (init_protection — see §3)
[x] CR4.SMEP + CR4.SMAP  (init_protection — see §3)
[x] EFER.NXE             (init_protection — see §3)
[ ] CR4.FSGSBASE = 1  — enables rdfsbase/wrfsbase for fast TLS in user space
[ ] Local APIC init
      — mask 8259 PIC after APIC is enabled
      — configure LAPIC timer as high-resolution tick source (replaces PIT for scheduling)
      — set spurious interrupt vector (0xFF)
[ ] I/O APIC init   (from MADT; route IRQs through I/O APIC, not 8259)
[ ] IOMMU (Intel VT-d) init
      — restrict each DMA-capable device to only its own memory regions
      — prevents a rogue driver from reading/writing arbitrary physical memory
```

---

### 5  Interrupt & Exception Handling

Handles all CPU exceptions and hardware interrupts.  A fault in one process
must never halt the system.

```
[x] #DE  (vector  0) — divide by zero     — dumps all 15 GPRs + rip/rflags/error_code → serial
[x] #GP  (vector 13) — general protection — full register dump → serial
[x] #PF  (vector 14) — page fault         — CR2 + full register dump → serial
[x] IRQ0 (vector 32) — PIT timer 100 Hz   — lock inc TICK_COUNT + EOI → iretq
[x] ExceptionFrame   (#[repr(C)] layout matches ISR push order)
[x] TICK_COUNT       (pub static AtomicU64, readable from any crate)

[x] All 256 IDT vectors registered
      — vectors 1,3–7,9–12,15–17,19–31,33–47,48–254: catch-all naked stubs
      — catch-all logs vector + register dump, EOI, iretq — no triple fault possible
[x] #NMI (vector 2)  — IST2 dedicated stack; logs "NMI received" and iretq (non-fatal)
[x] #DF  (vector 8)  — IST1 dedicated stack; always runs on a fresh stack; logs + halts
[x] #MC  (vector 18) — IST3 dedicated stack; logs "machine check" + halts
[x] User/kernel fault distinction in #DE, #GP, #PF handlers
      — ExceptionFrame.cs & 3 == 3 → user-mode fault
      — prints "origin: user-mode" or "origin: kernel"; future: terminate + notify HM
[x] Spurious interrupt handler (vector 255) — iretq only, no EOI (LAPIC spurious)
[x] MAX_ISR_LATENCY (pub static AtomicU64) — placeholder for latency measurement
[~] Preemptive context switch from timer ISR
      — timer ISR increments TICK_COUNT; scheduler.timer_tick() returns switch triple
      — full preemptive ISR (save all GPRs, swap stacks, iretq) deferred until
        a global SCHEDULER static is safe to access from ISR context
```

---

### 6  Process Management

Creates and destroys processes; owns per-process state for the kernel's lifetime.

```
[x] ProcessId         (u32 newtype, Copy)
[x] ProcessState      (Ready / Running / Blocked / Terminated)
[x] TaskContext       (#[repr(C)], all 15 GPRs + rsp + rip + rflags, documented byte offsets)
[x] KERNEL_STACKS     (static [[u8; 8192]; 32] in BSS, AtomicUsize allocation)
[x] alloc_kernel_stack()
[x] ProcessControlBlock
      — TaskContext, kernel_stack_id, kernel_rsp, page_table_base,
        time_slice, cpu_time, priority, mailbox
      — NEW: memory_quota_pages, cpu_budget_ticks, cpu_budget_used,
             ipc_rate_limit, ipc_rate_used, total_cpu_ticks, blocked_deadline
[x] ProcessControlBlock::new()  → Option<Self>
      — allocates kernel stack, writes entry_point to [kern_rsp]
      — all quota fields zero-initialised; blocked_deadline = u64::MAX
[x] ProcessTable      (fixed [Option<PCB>; 32])
[x] create_process / get_ready_processes
[x] get_ready_with_priority() → Vec<(ProcessId, u8)>  — for priority scheduler
[x] terminate_process — now RECLAIMS the table slot (Option set to None; PCB dropped)
[x] check_deadlines(tick) — unblocks processes whose blocked_deadline has elapsed
[x] reset_ipc_rate_counters() — clears ipc_rate_used each 100-tick window

[x] TSS.RSP0 update   (see §4 — tick_scheduler() updates RSP0 on every context switch)
[~] Per-process PML4  (see §3 — page_table_base threaded through; all processes share kernel PML4 today)
[~] Resource reclaim on terminate
      — PCB slot is freed (table slot → None); kernel stack frames not yet returned
        to physical allocator (deferred to free-list allocator)
[ ] Guard page per kernel stack   (see §3)
[ ] Capability table in PCB
      — fixed array of N capability slots (e.g., 64)
      — entry: { type: Cap, object_id: u32, rights: u8 }
[x] Process quota fields
      — memory_quota_pages: u32 (0 = unlimited)
      — cpu_budget_ticks: u32  (temporal partitioning; 0 = unlimited)
      — ipc_rate_limit: u16    (max IPC sends per 100-tick window; 0 = unlimited)
[ ] Ring-3 entry
      — set up user-space RSP from ELF initial stack
      — iretq to ring-3 CS with RFLAGS.IF=1
[ ] ELF loader        (covered in Part II — shell/init loads ELF images)
```

---

### 7  Scheduling

Decides which process runs next; enforces time isolation between processes.

```
[x] Scheduler struct  (RefCell<ProcessTable>, current_process, queue_index, audit, tick)
[x] add_process(entry, stack, pml4) / schedule() / current_process()
[x] set_priority(pid, u8) — change process priority at runtime
[x] set_quotas(pid, memory_pages, cpu_budget, ipc_rate) — apply all resource limits
[x] timer_tick()
      — increments internal tick; unblocks deadline-expired processes via check_deadlines
      — resets IPC rate counters every 100 ticks
      — advances cpu_time and cpu_budget_used; preempts when quantum OR budget expires
      — returns (*mut TaskContext, *const TaskContext, u64 pml4) for arch layer
[x] Priority-based scheduler
      — pick_next_priority(): selects lowest priority number (0 = highest) among Ready
      — round-robin within the same priority level
[x] IPC timeout on blocking_receive(pid, timeout_ticks)
      — stores blocked_deadline = tick + timeout in PCB
      — timer_tick calls check_deadlines() to unblock timed-out processes
[x] Temporal partitioning (cpu_budget_ticks per process)
      — process is preempted when cpu_budget_used >= cpu_budget_ticks
      — budget reset at start of next frame (TODO: frame reset hook)
[x] CPU time accounting  — pcb.total_cpu_ticks incremented every tick; readable via cpu_time_for(pid)
[x] Kernel invariant assertions (#[cfg(debug_assertions)] check_invariants())
[x] send_message(from_pid, to_pid, msg) — stamps sender PID, enforces rate limit, audits
[x] blocking_receive(pid, timeout) — dequeues or blocks with deadline; audits
[x] terminate_process(pid) — reclaims slot; audits
[x] audit_entries() → Vec<AuditEntry>  — IPC audit log readable at runtime

[~] Preemptive scheduling from timer ISR
      — tick_scheduler() called after hlt in shell/idle loop (deferred preemption)
      — true ISR-level preemption (save full GPR frame inside timer ISR) not yet done
[x] Global scheduler (GLOBAL_SCHEDULER static)
      — init_global(sched) in main.rs; get_global() used by syscall dispatcher
      — CURRENT_PID AtomicU32 updated on every tick_scheduler() context switch
[x] Idle process (priority 255, hlt loop)
      — registered via add_process(idle_process as *const () as u64, ...)
      — set_priority(idle_pid, 255); runs only when no other Ready process exists
[ ] Priority inheritance protocol
      — when a high-priority process blocks on IPC waiting for a low-priority server,
        the server temporarily inherits the caller's priority
      — prevents priority inversion (required by IEC 61508 for SIL 3/4)
[ ] Deadline-based scheduling hook
      — pcb.deadline: u64, pcb.period: u64  — structure for future EDF/RMA
```

---

### 8  IPC — Inter-Process Communication

The only legal channel between processes.  Every kernel-bypass communication
is a security and safety violation.

```
[x] Message           (Copy+Clone, sender: ProcessId, data: [u64; 8])
[x] MessageQueue      (circular buffer, capacity 16, head/tail/count)
[x] send / receive / is_empty / is_full
[x] Per-process mailbox in PCB  (mailbox: MessageQueue)
[x] Scheduler integration       (send unblocks; receive parks)

[x] Kernel stamps Message.sender = actual calling PID in send_message()
      — user-space cannot forge the sender; kernel overwrites before enqueue
[x] IPC message rate limiting
      — per-PCB ipc_rate_limit (u16, msgs/100-tick window)
      — send_message() returns false and drops the message if limit exceeded
      — ipc_rate_used reset by reset_ipc_rate_counters() every 100 ticks
[x] Notification / signal object  (Notification struct in ipc/message.rs)
      — MessageQueue.notify(word) ORs bits into pending_notification
      — poll_notification() atomically consumes the pending word
      — seL4 Notification / QNX pulse equivalent
[x] IPC audit log  (64-entry ring buffer in Scheduler.audit)
      — records Send / Receive / Block / Unblock / Terminate events
      — each entry: tick, kind, sender u32, target u32
      — audit_entries() returns a snapshot Vec for shell inspection
[ ] Capability-based endpoints   (see §6 — replaces raw-PID addressing)
[ ] Synchronous call/reply primitive
      — send_and_receive(endpoint, msg, reply_buf) — caller blocks until reply
[ ] Bulk data transfer (shared memory region)
      — for large payloads (> 64 bytes) IPC should map a shared frame, not copy
```

---

### 9  System Calls

The hardware boundary between ring 3 and ring 0.

```
[x] EFER.SCE = 1                   — System Call Extensions enabled
[x] STAR MSR                       — ring-0 CS=0x08, ring-3 base=0x10
[x] LSTAR MSR                      — points to syscall_entry
[x] SFMASK MSR                     — clears IF and DF on entry
[x] syscall_entry naked stub
      — saves callee-saved + rcx/r11; dispatches to dispatch_syscall()
[x] dispatch_syscall() in Rust — match rax to syscall table
[x] SYS_YIELD  (0) — voluntary preemption (returns 0; scheduler handles the rest)
[x] SYS_EXIT   (1) — logs exit code; full termination deferred to global SCHEDULER wire-up
[x] SYS_GETPID (2) — returns calling PID (stub; full impl needs per-CPU GSBASE)
[x] SYS_SEND   (3) — wired to Scheduler.send_message(); stamps sender PID; checks rate limit
[x] SYS_RECV   (4) — wired to Scheduler.blocking_receive(); returns u64::MAX if blocked (retry)
[x] SYS_NOTIFY (5) — wired to Scheduler.notify_process(); ORs word into pending_notification
[x] TSS.RSP0 stack switch — tick_scheduler() calls set_rsp0(kernel_rsp) before every switch_context()
[ ] sys_mmap / sys_munmap  — requires physical frame pool
[ ] sys_cap_grant          — requires capability table in PCB
[ ] Syscall argument validation  — pointer checks against user address range + PTE flags
```

---

### 10  Timer

Drives the scheduler heartbeat and provides time to user space.

```
[x] PIT channel 0 at 100 Hz   (divisor 11931)
[x] 8259 PIC master + slave init, IRQ0 unmasked
[x] TICK_COUNT  (AtomicU64, incremented at 100 Hz by timer ISR)

[ ] Replace 8259 PIC with LAPIC   (see §4)
[ ] LAPIC one-shot timer for per-process deadline wakeup
      — precision: TSC-derived, sub-millisecond
      — needed for IPC timeout and temporal partitioning
[ ] HPET init                  — high-resolution event timer, monotonic clock source
[ ] sys_clock_gettime()        — expose monotonic + wall-clock time to user space
[ ] Timer deadline API         — kernel sets LAPIC to fire at absolute tick; used by scheduler
[ ] Calibrate TSC against HPET at boot  — for consistent nanosecond timestamps
```

---

### 11  Kernel Safety & Integrity

These items are required by IEC 61508 SIL 4 / ISO 26262 ASIL D.
None of them add features — they make the existing features safe enough to certify.

```
[ ] Hardware watchdog integration
      — init: configure watchdog timeout (e.g., 100 ms) in hal/watchdog.rs
      — idle process (§7): kick watchdog on every hlt iteration
      — if scheduler or ISR stops running, system resets to safe state
[x] Kernel invariant assertions
      — check_invariants() in Scheduler (debug_assertions only)
      — verifies PIDs in ready queue are in expected range
      — additional assertions at PCB creation (stack alignment, entry point)
[ ] Persistent crash log
      — ErrorRecord { error_code, tick, pid, rip, context[64] } — 96 bytes per record
      — ring buffer of 16 records in a reserved physical region that survives warm-reset
      — printed at boot before being overwritten
[ ] Health monitor notification path
      — #GP/#PF in user space → terminate process + IPC message to PID 1 health monitor
      — replaces current unconditional halt
[ ] Kernel .text read-only mapping  (see §3 — CR0.WP + PTE without WRITABLE)
[ ] Stack canaries on non-naked Rust frames  (RUSTFLAGS=-Z sanitize=shadow-call-stack)
[x] CPU feature checks at boot
      — check_cpu_features() in main.rs: verifies SMEP and MSR support via CpuFeatures
      — halts with diagnostic message if any required feature is absent
[ ] Single-core enforcement
      — read CPUID leaf 1 EBX bits[23:16]; park additional cores with SIPI → hlt loop
      — documents and enforces the single-core assumption that Relaxed atomics rely on
[x] ECC machine check handler     (see §5 — #MC IST3, vector 18, logs + halts)
[x] All 256 IDT vectors handled   (see §5 — catch-all stubs for all unregistered vectors)
[ ] Reproducible build
      — SOURCE_DATE_EPOCH + --remap-path-prefix in scripts/build.sh
      — CI: build twice, diff output binaries, assert identical
[x] OOM safe-state handler
      — BumpAllocator::alloc() logs "heap exhausted" over serial then halts
      — alloc_error_handler attribute not yet stable; OOM caught at allocation site
```

---

### 12  Formal Verification & Testing

```
[ ] Unit tests — core-kernel crate  (no arch deps; runs on host with cargo test)
      [ ] memory/paging.rs   — map_page all levels, reuse existing table,
                               translate present/absent at each level, alloc failure
      [ ] memory/physical.rs — allocate, exhaust, OOM path
      [ ] process/table.rs   — create 32, create 33rd (fails), terminate + recycle
      [ ] scheduler          — timer_tick quantum expiry, priority selection,
                               blocking_receive park/unpark, send_message unblock
      [ ] ipc/message.rs     — queue full, queue empty, wrap-around, forged sender rejected
[ ] Branch coverage ≥ MC/DC for all kernel modules  (cargo llvm-cov)
[ ] System tests in QEMU (automated serial-capture harness)
      [ ] Boot sequence completes without panic
      [ ] Timer ISR fires at 100 Hz (verify TICK_COUNT via shell)
      [ ] Process fault (#PF) → health monitor notified, system continues
      [ ] IPC timeout fires after deadline
      [ ] Memory exhaustion returns ENOMEM, does not panic
[ ] Fault injection test mode  (feature = "fault-injection" build flag)
      [ ] sys_inject_fault(vector, error_code) — triggers any exception
      [ ] verify every exception handler path
[ ] Formal kernel invariants  (TLA+ or Alloy model — separate repository)
      [ ] Scheduler liveness: a Ready process is eventually scheduled
      [ ] IPC safety: only the holder of an endpoint capability can send to it
      [ ] Memory safety: no two virtual addresses in different processes map to the same
          physical frame unless the kernel explicitly created a shared mapping
[ ] Requirements traceability matrix
      — REQ-MEM-001 → paging.rs → test_paging_map — as a structured document
```

---

## Part II — User Space

Everything below runs in ring 3 and communicates with the kernel only through
the six core system calls (§9).  A kernel bug cannot be caused by any code
in this section.

---

### 13  Init Process  (PID 1 — Health Monitor)

The first user-space process; owns the system lifecycle.

```
[ ] Launch at kernel boot as PID 1
[ ] Receive fault notifications from kernel via IPC  (§5 user/kernel fault distinction)
[ ] Process restart policy
      — configurable: restart / escalate / ignore per process name
[ ] System-level safe-state transition
      — ordered shutdown when a critical process fails unrecoverably
[ ] Heartbeat from every registered process
      — process sends heartbeat IPC every N ms; init restarts if missed
[ ] Expose boot log over IPC to diagnostic clients
[ ] Service registry
      — name → endpoint capability  (replaces raw PID lookups)
      — processes register their endpoint; clients look up by name
```

---

### 14  Shell  (already partially done)

Interactive diagnostic interface over serial.

```
[x] Interactive UART read loop
[x] In-place line editing (insert/delete at cursor, Home/End)
[x] VT100/xterm escape sequence parser (arrow keys, Delete)
[x] Command history (32 entries, circular, skips duplicates)
[x] Tab completion
[x] echo, help, clear, halt, history commands

[ ] Migrate shell to ring-3 process
      — currently runs in efi_main (ring 0); must become a user-space process
      — communicates with kernel via syscalls only
[ ] ps command      — list processes, state, CPU ticks, priority
[ ] kill <pid>      — send terminate signal to a process
[ ] mem command     — show physical allocator state, per-process page count
[ ] ipc command     — show IPC queue depths
[ ] log command     — dump crash log from persistent region
[ ] load <path>     — load and launch an ELF binary from VFS
[ ] export TICK_COUNT display  (e.g., `uptime` command)
```

---

### 15  ELF Loader

Parses ELF64 images and launches them as new ring-3 processes.

```
[ ] ELF64 header validation  (magic, class=64, machine=x86_64, type=ET_EXEC/ET_DYN)
[ ] Program header walk       (PT_LOAD segments → map into per-process PML4)
[ ] PT_LOAD mapping
      — allocate physical frames; map at ELF vaddr with correct flags (R/W/X)
[ ] Initial user stack        (allocate + map; place argv/envp per System V ABI)
[ ] Entry point extraction    (e_entry from ELF header → PCB context.rip)
[ ] Dynamic linking stub      (initially: require static ELF only; dynamic = future)
[ ] sys_exec(path, argv)      — syscall wrapper that the shell and init use
```

---

### 16  VFS Server  (Virtual Filesystem)

The single namespace for all storage objects.  Runs as a userspace server.

```
[ ] VFS server process        — listens on a well-known IPC endpoint
[ ] Mount table               — (device_id, fs_driver_pid, mount_point)
[ ] File descriptor table     — per-process, managed by VFS server
[ ] sys_open(path, flags)     — returns fd capability
[ ] sys_read(fd, buf, len)
[ ] sys_write(fd, buf, len)
[ ] sys_close(fd)
[ ] sys_stat(path, stat_buf)
[ ] FAT32 driver process
      — reads boot partition sectors via block device driver IPC
      — minimal implementation: read-only; sufficient to load ELF binaries from disk
[ ] ramfs / initrd
      — in-memory filesystem loaded at boot from an embedded image
      — allows userspace to start without any disk driver
```

---

### 17  Device Drivers  (userspace servers)

All drivers run in ring 3.  A driver crash cannot take down the kernel.

```
[ ] Driver model
      — drivers register with init; receive IRQ notifications via IPC
      — kernel forwards hardware IRQs to registered driver processes
[ ] UART driver process   (wraps hal::uart; exposes read/write over IPC)
[ ] Block device driver   (ATA PIO or virtio-blk for QEMU)
[ ] GOP framebuffer driver
      — maps framebuffer physical address via sys_mmap
      — exposes a blit / fill / draw-text IPC interface
[ ] PS/2 keyboard driver  (or USB HID via xHCI — long term)
[ ] virtio-net driver     (QEMU networking; long term)
[ ] PCI bus enumeration   (scan, read config space, allocate BARs)
```

---

### 18  GOP Framebuffer Console

Visible output in the QEMU window (currently blank — serial only).

```
[ ] Map GOP framebuffer into display driver address space
[ ] PSF2 bitmap font (PC Screen Font — compact, public domain)
[ ] Text renderer     (glyph blit, cursor, scroll)
[ ] Terminal emulator (subset of VT100 — enough for the shell)
[ ] Panic screen      (kernel writes directly to framebuffer on fatal error,
                       bypassing the driver server)
```

---

### 19  Network Stack  (long term)

```
[ ] virtio-net driver    (see §17)
[ ] Ethernet frame TX/RX
[ ] ARP
[ ] IPv4 / ICMPv4
[ ] UDP
[ ] TCP  (minimal — connection setup/teardown + stream)
[ ] BSD-style socket API  (sys_socket, sys_bind, sys_connect, sys_send, sys_recv)
```

---

### 20  POSIX Compatibility Layer  (long term)

Thin library that maps POSIX calls onto Rost syscalls.
Runs entirely in user space; nothing in ring 0.

```
[ ] libc subset  (malloc via sys_mmap, free, memcpy, string.h)
[ ] pthread subset  (threads within a process share address space — requires kernel TLS)
[ ] POSIX signals  (mapped to IPC notifications from init)
[ ] fork / exec  (fork = copy address space; exec = ELF loader)
[ ] File I/O  (wraps VFS server IPC)
```

---

## Dependency Order (critical path)

The items below are hard prerequisites — nothing above them on this chain can
be done correctly without them.

```
ExitBootServices()
  └─ Physical frame tracker  (know what memory is free to own)
       └─ Free-list allocator  (can reclaim memory)
            └─ [x] CR3 loaded  (KERNEL_PML4 identity-maps all UEFI regions; NXE+WP+SMEP+SMAP active)
                 └─ [~] Per-process PML4  (plumbing done; per-process allocation deferred to ELF loader)
                      └─ [x] TSS loaded + IST stacks configured
                           └─ [~] TSS.RSP0 per-switch update  ← NEXT (ring-3 entry prerequisite)
                                └─ User/kernel fault distinction → Health monitor notify path
                                     └─ ELF loader + ring-3 entry
                           └─ User/kernel fault distinction  (#GP/#PF handlers)
                                └─ Health monitor (PID 1) receives fault IPC
                                     ├─ Preemptive timer ISR  (full GPR save → switch)
                                     │    └─ Priority scheduler  (WCET-bounded)
                                     │         └─ Temporal partitioning
                                     └─ Capability-based IPC endpoints
                                          └─ Syscall dispatcher (real sys_send/recv)
                                               └─ ELF loader  → ring-3 processes
                                                    └─ VFS + drivers + shell in ring-3
```
