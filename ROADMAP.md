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

Build modes:
- Default (dev/QEMU) — `cargo build --target x86_64-unknown-uefi -p kernel`
- Safety-mode (production / SIL-4) — append `--features safety-mode`
  Enables: Secure Boot = Enabled enforcement (halt if not Enabled)

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
[x] Serial console    (COM1, 115 200 baud 8N1 via port I/O — works before any driver)
[x] Firmware info     (vendor string, UEFI revision)
[x] CPUID collection  (vendor, brand, family/model/stepping, address bits, feature flags)
[x] Physical memory map  (all UEFI MemoryType regions → MemoryKind)
[x] GOP framebuffer info  (base, size, resolution, stride, pixel format — up to 4 outputs)
[x] ACPI RSDP address   (v1 fallback, v2 preferred)
[x] SMBIOS entry point  (32-bit v2 and 64-bit v3)
[x] Secure Boot state   (Enabled / Disabled / SetupMode / Unknown)
[x] Boot-time wall clock
[x] Kernel command-line (load options, UCS-2 → ASCII)
[x] Call ExitBootServices() — take exclusive hardware ownership, end UEFI involvement
      — called in efi_main after collect(); consumes SystemTable<Boot> so no boot service
        call can be issued after it; satisfies IEC 61508 §7.2.1 hardware exclusivity
[x] Validate Secure Boot state; halt or warn when not Enabled (safety-mode build flag)
      — check_cpu_features() checks boot_info.secure_boot; warns in dev builds (default)
      — halts with FATAL in --features safety-mode builds (IEC 61508 integrity chain-of-trust)
[x] Parse ACPI MADT — discover Local APIC and I/O APIC addresses
      — core_kernel::acpi module: find_madt(rsdp_phys) → Option<u64>
      — Traverses XSDT (ACPI ≥ 2.0, 64-bit ptrs) or RSDT (v1, 32-bit ptrs)
      — parse_madt(madt_phys) → Option<MadtInfo>:
          LocalApic (acpi_id, apic_id, enabled), IoApic (id, address, gsi_base),
          IrqOverride (irq, gsi, flags); irq_to_gsi() resolves ISA IRQ→GSI
      — LAPIC_PHYS_ADDR static in main.rs stores the discovered LAPIC address
      — Printed at boot: lapic_addr, lapic count, I/O APIC count, IRQ 0 override
      — All field reads use byte-offset helpers (no packed struct references)
      — 6 unit tests in acpi::madt::tests (pass/bad-checksum/bad-sig/identity/truncated)
[x] Parse ACPI DMAR — discover IOMMU (Intel VT-d) units
      — core_kernel::acpi::dmar module: find_dmar(rsdp_phys) → Option<u64>
      — parse_dmar(dmar_phys) → Option<DmarInfo>:
          DrhUnit { flags, segment, pci_segment, register_base }; covers_all_pci()
          DmarInfo.haw() = host_address_width + 1 (actual physical address width)
          DmarInfo.intr_remap_supported() checks DMAR FLAGS bit 0 (INTR_REMAP)
          DmarInfo.iter_units() iterates valid DrhUnit entries
      — Only DRHD (type 0) entries decoded; RMRR/ATSR/RHSA/ANDD/SATC skipped silently
      — IOMMU_BASES/IOMMU_COUNT statics in main.rs store discovered controller addresses
      — Printed at boot: unit count, HAW, intr_remap flag, each unit's MMIO base
      — find_dmar shares the generic find_table(rsdp, b"DMAR") helper with MADT/FADT
      — IEC 61508 §7.4.3: IOMMU discovery is prerequisite for DMA confinement (§4)
      — 9 unit tests in acpi::dmar::tests
[x] Parse ACPI FADT — discover PM timer I/O port, RESET_REG
      — core_kernel::acpi::fadt module: find_fadt(rsdp_phys) → Option<u64>
      — parse_fadt(fadt_phys) → Option<FadtInfo>:
          pm_timer_port (I/O port, 3.579545 MHz), pm_timer_len,
          reset_reg_space / reset_reg_addr / reset_value (GAS fields)
      — pm_timer_valid(): port ≠ 0 && len == 4
      — reset_supported(): FLAGS bit 10 (RESET_REG_SUP) && addr ≠ 0
      — Only populated for ACPI ≥ 2.0 tables (revision ≥ 2 + FADT ≥ 129 bytes)
      — find_madt / find_fadt share a generic find_table(rsdp, sig) helper
      — PM_TIMER_PORT static in main.rs stores the discovered port
      — Printed at boot: pm_timer=0x<port> or absent; reset_reg or absent
      — IEC 61508 §7.4.9: reset register = second safe-state path alongside watchdog
      — 9 unit tests in acpi::fadt::tests
```

---

### 2  Physical Memory Management

Owns every byte of RAM from the moment ExitBootServices is called.

```
[x] Bump allocator    (4 KB-aligned; seeded from largest usable UEFI region)
[x] Allocate          (rounds up to 4 KB, decrements heap_remaining)
[x] Global physical bump allocator
      — init_global_allocator(base, size) published during stage-1 init
      — global_alloc_4k() returns next free 4 KB frame; used by SYS_MAP and ELF loader
      — KERNEL_PML4_PHYS (AtomicU64) recorded at stage-1; read by SYS_SPAWN / SYS_MAP
[x] Free-list / slab allocator
      — IEC 61508 §7.4.5: safety-critical software must reclaim resources from
        terminated processes; a pure bump allocator cannot satisfy this requirement
      — FREE_BITMAP: [u64; 1024] = 65 536 bits covering 256 MB (one bit per 4 KB frame)
      — init_global_allocator() now zeroes FREE_BITMAP + FRAME_TAGS and marks all
        frames in [base, base+size) as free (bit=1) before returning
      — global_alloc_4k(): bitmap_find_free() → CTZ scan O(N/64), clear bit, tag KernelData
      — global_free_4k(phys): set bit, retag Free; silently ignored for out-of-range addrs
      — Both functions exported from core_kernel::memory
      — terminate_process prerequisite met; full PCB reclaim (page-table walk) is future work
      — Bitmap tests consolidated into test_frame_tracker (avoids static-mut data races)
[x] Per-type object pools
      — fixed-size pools so the allocator is never called on the hot (timer-ISR) path
      — PageTable frame pool (memory/pool.rs):
          PT_POOL: [u64; 512] BSS array = 2 MB of pre-allocated PT frames
          pool_init(n): draws n frames from global bitmap allocator at boot O(N)
          pool_alloc_pt(): O(1) LIFO pop; falls back to global_alloc_4k() on miss
          pool_free_pt(phys): O(1) LIFO push; spills to global_free_4k() if full
          map_page_global() and split_huge_page_global() use pool first, bitmap fallback
          Printed at boot: "pt_pool: filled=N/512"
      — PCB pool: ProcessTable [Option<PCB>; 32] is already a fixed-size pool ✓
      — Kernel-stack slot reclaim pool (pcb.rs):
          STACK_RECLAIM: [u8; 32] + STACK_RECLAIM_TOP: usize — LIFO free list
          free_kernel_stack(id): zeroes usable stack, pushes id onto reclaim pool
            (IEC 61508 §7.4.3: terminated process data must not be visible to successor)
          alloc_kernel_stack(): checks reclaim pool first, then NEXT_STACK counter
          terminate_process() now calls free_kernel_stack(pcb.kernel_stack_id)
            — closes the resource-reclaim loop for kernel stacks
      — Channel objects: MessageQueue is inline in PCB (no heap); no separate pool needed
      — 4 pool unit tests (pool.rs + pcb.rs)
[~] OOM handler (alloc_error_handler)
      — #[alloc_error_handler] is nightly-only (stable Rust 1.93: E0658)
      — BumpAllocator halts inline on exhaustion; panic_handler halts on alloc null
      — acceptable for SIL-4: OOM always reaches a defined safe state (halt)
      — full override requires nightly toolchain or future stabilisation
[x] Physical frame tracker
      — FRAME_TAGS: [u8; 65536] BSS array covers 256 MB of allocatable space (64 KB static)
      — FrameKind: Free(0) / KernelData(1) / UserOwned(2) / Guard(3) / Mmio(4)
      — global_alloc_4k() auto-tags allocated frames as KernelData
      — ELF loader and SYS_MAP reclassify user-process pages as UserOwned
      — install_kernel_stack_guard_pages() tags guard slots as Guard
      — frame_stats() returns [free, kdata, user, guard, mmio] — emitted at boot
[x] Persistent crash log
      — fixed phys 0x0000_4000 (conventional memory below EBDA, survives warm reset)
      — 64-byte header (HEADER_MAGIC) + 16 × 64-byte ErrorRecord ring buffer
      — ErrorRecord: magic, vector, tick, pid, rip, rflags, cr2
      — drain() callback pattern keeps core-kernel hal-free
      — crash_log::write() called from #GP / #PF / #DF / #MC handlers
```

---

### 3  Virtual Memory & Paging

Provides isolated virtual address spaces; the mechanism by which the kernel
enforces spatial isolation between processes.

```
[x] PageTable struct          (#[repr(C, align(4096))], 512 × u64 entries)
[x] 4-level walk: PML4→PDPT→PD→PT
[x] map_page(pml4, virt, phys, flags, alloc)
      — flags: u64 (PTE_PRESENT | PTE_WRITABLE | PTE_USER | PTE_NO_EXECUTE)
      — allocates missing intermediate tables from caller-supplied allocator
[x] map_page_global(pml4, virt, phys, flags)
      — variant that draws intermediate tables from the global bump allocator
      — used by SYS_MAP and the ELF loader; no PhysicalAllocator argument required
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
[x] CR4.SMAP = 1   — fully guarded
      — syscall_entry switches to kernel stack before any push (no PTE_USER touch)
      — dispatch_syscall brackets all user-buffer accesses with STAC/CLAC via SmapGuard
[x] CR3 reload on context switch
      — switch_context(old, new, new_pml4) — third arg triggers mov cr3,rdx when non-zero
      — kernel processes pass kernel_pml4_phys; test rdx/jz skips redundant TLB flush
[x] PTE_NO_EXECUTE applied to data pages
      — PTE_NO_EXECUTE (bit 63) set on every .rdata, .data, and .bss 4 KB page
      — implemented via remap_page_flags() called by apply_section_flags() at boot
[x] Per-process PML4
      — page_table_base threaded through create_process → PCB → timer_tick → switch_context
      — ELF loader (§15) creates a fresh PML4 per loaded binary
      — processes created by SYS_SPAWN can pass an explicit pml4 or inherit kernel PML4
      — kernel page tables merged into each user PML4 via merge_kernel_into_user_pml4()
        so that syscall handlers can access kernel code/data/stacks when CR3=user PML4
[x] Kernel guard pages
      — KERNEL_STACKS layout: 12 KB slots (4 KB guard at offset 0 + 8 KB usable)
      — split_huge_page_global() converts 2 MB PD entry → 512 × 4 KB PT entries
      — unmap_page() clears guard slot → stack overflow → #PF, not silent corruption
      — install_kernel_stack_guard_pages() called before activate_page_table (no invlpg needed)
[x] Kernel .text mapped read-only
      — pecoff.rs parses PE32+ section table from LoadedImage.info() base address
          (DOS header → e_lfanew → PE sig → COFF → optional header → section table)
      — boot_collector captures image_base + image_size into BootInfo before ExitBootServices
      — remap_kernel_sections() called in Stage 1 BEFORE activate_page_table():
          .text   → PTE_PRESENT only (read + execute; PTE_WRITABLE cleared)
          .rdata  → PTE_PRESENT | PTE_NO_EXECUTE (read-only)
          .data   → PTE_PRESENT | PTE_WRITABLE | PTE_NO_EXECUTE
          .bss    → PTE_PRESENT | PTE_WRITABLE | PTE_NO_EXECUTE
      — apply_section_flags(): splits all 2 MB pages spanning the section, then
          calls remap_page_flags() on each 4 KB page within the section range
      — CR3 reload by activate_page_table() flushes TLB; no invlpg needed
      — IEC 61508 §7.4.3: kernel code segment is read-only; write → #PF (CR0.WP)
[x] TLB shootdown stub  (single-core: invlpg; SMP: IPI path reserved)
      — invlpg(vaddr) inline asm in arch-x86_64/src/cpu/mod.rs
      — called after unmap_page / remap operations while CR3 is active
[x] Huge pages for kernel .text / .data split  — NX on data, X on .text only
      — remap_page_flags(pml4, virt, new_flags): walks PML4→PDPT→PD→PT, preserves
          physical address, replaces all flag bits in the PT entry; returns false
          if any level absent or if PD entry is still a huge page
      — split_huge_page_global() prerequisite enforced: apply_section_flags()
          splits every 2 MB page before calling remap_page_flags() on sub-pages
      — 4 unit tests: preserves-phys, absent-pml4, absent-pt-entry, rejects-huge-page
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

[x] Task State Segment (TSS)
      — tss.rs: TaskStateSegment (#[repr(C,packed)], 104 bytes), IST1/IST2/IST3 stacks
      — GDT extended to 7 entries; 16-byte system descriptor at slots 5/6 (selector 0x28)
      — init_tss() fills IST stacks; install_tss() encodes the TSS descriptor into GDT
      — ltr 0x28 loads the Task Register after GDT.load()
[x] TSS.RSP0 update on every context switch
      — tick_scheduler() / tick_scheduler_isr() call set_rsp0(kernel_rsp) before switch
      — kernel_rsp is the 4th element returned by timer_tick()
[x] CR0.WP = 1           (init_protection — see §3)
[x] CR4.SMEP + CR4.SMAP  (init_protection — see §3)
[x] EFER.NXE             (init_protection — see §3)
[x] CR4.FSGSBASE = 1  — enables rdfsbase/wrfsbase/rdgsbase/wrgsbase in ring-3
      — set in init_protection() alongside SMEP (single CR4 write, bit 16)
[x] Local APIC init
      — crates/arch-x86_64/src/apic/lapic.rs: SVR (bit 8 SW-enable, spurious 0xFF),
        TPR=0, LINT0/LINT1/Error LVT masked, PIT channel 2 calibration (10 ms poll),
        LAPIC timer periodic 100 Hz vector 32, LAPIC_EOI_ADDR set, 8259 PIC masked
      — timer ISR sends LAPIC EOI via LAPIC_EOI_ADDR atomic after PIC EOI
      — LAPIC MMIO page explicitly mapped before lapic::init() in efi_main Stage 3:
          map_page_global(KERNEL_PML4, lapic_base, lapic_base, PRESENT|WRITABLE)
          guards against LAPIC MMIO falling outside the UEFI free-memory map range
      — 3 unit tests: svr_value, lvt_timer_periodic, svr_enable_bit_independent_of_vector
[x] I/O APIC init   (from MADT; route IRQs through I/O APIC, not 8259)
      — crates/arch-x86_64/src/apic/ioapic.rs: reads IOAPICVER, masks all redirect
        table entries; route_irq() unmaskes individual GSIs on demand
      — I/O APIC MMIO page explicitly mapped before ioapic::init() (same pattern as LAPIC)
      — IOAPIC_PHYS_ADDR / IOAPIC_GSI_BASE statics set from MADT primary_io_apic()
      — 4 unit tests: redirect_entry_vector, redirect_entry_apic_id,
        redirect_entry_unmasked, masked_entry
[x] IOMMU (Intel VT-d) init
      — crates/core-kernel/src/iommu/mod.rs: checks ECAP.PT, allocates 4 KB root +
        context tables, fills 256 passthrough entries (TT=10b, domain_id=1),
        programs RTADDR, issues SRTP+WBF+TE with GSTS polling
      — permissive passthrough mode (equivalent to no IOMMU security-wise but proves
        hardware enable path; per-device restriction is a future patch)
      — all IOMMU_BASES[..IOMMU_COUNT] MMIO pages explicitly mapped before iommu::init()
      — IEC 61508 §7.4.3: DMA remapping infrastructure installed at boot
      — 4 unit tests: root_entry_present, root_entry_alignment,
        passthrough_ctx_entry, passthrough_ctx_domain_id
```

---

### 5  Interrupt & Exception Handling

Handles all CPU exceptions and hardware interrupts.  A fault in one process
must never halt the system.

```
[x] #DE  (vector  0) — divide by zero     — ring-3: terminate process + notify init (PID 1)
                                           — ring-0: full register dump + halt
[x] #GP  (vector 13) — general protection — ring-3: terminate + notify; ring-0: dump + halt
[x] #PF  (vector 14) — page fault         — ring-3: terminate + notify; ring-0: CR2 + dump + halt
[x] IRQ0 (vector 32) — PIT timer 100 Hz
      — lock inc TICK_COUNT + EOI + call tick_scheduler_isr() + iretq
      — tick_scheduler_isr() calls switch_context_noints() if quantum expired
      — IRETQ restores RFLAGS (IF) from interrupted task's saved state
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
      — ring-3 faults: terminate_faulting_process() → IPC to PID 1 + tick_scheduler_isr()
      — ring-0 faults: full register dump + system halt
[x] Preemptive context switch from timer ISR
      — timer ISR: save caller-saved, inc TICK_COUNT, EOI, call tick_scheduler_isr, restore, iretq
      — tick_scheduler_isr() uses switch_context_noints (no sti) so IRETQ restores IF
      — switch_context_noints: CLI + callee-saved save/restore + optional CR3 load, no STI
[x] Spurious interrupt handler (vector 255) — iretq only, no EOI (LAPIC spurious)
[x] MAX_ISR_LATENCY (pub static AtomicU64) — placeholder for latency measurement
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
      — memory_quota_pages, memory_pages_used, cpu_budget_ticks, cpu_budget_used,
        ipc_rate_limit, ipc_rate_used, total_cpu_ticks, blocked_deadline
[x] ProcessControlBlock::new()  → Option<Self>
      — allocates kernel stack, writes entry_point to [kern_rsp]
      — all quota fields zero-initialised; blocked_deadline = u64::MAX
[x] ProcessTable      (fixed [Option<PCB>; 32])
[x] create_process / get_ready_processes
[x] get_ready_with_priority() → Vec<(ProcessId, u8)>  — for priority scheduler
[x] terminate_process — reclaims table slot (Option set to None; PCB dropped)
[x] check_deadlines(tick) — unblocks processes whose blocked_deadline has elapsed
[x] reset_ipc_rate_counters() — clears ipc_rate_used each 100-tick window

[x] TSS.RSP0 update   (see §4 — tick_scheduler_isr() updates RSP0 on every context switch)
[x] Per-process PML4  (page_table_base threaded through; ELF loader creates fresh PML4 per process;
                       kernel PD/PDPT/PML4 entries merged in so syscall handlers work with user CR3)
[~] Resource reclaim on terminate
      — PCB slot cleared (table slot → None); kernel stack zeroed + reclaimed ✓
      — global_free_4k() available for page-frame reclaim ✓
      — PCB does not yet track per-process page-table / user-stack frame addresses
      — Full reclaim requires a PML4 walk or a per-PCB frame list (future work)
[x] Guard page per kernel stack   (see §3 — split_huge_page_global + unmap_page)
[x] Capability table in PCB
      — CapKind enum: None/Channel/Process/Memory/Service; CAP_R/W/G/X rights bitmask
      — Capability { kind: CapKind, rights: u8, _pad: [u8;2], object_id: u32 } — 8 bytes
      — 64-slot array in PCB (CAP_TABLE_SIZE=64; 512 bytes per PCB; 16 KB total BSS)
      — cap_alloc / cap_find / cap_revoke / cap_grant in ProcessTable
      — cap_alloc / cap_grant / cap_revoke / cap_slot_rights wrappers in Scheduler
      — Scheduler::cap_slot_rights() used by SYS_CAP_GRANT to distinguish EPERM vs EINVAL
[x] Process quota fields
      — memory_quota_pages: u32    (0 = unlimited)
      — memory_pages_used:  u32    (pages mapped so far; checked + incremented on SYS_MAP / SYS_MAP_SHARE)
      — cpu_budget_ticks:   u32    (temporal partitioning; 0 = unlimited)
      — ipc_rate_limit:     u16    (max IPC sends per 100-tick window; 0 = unlimited)
[x] Ring-3 entry
      — user-space PML4 created by ELF loader; kernel pages merged via merge_kernel_into_user_pml4
      — ring3_entry_trampoline: builds IRETQ frame (RIP/CS=0x23/RFLAGS/RSP/SS=0x1B), iretq
      — 16 KB user stack allocated by spawn_elf at 0x7FFF_FFFF_F000 with PTE_USER|NX
[x] ELF loader        (see §15 — kernel/src/elf.rs; spawn_elf() wires it to the scheduler)
```

---

### 7  Scheduling

Decides which process runs next; enforces time isolation between processes.

```
[x] Scheduler struct  (RefCell<ProcessTable>, current_process, queue_index, audit, tick)
[x] add_process(entry, stack, pml4) / schedule() / current_process()
[x] set_priority(pid, u8) — change process priority at runtime
[x] set_quotas(pid, memory_pages, cpu_budget, ipc_rate) — apply all resource limits
[x] yield_current() — mark current process Ready and exhaust quantum (timer-deferred fallback)
[x] yield_switch()  — cooperative yield with immediate context switch (used by SYS_YIELD)
      — resets current process quantum to 0, marks Ready, picks next via pick_next_priority()
      — performs switch_context_noints immediately — no timer tick required
      — returns None if no other Ready process exists (caller continues running)
[x] get_process_pml4(pid) → Option<u64> — expose page_table_base (used by SYS_MAP)
[x] timer_tick()
      — increments internal tick; unblocks deadline-expired processes via check_deadlines
      — resets IPC rate counters every 100 ticks
      — advances cpu_time and cpu_budget_used; preempts when quantum OR budget expires
      — returns (*mut TaskContext, *const TaskContext, u64 pml4, u64 kernel_rsp)
[x] Priority-based scheduler
      — pick_next_priority(): selects lowest priority number (0 = highest) among Ready
      — round-robin within the same priority level
[x] IPC timeout on blocking_receive(pid, timeout_ticks)
      — stores blocked_deadline = tick + timeout in PCB
      — timer_tick calls check_deadlines() to unblock timed-out processes
      — timeout_ticks == 0: non-blocking poll — returns None immediately without blocking
[x] Temporal partitioning (cpu_budget_ticks per process)
      — process is preempted when cpu_budget_used >= cpu_budget_ticks
      — cpu_budget_used reset to 0 every CPU_BUDGET_FRAME_TICKS (1000 ticks = 10 s)
          by reset_cpu_budget_counters() called inside timer_tick()
      — IEC 61508 §7.4.1: budgets are window-relative; throttled processes re-admitted
          each frame instead of being permanently starved
[x] CPU time accounting  — pcb.total_cpu_ticks incremented every tick
[x] Kernel invariant assertions (#[cfg(debug_assertions)] check_invariants())
[x] send_message(from_pid, to_pid, msg) — stamps sender PID, enforces rate limit, audits
[x] blocking_receive(pid, timeout) — dequeues or blocks with deadline; audits
[x] terminate_process(pid) — reclaims slot; audits
[x] audit_entries() → Vec<AuditEntry>  — IPC audit log readable at runtime
[x] detect_deadlock(waiter, target) → bool
      — O(32) iterative DFS over waiting_for graph in ProcessTable::detect_cycle()
      — follows waiting_for links from target; returns true if waiter is reachable
      — visited[MAX_PROCESSES] bitmap prevents looping on existing cycles
      — called from SYS_CALL between send and block; returns EDEADLK (-8) on cycle
      — IEC 61508 §7.4.4: every blocking operation must have a bounded wait time;
          cycle detection provides a hard guarantee independent of application timeouts
[x] Memory quota enforcement (check_memory_quota / use_memory_page)
      — check_memory_quota(pid): read-only; returns false if memory_pages_used >= quota ≠ 0
      — use_memory_page(pid): increments memory_pages_used (saturating) after successful map
      — enforced in SYS_MAP and SYS_MAP_SHARE before any physical frame is allocated
      — IEC 61508 §7.4.5: physical memory footprint of each process is bounded

[x] Preemptive scheduling from timer ISR
      — tick_scheduler_isr() called directly from the timer ISR after EOI
      — calls switch_context_noints(old, new, pml4): CLI + save + restore, no STI
      — IRETQ in ISR stub restores RFLAGS (IF) — true preemption, not deferred
      — tick_scheduler() (cooperative, with STI) still used from shell idle loop
[x] Global scheduler (GLOBAL_SCHEDULER static)
      — init_global(sched) in main.rs; get_global() used by syscall dispatcher
      — CURRENT_PID AtomicU32 updated on every context switch
[x] Idle process (priority 255, hlt loop)
      — calls enable_interrupts() at entry (safe for both ISR and cooperative first-schedule)
      — runs only when no other Ready process exists
[x] Priority inheritance protocol
      — PCB.waiting_for: Option<ProcessId> set in send_message; cleared on unblock/timeout
      — pick_next_priority: get_blocked_waiters() builds donation table; effective priority =
        min(natural, best_donated) — servers inherit the highest priority of their blocked clients
      — prevents priority inversion (required by IEC 61508 for SIL 3/4)
[x] Deadline-based scheduling hook (EDF)
      — PCB: rt_period: u64 (0 = best-effort), rt_deadline: u64 (absolute tick)
      — set_realtime(pid, period_ticks): enables EDF for a process; initial deadline = now + period
      — pick_next_priority(): EDF tier first — RT processes preempt all priority-based processes;
        among RT processes, minimum rt_deadline wins (Earliest Deadline First)
      — period renewal in timer_tick(): rt_deadline += rt_period on expiry (no drift)
      — best-effort tier (rt_period == 0): existing priority + donation logic unchanged
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
[x] Notification / signal object
      — MessageQueue.notify(word) ORs bits into pending_notification
      — poll_notification() atomically consumes the pending word
[x] IPC audit log  (64-entry ring buffer in Scheduler.audit)
      — records Send / Receive / Block / Unblock / Terminate events
      — each entry: tick, kind, sender u32, target u32
      — audit_entries() returns a snapshot Vec for shell inspection
[x] Full 72-byte IPC message over SYS_RECV_MSG / SYS_SEND_MSG
      — 8 × u64 payload; user buffer pointer; kernel stamps sender PID
      — used by VFS server ↔ shell for filesystem IPC (OP_READDIR, OP_READ, OP_STAT, OP_MOUNT, OP_WRITE_OPEN, OP_WRITE_DATA, OP_WRITE_CLOSE, OP_MKDIR, OP_UNLINK)
[x] Capability-based endpoints   (see §6 — replaces raw-PID addressing)
      — SYS_CHAN_BIND (20): allocates CapKind::Channel slot pointing to target PID;
        caller holds a send token without needing to know the raw PID at send time
      — SYS_SEND_CAP (21): send 72-byte Message via Channel cap; kernel checks
        CapKind::Channel + CAP_W, extracts target PID from object_id, forwards;
        sender field overwritten with calling PID — still unforgeable
      — cap_slot_info() added to Scheduler: returns (kind, rights, object_id) for
        efficient right-check + target extraction in a single borrow
      — Security: a process that knows a PID but holds no Channel cap is rejected
        with EPERM by SYS_SEND_CAP; SYS_SEND_MSG (raw PID) is still available
        for backward-compatible kernel↔server IPC where both PIDs are known
[x] Synchronous call/reply primitive  (SYS_CALL #17)
      — send_message() + blocking_receive() atomically; single-core + IF=0 guarantees no race
      — ring-3 wrapper: syscall::call(to_pid, &req, &mut reply, timeout_ticks) → bool
[x] Bulk data transfer (shared memory region)
      — SYS_MAP_SHARE (22): allocates a zeroed 4 KB frame (FrameKind::UserOwned),
        maps it at the caller's requested vaddr with PTE_USER, and installs a
        CapKind::Memory slot with object_id = PFN (frame_phys >> 12); returns cap slot
      — SYS_MAP_CAP (23): receiver maps the same frame into its own address space
        using the Memory cap (after SYS_CAP_GRANT transfers it); kernel derives
        frame_phys = cap.object_id << 12; only CAP_W cap allows writable mapping
      — Shared memory flow: sender SYS_MAP_SHARE → SYS_CAP_GRANT → IPC notify;
        receiver SYS_MAP_CAP → accesses shared frame at its own VA
      — No additional physical allocation on receiver side; kernel enforces read/write
        rights from the capability at every map call
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
      — saves user RSP to SYSCALL_USER_RSP_SAVE (kernel static), switches to
        SYSCALL_KERN_RSP (mirrored from TSS.RSP0) before any push — SMAP-safe
      — saves callee-saved + rcx/r11 onto kernel stack; dispatches to dispatch_syscall()
      — restores user RSP from SYSCALL_USER_RSP_SAVE before SYSRETQ
[x] dispatch_syscall() in Rust — match rax to syscall table
      — SmapGuard: STAC on entry, CLAC on drop — brackets all user-buffer accesses
[x] SYS_YIELD  (0) — immediate cooperative context switch via Scheduler::yield_switch()
                     picks next Ready process and switches now (no timer tick wait)
                     arm_oneshot(1) called to keep LAPIC timer alive for deadline wakeups
                     returns None (no switch) if calling process is the only Ready one
[x] SYS_EXIT   (1) — terminates calling process via Scheduler::terminate_process()
[x] SYS_GETPID (2) — returns CURRENT_PID (AtomicU32, updated on every switch)
[x] SYS_SEND   (3) — wired to Scheduler.send_message(); stamps sender PID; checks rate limit
[x] SYS_RECV   (4) — wired to Scheduler.blocking_receive(); returns u64::MAX if blocked
                     timeout=0: non-blocking poll (returns immediately, no context switch)
                     on block: arm_oneshot(1) + switch_context_noints to keep timer alive
[x] SYS_NOTIFY (5) — wired to Scheduler.notify_process(); ORs word into pending_notification
[x] SYS_RECV_MSG (6) — receive full 72-byte Message into user buffer
                       on block: arm_oneshot(1) to re-arm LAPIC before switch_context_noints
[x] SYS_SEND_MSG (7) — send full 72-byte Message from user buffer; kernel stamps sender
[x] SYS_SPAWN    (8) — create a new ring-0 process
                       a0=entry_point, a1=pml4 (0=kernel PML4), a2=priority (0=default 128)
                       returns new PID or EINVAL
[x] SYS_MAP      (9) — map a 4 KB virtual page in the calling process's address space
                       a0=vaddr (4 KB aligned), a1=paddr (0=allocate), a2=flags (R/W/U)
                       checks memory_quota_pages before allocation; increments memory_pages_used on success
                       returns ENOMEM if quota exhausted (IEC 61508 §7.4.5)
                       uses global bump allocator for intermediate page-table nodes
[x] SYS_REGISTER (10) — register current PID under a ≤15-byte ASCII service name
[x] SYS_LOOKUP   (11) — return PID for a registered service name
[x] SYS_UART_WRITE (12) — write one byte to COM1; only meaningful to a privileged driver process
[x] SYS_UART_READ  (13) — non-blocking read from COM1; returns byte or u64::MAX if empty
[x] SYS_CLOCK     (14) — monotonic nanoseconds since boot; TICK_COUNT × 10,000,000 (100 Hz = 10 ms/tick)
                          shell `uptime` command now shows h/m/s/ms via this syscall
[x] SYS_SETPRIO   (15) — set process scheduling priority (a0=pid 0=self, a1=0–255)
[x] SYS_SETRT     (16) — assign EDF real-time period (a0=pid 0=self, a1=period_ticks; 0=disable)
                          directly calls Scheduler::set_realtime(); initial deadline = now + period
[x] SYS_CALL      (17) — synchronous call/reply IPC primitive
                          a0=to_pid, a1=send_buf, a2=reply_buf, a3=timeout_ticks (0=forever)
                          send_message() → detect_deadlock() → blocking_receive()
                          returns 0 on reply, ETIMEDOUT on timeout, EAGAIN if mailbox full
                          returns EDEADLK (-8) if cycle detected in waiting_for graph
                          ring-3 wrapper: syscall::call(to_pid, &req, &mut reply, timeout)
[x] Error codes extended: EAGAIN (-5), ETIMEDOUT (-6) added to dispatch_syscall
[x] dispatch_syscall a3 argument exposed (renamed from _a3); used by SYS_CALL timeout
[x] TSS.RSP0 + SYSCALL_KERN_RSP stack switch
      — tick_scheduler_isr() calls set_rsp0(kernel_rsp) before every context switch
      — set_rsp0 also updates SYSCALL_KERN_RSP (AtomicU64) used by syscall_entry
      — SYSCALL_USER_RSP_SAVE (AtomicU64) scratch-saves user RSP between stack switch instructions
[x] Syscall argument validation
      — USER_VA_END = 0x0000_8000_0000_0000 (canonical user-space ceiling, 4-level paging)
      — validate_user_ptr(ptr, size, align): rejects null, misaligned, overflowing, or
        out-of-user-space pointers; applied to every syscall that accepts a user pointer
      — validate_user_vaddr(vaddr): SYS_MAP virtual address check (user-space + 4KB aligned)
      — SYS_RECV_MSG (6): buffer ptr validated (72 bytes, 8-byte aligned, user space)
      — SYS_SEND_MSG (7): buffer ptr validated (72 bytes, 8-byte aligned, user space)
      — SYS_MAP      (9): virtual address validated via validate_user_vaddr; prevents
        mapping over kernel identity-mapped addresses
      — SYS_REGISTER (10): name ptr validated (16 bytes, 1-byte aligned, user space)
      — SYS_LOOKUP   (11): name ptr validated (16 bytes, 1-byte aligned, user space)
      — SMAP remains active as hardware backstop for unmapped pages that pass range checks
[x] SYS_CAP_GRANT  (18) — grant capability from caller's table to another process
                          a0=slot_idx, a1=to_pid
                          checks CAP_G right; returns new slot index on success
                          EPERM (missing CAP_G), EINVAL (bad slot/full table), ENOSYS
                          ring-3 wrapper: syscall::cap_grant(slot_idx, to_pid) → Result<usize,u64>
[x] SYS_SETQUOTA   (19) — set resource quotas for a process at runtime
                          a0=pid (0=self), a1=memory_pages, a2=cpu_budget_ticks, a3=ipc_rate
                          calls Scheduler::set_quotas(); all zero = unlimited
                          IEC 61508 §7.4.1 (temporal) + §7.4.5 (resource partitioning)
                          ring-3 wrapper: syscall::setquota(pid, pages, budget, rate)
[x] SYS_CHAN_BIND  (20) — create Channel capability pointing to a target PID
                          a0=target_pid, a1=rights (0=default CAP_R|CAP_W|CAP_G)
                          returns cap_slot index; EINVAL if pid=0 or cap table full
[x] SYS_SEND_CAP   (21) — send 72-byte Message via channel capability (not raw PID)
                          a0=cap_slot, a1=buf_ptr (72B, 8-aligned)
                          checks CapKind::Channel + CAP_W; extracts to_pid from object_id
                          EPERM if wrong kind/rights, EINVAL if bad buf or queue full
[x] SYS_MAP_SHARE  (22) — allocate shared 4 KB frame, map in caller's VAS, create Memory cap
                          a0=vaddr (4KB-aligned user space), a1=flags (bit0=writable)
                          allocates+zeroes frame, maps with PTE_USER, installs CapKind::Memory
                          object_id=PFN; returns cap_slot; ENOMEM if OOM or quota exhausted
                          checks memory_quota_pages before frame allocation;
                          increments memory_pages_used on success (IEC 61508 §7.4.5)
[x] SYS_MAP_CAP    (23) — map a shared frame via Memory capability
                          a0=vaddr (caller's VA), a1=cap_slot, a2=flags (bit0=writable)
                          derives frame_phys = cap.object_id<<12; EPERM if not Memory+CAP_R
[x] SYS_LOOKUP_CAP (24) — lookup service name → Channel capability (named endpoints)
                          a0=name ptr (16 bytes); returns cap slot index
                          creates CapKind::Channel (CAP_W) in caller's table
                          ENOENT if name not found; ENOMEM if cap table full
[x] SYS_SPAWN_ELF       (26) — load ring-3 ELF image from caller's user-space buffer
[x] SYS_RESTART_SERVER  (27) — restart a named server from the kernel's embedded ELF image
                                a0=ptr to 16-byte null-padded service name
                                kernel hook maps "uart-drv"/"rost-vfs"/"rost-shell" → ELF data
                                returns new PID; EINVAL if name unknown or table full
                                registered alongside SYS_SPAWN_ELF hook at Stage 6
                          a0=elf_data_ptr, a1=elf_data_len, a2=priority (0=default 128)
                          validates user pointer; reads ELF with STAC active
                          uses core_kernel::elf_spawn hook (avoids arch→kernel circular dep)
                          hook registered at Stage 6 before first server spawn
                          returns new PID on success; EINVAL on parse fail or table full
                          ring-3 wrapper: syscall::spawn_elf(buf, priority) → Option<u32>
```

---

### 10  Timer

Drives the scheduler heartbeat and provides time to user space.

```
[x] PIT channel 0 at 100 Hz   (divisor 11931)
[x] 8259 PIC master + slave init, IRQ0 unmasked
[x] TICK_COUNT  (AtomicU64, incremented at 100 Hz by timer ISR)

[x] Replace 8259 PIC with LAPIC   (see §4)
      — LAPIC sw-enabled, PIT calibrated 10 ms ICR, all 8259 lines masked
[x] LAPIC one-shot timer for per-process deadline wakeup
      — LAPIC_BASE / LAPIC_ICR_PER_TICK AtomicU64/AtomicU32 stored at init
      — arm_oneshot(n): ICR ← LAPIC_ICR_PER_TICK × n (no-op if LAPIC absent)
      — tick_scheduler_isr() calls arm_oneshot(ticks_until_next_event()) after each tick
[x] HPET init                  — high-resolution event timer, monotonic clock source
      — core_kernel::acpi::hpet: find_hpet() + parse_hpet() → HpetInfo
        (period_fs, base_address, timer_count, counter_64bit)
      — core_kernel::hpet: init() enables ENABLE_CNF; read_counter() / period_fs()
      — Stage 4 logs period and timer count; TSC calibration follows
[x] sys_clock_gettime()        — SYS_CLOCK (14): TICK_COUNT × 10,000,000 ns; 10 ms resolution
                                  ring-3 wrapper syscall::clock(); shell uptime uses it
[x] Timer deadline API         — kernel sets LAPIC to fire at absolute tick; used by scheduler
      — ProcessTable::earliest_blocked_deadline() → min(blocked_deadline) or MAX
      — Scheduler::ticks_until_next_event() → delta to nearest blocked deadline (min 1)
      — tick_scheduler_isr() programs LAPIC one-shot via arm_oneshot(ticks_until_next_event())
[x] Calibrate TSC against HPET at boot  — for consistent nanosecond timestamps
      — arch_x86_64::timer::tsc::calibrate(hpet_base, period_fs): 10 ms busy-wait, TSC_KHZ = delta/10
      — tsc::khz() / tsc::tsc_to_ns(delta) / tsc::compute_khz() / tsc::hpet_ticks_per_ms()
      — Stage 4 logs TSC kHz after HPET is enabled
```

---

### 11  Service Registry

Maps ASCII service names to PIDs so clients don't hard-code PIDs.

```
[x] core_kernel::service_registry module
      — static 16-entry table (name[16], pid, used flag); no heap required
      — register(name, pid) — add or overwrite entry; returns false if table full
      — lookup(name) → Option<u32>
      — unregister_pid(pid) — called on process termination
[x] SYS_REGISTER (10) — ring-3 servers call this after start-up
[x] SYS_LOOKUP   (11) — ring-3 clients call this to find a server PID
[x] Integrate with service lifecycle
      — terminate_process() calls service_registry::unregister_pid(pid) unconditionally
      — prevents dead PIDs remaining discoverable and table slots being leaked
[x] Named endpoint capabilities  — SYS_LOOKUP_CAP (24): looks up service name,
      allocates CapKind::Channel (CAP_W) in caller's cap table, returns slot index
      — ENOENT if name not found; ENOMEM if cap table full (64 slots)
      — caller uses SYS_SEND_CAP(slot, msg) — raw PID never exposed to ring-3
      — ENOENT error code added: u64::MAX−6 (-7: no such entry)
```

---

### 12  Kernel Safety & Integrity

These items are required by IEC 61508 SIL 4 / ISO 26262 ASIL D.
None of them add features — they make the existing features safe enough to certify.

```
[x] Hardware watchdog integration
      — hal/src/watchdog.rs: iB700 driver (ports 0x441 = enable+pet, 0x443 = disable)
      — timeout_index() maps desired seconds to iB700 table index (rounds up for safety)
      — hal::watchdog::init(10): armed at Stage 4 with 10-second timeout
      — idle process: hal::watchdog::kick() every 50 ticks (500 ms, well inside 10 s window)
      — scripts/run.sh: -device ib700,id=watchdog0 -watchdog-action reset
      — IEC 61508 §7.4.9: if scheduler stops delivering ticks → system resets to safe state
[x] Kernel invariant assertions
      — check_invariants() in Scheduler (debug_assertions only)
      — verifies PIDs in ready queue are in expected range
      — additional assertions at PCB creation (stack alignment, entry point)
[x] Persistent crash log
      — ErrorRecord { magic, vector, tick, pid, rip, rflags, cr2 } — 64 bytes per record
      — ring buffer of 16 records at fixed phys 0x4000 (conventional memory, survives warm reset)
      — crash_log::write() called from ring-0 #GP/#PF/#DF/#MC before halt
      — crash_log::drain() called in Stage 1 after activate_page_table(); prints via caller
        callback (hal-free core-kernel module) then zeros region; init() re-stamps header
      — IEC 61508 §7.4.6 post-mortem data requirement satisfied
[x] Health monitor notification path
      — #DE / #GP / #PF in ring-3 → terminate_faulting_process(fault_code)
      — sends IPC to PID 1 (init) with fault_code + faulting PID before terminating
      — forces context switch via tick_scheduler_isr(); system continues running
[x] Kernel .text read-only mapping  (see §3 — CR0.WP + PTE without WRITABLE)
      — arch_x86_64::cpu::init_protection() enables CR0.WP
      — remap_kernel_sections() clears PTE_WRITABLE from all .text pages
      — IEC 61508 §7.4.3: spatial isolation — kernel code cannot be patched at runtime
[x] Stack canaries on non-naked Rust frames  (-Z stack-protector=strong, nightly toolchain)
      — rust-toolchain.toml pins to nightly (required for -Z stack-protector)
      — .cargo/config.toml: [target.x86_64-unknown-uefi] rustflags = ["-Z", "stack-protector=strong"]
      — crates/core-kernel/src/stack_guard.rs: MSVC/UEFI ABI symbols (in core-kernel so all crates link):
            __security_cookie  = 0x595F_D4A3_E8C7_2B16  (fixed compile-time canary)
            __security_check_cookie(cookie)  — halts immediately on mismatch
      — Fixed canary avoids init-race: efi_main prologue runs before any init() could
        write a runtime value; epilogue would mismatch → false-positive → abort
      — IEC 61508 §7.4.3: canary word between locals and saved return address;
        complements the read-only .text mapping
[x] CPU feature checks at boot
      — check_cpu_features() in main.rs: verifies SMEP + MSR support via CpuFeatures
      — single-core enforcement: halts if HTT + max_logical_cpus > 1 (IEC 61508 §7.4.10)
      — Secure Boot: WARN in dev builds; FATAL halt in --features safety-mode builds
      — halts with diagnostic message if any required feature is absent
[x] Single-core enforcement
      — check_cpu_features() in main.rs: verifies HTT bit + max_logical_cpus (CPUID leaf 1)
      — halts with clear diagnostic if > 1 logical CPU detected (IEC 61508 SIL-4 §7.4.10)
      — documents the Relaxed-atomic / no-spinlock assumption enforced at boot
[x] ECC machine check handler     (see §5 — #MC IST3, vector 18, logs + halts)
[x] All 256 IDT vectors handled   (see §5 — catch-all stubs for all unregistered vectors)
[x] Reproducible build
      — SOURCE_DATE_EPOCH: defaults to git log -1 --pretty=%ct if not set by caller
      — RUSTFLAGS=--remap-path-prefix=${ROOT}=/rost strips absolute paths from debug info
      — scripts/build.sh prints SHA-256 of output EFI for verification
      — CI: build twice, compare SHA-256, assert identical
[x] OOM safe-state handler
      — BumpAllocator::alloc() logs "heap exhausted" over serial then halts
      — alloc_error_handler attribute not yet stable; OOM caught at allocation site
```

---

### 13  Formal Verification & Testing

```
[x] Unit tests — core-kernel crate  (no arch deps; runs on host with cargo test)
      — lib.rs: #![cfg_attr(not(test), no_std)] — std available in test mode
      — 123 tests total (run: cargo test -p core-kernel --target x86_64-apple-darwin)
      — test_priority_selection: lowest priority number wins
      — test_round_robin_within_priority: equal-priority processes alternate
      — test_edf_preempts_best_effort: RT process preempts even prio=0 BE process
      — test_edf_earliest_deadline_first: min(rt_deadline) wins among RT candidates
      — test_edf_period_renewal: deadline advances by period on expiry (no drift)
      — test_priority_inheritance: blocked client donates priority to server
      [x] memory/paging.rs   — 13 tests: map+translate basic/with-offset, translate absent
                               at PML4/PDPT/PD/PT level, flags preserved, alloc failure,
                               shared intermediate table reuse, unmap clears entry,
                               unmap absent returns false, unmap rejects huge page;
                               remap_page_flags: preserves-phys, absent-pml4,
                               absent-pt-entry, rejects-huge-page
      [x] memory/physical.rs — 6 tests: sequential allocation, rounds-up-to-page,
                               OOM (None after exhaustion), zero-capacity OOM,
                               current_base tracking, frame_tag/frame_kind/frame_stats
      [x] process/table.rs   — create, unique PIDs, get_ready_with_priority,
                               terminate_clears_slot, slot_reuse_after_terminate,
                               check_deadlines partial unblock
      [x] scheduler          — timer_tick quantum expiry / round-robin, priority selection,
                               EDF preemption + earliest-deadline + period-renewal,
                               priority inheritance with blocking client
      [x] ipc/message.rs     — 5 tests: empty queue, FIFO order, full rejects send,
                               circular wrap-around, notification OR + consume
      [x] acpi/madt.rs       — 6 tests: all entry types, null address, bad checksum,
                               bad signature, identity IRQ→GSI, truncated entry skipped
      [x] acpi/fadt.rs       — 9 tests: null/bad-sig/bad-checksum/too-short, pm timer valid,
                               pm timer invalid when len≠4, reset register, reset absent
                               when flag unset, reset absent for v1 table, zero port invalid
      [x] acpi/dmar.rs       — 9 tests: null/bad-sig/bad-checksum/too-short, no units,
                               single DRHD, multiple DRHDs, skip non-DRHD, truncated DRHD
                               skipped, iter_units, intr_remap flag absent
      [x] iommu/mod.rs       — 4 tests: root_entry_present, root_entry_alignment,
                               passthrough_ctx_entry (TT=0b10, present), passthrough_ctx_domain_id
      [x] process/table.rs  — 7 capability tests added: cap_alloc_returns_slot,
                               cap_find_after_alloc, cap_grant_copies_to_target,
                               cap_grant_requires_cap_g, cap_revoke_clears_slot,
                               cap_table_full_returns_none, cap_slot_reuse_after_revoke
      [x] memory/pool.rs     — 1 test (consolidated): LIFO order, exhaustion→None,
                               free+realloc, overflow spills to global allocator
      [x] process/pcb.rs     — 3 tests: free out-of-range safe, stack zeroed on free,
                               reclaim LIFO reuse; Drop impl automatically frees stack slot
      [x] acpi/hpet.rs       — 8 tests: null/bad-sig/bad-checksum/too-short, non-memory GAS
                               rejected, zero period rejected, period_too_large rejected,
                               valid 10 MHz (QEMU), valid 14 MHz, boundary period accepted
      [x] hpet.rs            — 3 tests: init zero-base is noop, read_counter before init → 0,
                               period_fs stored by init
      [x] timer/tsc.rs       — 8 tests: compute_khz (3 GHz over 10 ms = 3 MHz kHz, zero duration,
                               1 GHz), hpet_ticks_per_ms (10 MHz 1 ms / 10 ms, 14 MHz, zero period),
                               tsc_to_ns (zero before calibration, 3 GHz 3M ticks = 1 ms)
      [x] process/table.rs   — 5 deadline tests: earliest_deadline_no_blocked, all_ready,
                               single_blocked, multiple_blocked (min), skips_indefinite (MAX)
      [x] scheduler          — 2 deadline-API tests: ticks_until_next_event_no_blocked,
                               ticks_until_next_event_ticks_advance
      [x] service_registry.rs — 7 tests: register_and_lookup, lookup_nonexistent,
                               register_overwrites_existing, unregister_makes_lookup_none,
                               unregister_nonexistent_returns_false,
                               trim_name_stops_at_nul, lookup_is_case_sensitive
      [x] process/table.rs   — 6 cycle + budget tests: detect_cycle_no_links,
                               detect_cycle_chain_no_cycle, detect_cycle_direct,
                               detect_cycle_indirect, detect_cycle_unknown_pid,
                               reset_cpu_budget_counters (zeroes all cpu_budget_used)
      [x] scheduler          — 8 IEC §7.4.4 / §7.4.5 tests:
                               deadlock: none_when_no_wait_chain, no_cycle_chain,
                                 direct_cycle (A→B→A), indirect_cycle (A→B→C→A)
                               memory quota: unlimited (0=no limit), enforced (rejects at limit),
                                 boundary (exactly-at-limit OK; one-over rejected)
                               cpu_budget: frame_reset (cpu_budget_used zeroed after 1000 ticks)
[x] Branch coverage ≥ MC/DC for all kernel modules  (cargo llvm-cov)
      — rust-toolchain.toml: components = ["llvm-tools-preview"]
      — scripts/coverage.sh: cargo llvm-cov --target host --package core-kernel
        --lcov --output-path target/lcov.info  (+ --html for interactive report)
      — THRESHOLD env var enforces minimum line-coverage percentage
      — Usage: scripts/coverage.sh --html
[x] System tests in QEMU (automated serial-capture harness)
      — scripts/test-qemu.sh: boots kernel in headless QEMU, monitors COM1 serial
        output via -serial file:, asserts expected strings within configurable timeout
      — watchdog-action=poweroff ensures every test terminates (no hung VM)
      — TC-SYS-001: boot-completes  ("Stage 1:" → "Stage 8:" → "Rost kernel ready")
      — TC-SYS-002: timer-hardware-init  ("HPET" + "TSC calibrated")
      — TC-SYS-003: ring3-servers-spawned  ("PID 2" + "PID 3" + "PID 4")
      — TC-SYS-004: scheduler-ticks  (kernel reaches ready state)
      — TC-SYS-005: crash-log-drain  ("Stage 1:" + "crash")
      — Usage: scripts/test-qemu.sh  [--no-build]  TIMEOUT=60 for slow machines
[x] Fault injection test mode  (feature = "fault-injection" build flag)
      — crates/kernel/Cargo.toml: fault-injection = ["arch-x86_64/fault-injection"]
      — crates/arch-x86_64/Cargo.toml: [features] fault-injection = []
      — SYS_INJECT_FAULT = syscall 25 (absent in production; falls through to ENOSYS)
      — Supported vectors: 3 (#BP catch-all), 6 (#UD catch-all),
        13 (#GP — halts on ring-0 origin), 14 (#PF — halts on ring-0 origin)
      — Build: cargo build --features fault-injection  (kernel + arch-x86_64)
      — IEC 61508 §7.4.7: every exception handler path is reachable via test injection
[x] Formal kernel invariants  (TLA+ model — specs/ directory)
      — specs/Scheduler.tla + Scheduler.cfg
          Safety:   TypeOK, AtMostOneRunning, RunningConsistent
          Liveness: NoStarvation (every Ready process eventually runs; WF on Tick)
      — specs/IPC.tla + IPC.cfg
          Safety:   NeverSendWithoutCap (Send precondition enforces capability check)
                    TypeOK, CapabilityConfinement
      — specs/Memory.tla + Memory.cfg
          Safety:   FrameIsolation, PrivateFrameExclusive
                    (private frames never aliased across processes)
      — Run: java -jar tla2tools.jar -config specs/Scheduler.cfg specs/Scheduler.tla
[x] Requirements traceability matrix
      — docs/traceability.md: 37 requirements across REQ-MEM / REQ-PROC / REQ-IPC /
        REQ-TIMER / REQ-SAFE / REQ-ACPI / REQ-SYSCALL domains
      — Each row: REQ-ID → Module → Unit/system test(s) → Status
      — Cross-references TLA+ invariants to implementation modules
```

---

## Part II — User Space

Everything below runs in ring 3 and communicates with the kernel only through
the system calls defined in §9.  A kernel bug cannot be caused by any code
in this section.

---

### 14  Init Process  (PID 1 — Health Monitor)

The first user-space process; owns the system lifecycle.

```
[x] Launch at kernel boot as PID 1 (via ELF loader — §15)
      — servers/init binary (rost-init) embedded in kernel via include_bytes!
      — spawned BEFORE idle process so PID assignment gives it PID 1
      — kernel fault handler (terminate_faulting_process) hardcodes PID 1 as target
      — registers as "init" via SYS_REGISTER on startup
      — priority 32 (above normal servers; below RT; above shell/uart-drv/vfs)
      — spawn order: init(PID1) → idle(PID2) → uart-drv(PID3) → vfs(PID4) → shell(PID5)
[x] Receive fault notifications from kernel
      — kernel sends IPC to PID 1 on any ring-3 #DE/#GP/#PF with fault_code + pid
      — init decodes fault_code (0xDE/#DE, 0x0D/#GP, 0x0E/#PF) and logs it
      — matches faulting PID against known critical services
[x] Service registry (kernel-side)
      — SYS_REGISTER / SYS_LOOKUP already wired in kernel (§11)
      — init resolves peer PIDs lazily (uart-drv, rost-vfs, rost-shell) via SYS_LOOKUP
[x] Process restart policy
      — per-service critical flag: uart-drv and vfs are critical; shell is non-critical
      — critical crash → ordered_shutdown() → SYS_EXIT(1); watchdog resets the system
      — non-critical crash → fault logged, system continues
      — restart (SYS_SPAWN) deferred until §15 SYS_SPAWN is implemented
[x] System-level safe-state transition
      — ordered_shutdown(): logs intent, exits with code 1
      — kernel terminates init; hardware watchdog fires after timeout → system reset
      — IEC 61508 §7.4.9: reset is the defined safe state for unrecoverable failure
[x] Heartbeat from every registered process
      — init tracks last_beat[i] timestamp (via SYS_CLOCK) per service slot
      — servers send OP_HEARTBEAT (data[0]=0x0001) IPC to init periodically
      — 5-second timeout: if a registered service misses its deadline, init logs a warning
      — heartbeat watchdog skips first 300 ms (warmup_ticks) to allow services to register
[x] Shutdown request opcode
      — any process can send OP_SHUTDOWN (data[0]=0x00FF) to init
      — init logs the requester and calls ordered_shutdown()
[x] Expose boot log over IPC to diagnostic clients
      — 16-entry ring buffer (LogBuf) in init; each entry: seq, timestamp_ns, 32-byte text
      — OP_LOG_READ (0x0002) request via SYS_CALL(17) → init replies OP_LOG_READ_REPLY (0x8002)
      — reply: data[0]=opcode, data[1]=seq, data[2]=next_seq, data[3]=ts, data[4..7]=text (4×u64)
      — events logged: startup, service PID resolved, fault, restart ok/fail, heartbeat timeout
[x] Process restart via SYS_RESTART_SERVER (27)
      — on fault notification, init calls SYS_RESTART_SERVER with the 16-byte service name
      — kernel hook (restart_server_hook) maps name → embedded ELF and calls spawn_elf
      — up to MAX_RESTARTS (3) attempts per service; restart_count tracked per ServiceEntry
      — critical service (uart-drv, vfs): restart exhausted → ordered_shutdown()
      — non-critical service (shell):     restart exhausted → warn and continue
```

---

### 15  ELF Loader

Parses ELF64 images and launches them as new processes.

```
[x] ELF64 header validation  (magic, class=64, data=LSB, machine=x86-64)
[x] Program header walk       (PT_LOAD segments → map into per-process PML4)
[x] PT_LOAD mapping
      — allocate physical frames from global bump allocator
      — map at ELF vaddr with PTE_PRESENT | PTE_USER | optional WRITABLE / NO_EXECUTE
      — zero-fill BSS (memsz > filesz gap)
[x] Entry point extraction    (e_entry from ELF header → PCB context / fake return addr)
[x] New PML4 per binary       (load() allocates fresh PML4 when none is provided)
[x] spawn_elf(data, priority) — load + allocate user stack + merge kernel PTs + add_ring3_process
[x] Kernel integration
      — elf::load and elf::spawn_elf in kernel/src/elf.rs
      — uart-drv / vfs / shell ELFs embedded via include_bytes! and spawned at boot (§18, §17, §16)
      — build.sh compiles servers first, then kernel (which embeds them)
[x] Initial user stack        (16 KB mapped at 0x7FFF_FFFF_F000 downward in spawn_elf, PTE_USER|NX)
[x] Kernel PML4 merge         (merge_kernel_into_user_pml4: copies PD[1..511]/PDPT[1..511]/PML4[1..511]
                               so syscall handlers access kernel memory when CR3=user PML4)
[x] IRETQ trampoline          (ring3_entry_trampoline: pushes IRETQ frame, CS=0x23, SS=0x1B, iretq)
[x] read_unaligned ELF header parse (1-byte aligned include_bytes! data, ELFCLASS64 struct)
[ ] Dynamic linking stub      (initially: require static ELF only; dynamic = future)
[ ] sys_exec(path, argv)      — syscall wrapper that the shell and init use
```

---

### 16  Shell Server  (servers/shell)

Interactive diagnostic interface over serial; runs as ring-3 ELF binary.
Implemented as a full zsh-compatible shell — the default interactive shell for Rost.

```
[x] Ring-3 ELF binary   (x86_64-unknown-none, servers/shell workspace)
[x] Serial I/O via SYS_UART_WRITE (12) for output — bypasses uart-drv IPC queue
      — eliminates queue-overflow byte loss during banner/prompt burst writes
      — put_byte() / put_newline() / print_str() all call SYS_UART_WRITE directly
      — put_newline() sends explicit \r\n; print_str() translates every \n → \r\n
[x] Input via SYS_RECV (IPC from uart-drv) — non-blocking poll + yield_cpu() idle loop
      — read_byte(): SYS_RECV(timeout=0) — returns immediately, no context switch
      — idle path: yield_cpu() → immediate switch to uart-drv via SYS_YIELD yield_switch()
[x] CRLF absorber in escape parser (last_cr field)
      — terminal sends \r\n on Enter; \r → Key::Enter, subsequent \n absorbed silently
      — prevents double Enter events from CRLF terminal input
[x] Raw-mode terminal guarantee (scripts/run.sh)
      — QEMU wrapped with exec script -q /dev/null so it always sees a proper PTY
      — guarantees QEMU's tcsetattr correctly sets raw mode regardless of how run.sh
        is invoked (terminal, IDE run button, pipe, etc.)
      — eliminates double-echo and spurious blank lines caused by cooked-mode buffering
[x] Correct CRLF output from all shell commands
      — cmd_echo uses put_newline() (\r\n) instead of bare put_byte(\n)
      — print_prompt() prefixes \r to guarantee column-0 alignment after any command
        that may leave the cursor mid-line (bare \n, partial output, etc.)
[x] Interactive UART read loop

[x] Full emacs-mode line editing
      — insert / delete at cursor; Backspace / Delete
      — Ctrl+A / Ctrl+E  (beginning / end of line; same as Home/End)
      — Ctrl+B / Ctrl+F  (backward / forward char; same as arrow keys)
      — Alt+B  / Alt+F   (word movement; also Ctrl+Left / Ctrl+Right)
      — Ctrl+K           (kill to end of line → kill ring)
      — Ctrl+U           (kill to beginning of line → kill ring)
      — Ctrl+W           (kill previous word → kill ring)
      — Ctrl+Y           (yank — paste from kill ring)
      — Ctrl+D           (delete-char or EOF if line empty → clean exit)
      — Ctrl+C           (cancel line)
      — Ctrl+L           (clear screen)

[x] VT100/xterm escape sequence parser
      — Arrow keys, Home, End, Delete (ESC[A/B/C/D, ESC[H/F, ESC[3~)
      — ESC O sequences (SS3 — rxvt/xterm alternate form)
      — ESC [ 1 ; modifier sequences (Ctrl+Arrow → word movement)
      — Alt+B / Alt+F  (ESC b / ESC f)
      — All Ctrl+ byte values 0x01–0x1F decoded to named Key enum variants

[x] Ctrl+R incremental reverse history search (zsh-style)
      — (reverse-i-search)'': prompt while typing query
      — Each character narrows the search through history
      — Ctrl+R again: cycle to next older match
      — Backspace: shorten query, restart from newest match
      — Enter: accept match and execute; Ctrl+C: cancel, restore original line

[x] Command history (32 entries, circular, skips consecutive duplicates)
[x] History navigation: Up/Down arrows, Ctrl+P / Ctrl+N

[x] History expansion (zsh-style !-expansion)
      — !!              last command
      — !n              nth absolute entry (1 = oldest)
      — !-n             nth from end (1 = last)
      — !prefix         last command starting with prefix
      — Expanded line echoed before execution (zsh behaviour)

[x] Variable store (48 slots; NAME_MAX=32, VAL_MAX=128)
      — Pre-populated defaults: HOME, USER, SHELL, HOSTNAME, TERM, PATH,
        PWD, OLDPWD, IFS, LANG
      — $VAR, ${VAR} expansion in every input line before dispatch
      — $$ (PID), $? (last exit code), $0 (shell name)
      — ~  tilde-expansion to $HOME at start of word
      — PWD / OLDPWD updated on every successful cd
      — export VAR=val  — set variable; unset VAR — remove it
      — set / env       — list all variables

[x] Alias table (16 slots; defaults: ll=ls, la=ls, h=history, quit=exit, .=source)
      — alias [name=val]   — define or list aliases
      — unalias name       — remove alias
      — Alias expansion: first token of every line resolved before dispatch
      — Quote stripping on alias values (single and double quotes)

[x] Compound command execution
      — cmd1 ; cmd2     execute sequentially
      — cmd1 && cmd2    execute cmd2 only if cmd1 returns exit code 0
      — cmd1 || cmd2    execute cmd2 only if cmd1 returns non-zero
      — Up to 8 segments per input line; quote-aware ; / && / || scanner

[x] Bare variable assignment  (VAR=value with no leading spaces = set var, no dispatch)

[x] Tab completion
      — Command names: prefix match against sorted COMMANDS table
      — Path arguments: VFS OP_READDIR of parent dir, filter by name prefix
      — Single match: complete inline; multiple: list on new line
      — Path completion also active when partial starts with / or ./

[x] Dynamic prompt  (rost@local:<cwd>$  with ANSI colour)

[x] Full command set:
      alias / unalias    — alias management
      cat <path>         — stream file via VFS OP_READ (chunked, stateless)
      cd [path|~]        — change directory; cd with no args → $HOME
      clear              — clear screen (ANSI ESC[2J ESC[H)
      date               — show uptime in days/h/m/s/ms
      echo [-n] <args>   — print arguments; -n suppresses newline
      env / set          — list all environment variables
      exec <path> [pri]  — read ELF from VFS + SYS_SPAWN_ELF (26); prints new PID
      exit [code]        — exit shell with optional code
      export VAR=val     — set environment variable
      false              — return exit code 1
      halt               — system halt
      help               — detailed help with keybindings and syntax
      history            — numbered list of command history
      kill <pid>         — SYS_NOTIFY(pid, 1) — send SIGTERM
      log                — crash log location / format info
      ls [path]          — list directory via VFS OP_READDIR (colour-coded)
      mem                — physical memory layout summary
      mkdir <path>       — create a directory in the mutable VFS overlay (OP_MKDIR)
      mount              — display VFS mount table via OP_MOUNT
      ps                 — list well-known PIDs; shows own PID
      pwd                — print working directory
      rm <path>          — remove a mutable file or directory (OP_UNLINK)
      set                — list all shell variables (alias for env)
      sleep <n>          — sleep n seconds (SYS_RECV timeout = n × 100 ticks)
      source <file> / .  — read and execute script from VFS (up to 2 KB)
      test EXPR / [ ]    — -f/-d/-z/-n FILE; A = B; A != B; -eq/-ne/-lt/-le/-gt/-ge
      touch <path>       — create empty file in the mutable VFS overlay (OP_WRITE_OPEN/CLOSE)
      true               — return exit code 0
      type <cmd>         — show whether cmd is builtin, alias, or not found
      unalias <name>     — remove alias
      unset <name>       — remove variable
      uptime             — show h/m/s uptime via SYS_CLOCK
      which <cmd>        — builtin/alias check; VFS /bin lookup via OP_STAT
      write <path> <text...>  — write space-joined args (+ newline) to a mutable file

[x] exec <path>   — read ELF from VFS, call SYS_SPAWN_ELF (26), print new PID
                     — /bin/hello   (hello-world Rust demo) embedded in VFS
                     — /bin/hello-c (freestanding C demo) embedded in VFS; no libc / no CRT
                     — 512 KB static EXEC_BUF in shell BSS; ELF magic validated before spawn
[x] ps command    — SYS_LIST_PROCS (28) snapshots kernel process table; 24 B/entry
```

---

### 17  VFS Server  (servers/vfs)

Virtual filesystem IPC server; runs as ring-3 ELF binary (PID 4 by convention).

```
[x] Ring-3 ELF binary   (x86_64-unknown-none, servers/vfs workspace)
[x] IPC dispatch loop   (SYS_RECV_MSG + SYS_SEND_MSG; blocks until request arrives)
[x] OP_READDIR    (0x20) — list directory entries; RESP_ENTRY per child + RESP_DONE
                           — static children emitted first; mutable entries that shadow a
                             static name are de-duplicated (mutable version wins)
[x] OP_READ       (0x22) — stateless read: client passes path + byte offset each call
                           — mutable overlay checked first; falls back to static tree
[x] OP_STAT       (0x23) — query flags (is_dir, executable) + size for a path
                           — mutable overlay checked first; falls back to static tree
[x] OP_MOUNT      (0x24) — enumerate mount table; RESP_MOUNT per entry + RESP_DONE
[x] OP_WRITE_OPEN (0x25) — open/create file for streaming write; truncates existing content
                           — allocates slot in mutable overlay; RESP_OK or RESP_ERROR
[x] OP_WRITE_DATA (0x26) — stream a 40-byte chunk into the open write session
                           — data[1]=chunk_len, data[2..8]=bytes (little-endian packed)
[x] OP_WRITE_CLOSE(0x27) — finalise and close the write session; RESP_OK
[x] OP_MKDIR      (0x28) — create a directory in the mutable overlay; RESP_OK or RESP_ERROR
[x] OP_UNLINK     (0x29) — remove a mutable entry; closes stale fds on that path; RESP_OK
[x] OP_OPEN       (0x30) — open a file descriptor (stateful, VFS tracks position)
                           — oflags: O_RDONLY=0, O_WRONLY=1, O_RDWR=2 | O_CREAT=4 | O_TRUNC=8 | O_APPEND=16
                           — reply: RESP_FD(fd_num) or RESP_ERROR(ENOENT/ENOSPC/EMFILE/EACCES/EISDIR)
[x] OP_CLOSE      (0x31) — close a file descriptor; RESP_OK or RESP_ERROR(EBADF)
[x] OP_READ_FD    (0x32) — read next CHUNK_SIZE bytes via fd; VFS advances offset
                           — reply: RESP_DATA or RESP_DONE (EOF) or RESP_ERROR
[x] OP_WRITE_FD   (0x33) — write up to 40 bytes via fd; VFS advances offset
                           — data[1]=fd, data[2]=chunk_len, data[3..8]=bytes
                           — O_APPEND writes always go to end of file regardless of offset
[x] OP_SEEK       (0x34) — set fd offset to absolute byte position; RESP_OK or RESP_ERROR(EBADF)
[x] OP_FSTAT      (0x35) — stat an open fd; same RESP_STAT reply as OP_STAT
[x] IPC protocol (proto.rs)
      — stateless opcodes 0x20–0x29; stateful fd opcodes 0x30–0x35
      — responses 0x80–0x8F; RESP_OK=0x85, RESP_FD=0x86
      — PATH_BYTES=48 (data[2..8]); CHUNK_SIZE=40 (data[3..8]); NAME_BYTES=40
      — errno: ENOENT=1, ENOTDIR=2, EISDIR=3, ENOSYS=4, ENOSPC=5, EBADF=6, EMFILE=7, EACCES=8
[x] Static RAM filesystem (fs.rs)
      — /bin/hello (Rust ELF demo), /bin/hello-c (freestanding C ELF demo)
      — /etc/{hosts,motd,passwd}
      — /home/user/{notes.txt, readme.txt}, /root/.profile
      — /var/log/ (empty directory), /motd.txt, /version.txt
      — lookup(path): splits on '/', trims nulls, walks DirEntry tree
[x] Mutable RAM-disk overlay (mutable.rs)
      — 32 nodes × 4096 B ≈ 131 KB BSS; heap-free, fixed-size
      — MutNode { path[64], plen, flags, dlen, used, data[4096] }
      — API: find(), alloc(), remove(), write_chunk(), truncate(), get(), children_of()
      — Mutable entries shadow same-path static entries in READ/STAT/READDIR
[x] File descriptor table (fd.rs)
      — 256 slots (8 fds × 32 processes) ≈ 20 KB BSS; heap-free
      — FdEntry { pid, fd, oflags, offset, path[64], plen, used }
      — API: alloc(), get(), advance(), seek(), free(), free_all(), free_all_path()
      — free_all_path() closes stale fds when a file is unlinked
[x] Mount table
      — MountPoint { path: b"/", fstype: b"ramfs", source: b"ramdisk:0" }
[x] Block device IPC client (blk.rs)
      — lazy-resolves "block-drv" PID via SYS_LOOKUP; cached after first probe
      — read_sector(lba, buf): up to 13 synchronous SYS_CALL round-trips per 512-byte sector
      — protocol: OP_BLK_READ(0x50) request, RESP_BLK_DATA(0x90) reply, 40 B per chunk
      — available() returns false if no block-drv registered → VFS silent fallback to static tree
[x] FAT32 parser (fat32.rs)
      — parses BPB from sector 0; validates jump-boot sig + extended boot sig (0x28/0x29)
      — single-sector FAT cache + single-sector cluster-data cache (~1 KB BSS total)
      — cluster chain traversal via fat_next() with FAT sector caching
      — 8.3 short filenames + LFN (Long File Name) entries, up to 255 chars
      — case-insensitive path component matching
      — public API: available(), lookup(path), readdir(path, cb), read_file(path, offset, buf)
[x] Three-layer storage priority in all read operations
      — mutable overlay > FAT32 > static ROM tree
      — READDIR: SeenNames dedup struct (32 × 40 B on stack) prevents duplicate entries
      — READ / STAT / FSTAT / OP_READ_FD: mutable checked first, then FAT32, then static
```

---

### 18  Device Drivers  (userspace servers)

All drivers run in ring 3.  A driver crash cannot take down the kernel.

```
[ ] Driver model
      — drivers register with init via SYS_REGISTER; receive IRQ notifications via IPC
      — kernel forwards hardware IRQs to registered driver processes
[x] uart-drv server (PID 3 by convention)  — servers/uart-drv, spawned by kernel via ELF loader
      — registers as "uart-drv" via SYS_REGISTER(10) at startup
      — main loop: drain SYS_RECV_MSG(0) write requests (OP_WRITE=0x01) → SYS_UART_WRITE(12)
      — IRQ-driven RX: UART ISR (vector 36, via IOAPIC) drains COM1 FIFO and sends one
          OP_UART_RX IPC message per byte to uart-drv; uart-drv forwards each byte to the
          foreground shell via SYS_SEND — zero-latency input, no polling required
      — IOAPIC routing: route_irq(ioapic_base, pin=4, vector=36, lapic=0) wires ISA IRQ4
          (COM1) to the LAPIC at boot (main.rs Stage 3, after ioapic::init())
      — uart_rx_isr() context-switches immediately to uart-drv after FIFO drain so
          keystrokes reach the shell within one scheduler quantum
      — looks up shell PID via SYS_LOOKUP("rost-shell")
      — priority 64 (equal to shell)
[ ] Block device driver   (ATA PIO or virtio-blk for QEMU)
[ ] GOP framebuffer driver
      — maps framebuffer physical address via SYS_MAP
      — exposes blit / fill / draw-text IPC interface
[ ] PS/2 keyboard driver  (or USB HID via xHCI — long term)
[x] virtio-net driver     (QEMU networking — rost-net server, PID 6)
[ ] PCI bus enumeration   (scan, read config space, allocate BARs)
```

---

### 19  GOP Framebuffer Console

Visible output in the QEMU window (currently blank — serial only).

```
[ ] Map GOP framebuffer into display driver address space via SYS_MAP
[ ] PSF2 bitmap font (PC Screen Font — compact, public domain)
[ ] Text renderer     (glyph blit, cursor, scroll)
[ ] Terminal emulator (subset of VT100 — enough for the shell)
[ ] Panic screen      (kernel writes directly to framebuffer on fatal error,
                       bypassing the driver server)
```

---

### 20  Network Stack

```
[x] virtio-net driver    (legacy PCI, QUEUE_SIZE=256, BAR0 I/O port, 8 pre-filled RX bufs)
[x] Ethernet frame TX/RX (eth.rs — Ethernet II parse/build, ETH_ARP/ETH_IPV4)
[x] ARP                  (arp.rs — 8-entry cache, request/reply, resolve helper)
[x] IPv4 / ICMPv4        (ipv4.rs — 20-byte header, auto-checksum; icmp.rs — echo req/reply)
[x] UDP                  (udp.rs — 4-slot socket table, bind/deliver/get_pending)
[x] TCP  (minimal)       (tcp.rs — single conn; states Closed/SynSent/Established/FinWait1/CloseWait; client-only)
[x] IPC socket API       (socket.rs — OP_NET_PING/UDP_BIND/UDP_SEND/UDP_RECV/TCP_CONNECT/TCP_SEND/TCP_RECV/TCP_CLOSE/GET_IP)
[x] Shell ping command   (shell: ping [ip] [count] → OP_NET_PING → ICMP echo → RTT display)
[x] Kernel I/O port syscalls (SYS_IOPORT_OUT=29, SYS_IOPORT_IN=30 — ring-3 PCI/device access)
[x] Kernel phys-addr syscall (SYS_PHYS_ADDR=31 — virt→phys translation for DMA setup)
[x] IRQ-driven RX        (SYS_IRQ_REGISTER=32: rost-net registers virtio-net GSI via IOAPIC;
                           kernel ISR reads virtio ISR status port, delivers IPC 0xFFFF_0000|gsi;
                           main loop dispatches IRQ notifications → poll_rx immediately)
[x] Async ping           (non-blocking OP_NET_PING: state machine WaitARP→WaitICMP→Idle;
                           reply sent asynchronously when ARP/ICMP packets arrive via IRQ;
                           2-second deadline checked each main-loop iteration)
[x] TX spin reduction    (virtio TX used-ring poll: 500 000 → 50 000 iterations ~5 ms cap)
      — rost-net registered as "rost-net" in service registry; spawned as PID 6
      — QEMU: -netdev user,id=net0 -device virtio-net-pci,netdev=net0
      — Guest IP: 10.0.2.15; gateway 10.0.2.2 (default ping target)
```

---

### 21  Interrupt-Driven I/O Conversion

Audit of all polling patterns in the codebase and conversion to hardware-interrupt
delivery where possible.

#### Polling patterns — disposition

| Pattern                   | Location       | Disposition                                                                                     |
|---------------------------|----------------|-------------------------------------------------------------------------------------------------|
| UART TX THRE spin         | hal/uart.rs    | **Converted** — interrupt-driven TX ring buffer (see §21 pass 3).                              |
| PIT calibration spin      | lapic.rs       | Cannot be interrupt-driven: PIT ch2 has no IRQ line (bit 5 of port 0x61 is a GPIO output).     |
|                           |                | Runs once at boot; no scheduling impact. Genuinely not convertible without HPET interrupt SM.  |
| Virtio TX used-ring poll  | virtio.rs      | **Converted** — non-blocking send; IRQ-driven reclaim via `reclaim_tx()` (see §21 pass 3).     |
| Virtio RX polling         | net/main.rs    | **Converted** — IRQ-driven (see §20 IRQ-driven RX).                                            |
| Ping blocking loop        | net/main.rs    | **Converted** — async ping state machine (§20).                                                 |
| ARP blocking loop (ping)  | net/main.rs    | **Converted** — async ping SM handles ARP miss (§20).                                          |
| resolve_mac() blocking    | net/main.rs    | **Converted** — cache-only + async ARP kick; UDP/TCP callers get EAGAIN on cache miss.          |
| TCP connect blocking loop | net/main.rs    | **Converted** — async TCP-connect SM (WaitARP→WaitSynAck→Idle; see §21 below).                 |
| IOMMU GSTS spins (×3)     | iommu.rs       | **Bounded** — added 100 000-iteration safety timeout per IEC 61508 §7.4.1; log+fail on breach. |
| Shell input               | shell/io.rs    | Already interrupt-driven via uart-drv ISR (vector 36).                                          |
| Net IPC recv timeout      | net/main.rs    | **Converted** — recv_msg(10)→recv_msg(100); IRQ wakes earlier.                                 |
| Init lazy PID lookup      | init/main.rs   | Low priority (boot-time retry). Acceptable.                                                     |

#### Infrastructure added (pass 1 — §20 milestone)

```
[x] irq_registry module     (core-kernel/src/irq_registry.rs)
      — Maps GSI 0-15 → (owner PID, ISR port) using lock-free AtomicU32/AtomicU16 arrays
      — register(gsi, pid, isr_port) + lookup(gsi) — used by kernel ISR and SYS_IRQ_REGISTER
[x] PCI IRQ IDT stubs       (arch-x86_64/src/interrupts/handlers.rs)
      — pci_irq_stub! macro generates naked ISR for each of GSI 8-15 (vectors 40-47)
      — ISR: save caller-saved regs → read device ISR port → IPC notify owner → slave+master+LAPIC EOI → reschedule
      — handle_pci_irq(gsi): looks up irq_registry, inb(isr_port), send_message(owner, 0xFFFF_0000|gsi)
[x] IOAPIC IDT wiring       (arch-x86_64/src/interrupts/mod.rs)
      — Vectors 40-47 wired to pci_irq_gsi8..pci_irq_gsi15 (replaces unexpected_stub stubs)
      — IOAPIC_BASE static published by set_ioapic_base() (called from kernel/main.rs Stage 3)
[x] SYS_IRQ_REGISTER = 32  (arch-x86_64/src/cpu/syscall.rs)
      — Ring-3 driver calls: a0=GSI (8-15), a1=ISR port
      — Kernel: registers caller PID in irq_registry; routes IOAPIC GSI→vector (32+GSI)
      — Validated: GSI must be 8-15; IOAPIC base must be non-zero
[x] Kernel main.rs Stage 3  (crates/kernel/src/main.rs)
      — Calls set_ioapic_base(ioapic_base) after ioapic::init() to publish base for SYS_IRQ_REGISTER
```

#### Infrastructure added (pass 3 — this milestone)

```
[x] IOMMU GSTS safety timeouts   (core-kernel/src/iommu/mod.rs)
      — SRTP / WBF / TE handshakes: unbounded spins → 100 000-iter countdown + warn+fail on breach
      — Satisfies IEC 61508 §7.4.1 liveness requirement for all kernel busy-waits
[x] resolve_mac() non-blocking   (servers/net/src/main.rs)
      — Removed 2-second blocking ARP poll loop
      — Cache hit → return MAC immediately; cache miss → send ARP request, return None (EAGAIN)
      — ARP reply populates cache asynchronously via IRQ → poll_rx → handle_arp_packet
[x] UDP send EAGAIN              (servers/net/src/main.rs)
      — OP_NET_UDP_SEND on ARP cache miss → reply 0xFFFD (EAGAIN); client retries after ~20 ms
[x] TCP-connect async SM         (servers/net/src/main.rs)
      — TCP_SM_STATE: Idle / WaitARP / WaitSynAck
      — WaitARP: sends ARP request; advances to WaitSynAck when ARP reply resolves destination
      — WaitSynAck: tcp_sm_on_established() sends ACK and delivers conn_id to caller
      — tcp_sm_on_reset(): cleans up and delivers 0xFF to caller
      — 5-second deadline checked each main-loop iteration via check_pending_deadlines()
      — Both ARP and SYN-ACK phases now driven by hardware IRQ → poll_rx → handle_packet
```

#### Infrastructure added (pass 3 — UART TX + Virtio TX)

```
[x] UART TX interrupt-driven    (crates/hal/src/uart.rs)
      — TX ring buffer: 255-byte SPSC (AtomicU8 head/tail, u8 wraps at 256)
      — put_byte(): push to ring; if THRE set write one byte directly (pump-prime);
        arm ETBEI (IER bit 1) so tx_isr drains the rest; zero busy-wait
      — tx_isr(): pop next byte from ring, write to COM1 THR; disable ETBEI when empty
      — Safe from ISR context: THRE interrupt armed but fires after iretq
      — init(): ETBEI disabled initially; armed on-demand by put_byte
[x] UART ISR IIR dispatch       (crates/arch-x86_64/src/interrupts/handlers.rs)
      — handle_uart_rx() now reads IIR to determine interrupt cause:
          IIR_THRE (0x02) → call hal::uart::tx_isr()
          IIR_RDA  (0x04) / IIR_CTI (0x0C) → existing RX drain + IPC delivery
          bit 0 set → spurious, return
      — Single ISR handles both RX and TX for vector 36 (IRQ4, COM1)
[x] Virtio TX non-blocking      (servers/net/src/virtio.rs)
      — send_packet(): calls reclaim_tx() then checks ring space, kicks queue,
        returns immediately — no spin loop
      — reclaim_tx(): reads TX used-ring idx, advances tx_used_last; called from
        IRQ notification path (reclaims TX completions alongside RX drain)
      — Ring-space guard: returns false if >128 descriptors in-flight (never
        happens in practice with single-threaded net server)
```

#### Remaining irreducible busy-waits (hardware limitations)

| Pattern               | Reason not convertible                                               |
|-----------------------|----------------------------------------------------------------------|
| PIT ch2 calibration   | Port 0x61 bit 5 is a GPIO pin with no IRQ line; boot-time only once |
| IOMMU SRTP/WBF/TE     | VT-d hardware protocol; completion signalled by GSTS poll only      |

---

### 22  POSIX Compatibility Layer  (long term)

Thin library that maps POSIX calls onto Rost syscalls.
Runs entirely in user space; nothing in ring 0.

```
[ ] libc subset  (malloc via SYS_MAP, free, memcpy, string.h)
[ ] pthread subset  (threads within a process share address space — requires kernel TLS)
[ ] POSIX signals  (mapped to IPC notifications from init)
[ ] fork / exec  (fork = copy address space; exec = ELF loader)
[ ] File I/O  (wraps VFS server IPC)
```

---

## Dependency Order (critical path)

Items marked `[x]` are complete; `[~]` are partial; `[ ]` are not started.

```
[x] ExitBootServices()
  └─ Physical frame tracker  (know what memory is free to own)
       └─ Free-list allocator  (can reclaim memory)
            └─ [x] CR3 loaded  (KERNEL_PML4 identity-maps all UEFI regions; NXE+WP+SMEP+SMAP)
                 └─ [x] Global bump allocator (global_alloc_4k — post-heap free RAM)
                      └─ [x] map_page_global  (SYS_MAP + ELF loader page mapping)
                 └─ [x] Per-process PML4  (ELF loader creates fresh PML4; kernel PTs merged in)
                      └─ [x] TSS loaded + IST stacks configured
                           └─ [x] TSS.RSP0 + SYSCALL_KERN_RSP per-switch update
                                └─ [x] Preemptive timer ISR  (tick_scheduler_isr + switch_context_noints)
                                     └─ [x] SYS_YIELD  (yield_current → quantum exhaustion)
                                          └─ [x] SYS_SPAWN  (add_process from ring-3)
                                               └─ [x] ELF loader  (spawn_elf: load + stack + merge + ring3_entry_trampoline)
                                                    └─ [x] SYSCALL kernel stack switch  (SMAP-safe entry)
                                                         └─ [x] uart-drv server  (PID 2, spawned at boot)
                                                              └─ [x] VFS server launched from ELF  (PID 3)
                                                                   └─ [x] Shell server launched from ELF  (PID 4)
                                └─ [x] Ring-3 fault termination  (#DE/#GP/#PF → terminate + notify init)
                                     └─ [x] Service registry  (SYS_REGISTER / SYS_LOOKUP)
```
