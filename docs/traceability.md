# Requirements Traceability Matrix

Maps each safety/functional requirement to the kernel module that implements
it and the unit/system test that verifies it.

Standard: IEC 61508 SIL 4 / ISO 26262 ASIL D

---

## Legend

| Column     | Meaning |
|------------|---------|
| REQ-ID     | Unique requirement identifier |
| Requirement | Short description |
| Module     | Source file(s) that implement the requirement |
| Test(s)    | Unit test name(s) or system test case ID |
| Status     | IMPL = implemented, PARTIAL = partial, OPEN = not yet |

---

## REQ-MEM — Memory Management

| REQ-ID      | Requirement | Module | Test(s) | Status |
|-------------|-------------|--------|---------|--------|
| REQ-MEM-001 | Physical allocator returns 4 KB-aligned frames | `memory/physical.rs` | `test_alloc_sequential`, `test_alloc_rounds_up_to_page` | IMPL |
| REQ-MEM-002 | Physical allocator returns None when exhausted | `memory/physical.rs` | `test_alloc_oom` | IMPL |
| REQ-MEM-003 | Virtual map succeeds for valid VA/PA/flags triple | `memory/paging.rs` | `test_map_and_translate_basic`, `test_map_with_offset` | IMPL |
| REQ-MEM-004 | Translate returns None for unmapped addresses | `memory/paging.rs` | `test_translate_absent_pml4`, `test_translate_absent_pdpt`, `test_translate_absent_pd`, `test_translate_absent_pt` | IMPL |
| REQ-MEM-005 | Flags are preserved after map | `memory/paging.rs` | `test_flags_preserved` | IMPL |
| REQ-MEM-006 | Shared-frame aliases are only created by explicit SYS_MAP_SHARE | `memory/paging.rs`, `syscall.rs` | `test_map_share_and_cap` | IMPL |
| REQ-MEM-007 | Intermediate page-table entries are reused (no leak) | `memory/paging.rs` | `test_shared_intermediate_table_reuse` | IMPL |
| REQ-MEM-008 | Unmap clears PTE; subsequent translate returns None | `memory/paging.rs` | `test_unmap_clears_entry`, `test_unmap_absent_returns_false` | IMPL |
| REQ-MEM-009 | remap_page_flags preserves physical address | `memory/paging.rs` | `test_remap_flags_preserves_phys` | IMPL |
| REQ-MEM-010 | Kernel .text mapped without WRITABLE flag (CR0.WP enforcement) | `arch-x86_64/cpu/mod.rs`, `memory/paging.rs` | TC-SYS-001 (boot — no write-fault) | IMPL |
| REQ-MEM-011 | Stack canary detects frame overflows (SSP) | `core-kernel/stack_guard.rs` | build-time: `__security_cookie` + `__security_check_cookie` linked | IMPL |

---

## REQ-PROC — Process Management

| REQ-ID       | Requirement | Module | Test(s) | Status |
|--------------|-------------|--------|---------|--------|
| REQ-PROC-001 | Process table assigns unique PIDs | `process/table.rs` | `test_unique_pids` | IMPL |
| REQ-PROC-002 | Terminated slot is cleared and reused | `process/table.rs` | `test_terminate_clears_slot`, `test_slot_reuse_after_terminate` | IMPL |
| REQ-PROC-003 | Kernel stack is zeroed on free | `process/pcb.rs` | `test_stack_zeroed_on_free` | IMPL |
| REQ-PROC-004 | Kernel stack slot is automatically returned on process drop | `process/pcb.rs` | `test_kernel_stack_reclaim_lifo` | IMPL |
| REQ-PROC-005 | Priority-ceiling inheritance: blocked sender donates priority | `scheduler/round_robin.rs` | `test_priority_inheritance` | IMPL |
| REQ-PROC-006 | EDF: process with nearest deadline preempts lower-priority | `scheduler/round_robin.rs` | `test_edf_preempts_best_effort`, `test_edf_earliest_deadline_first` | IMPL |
| REQ-PROC-007 | EDF period renewal: deadline advances by period (no drift) | `scheduler/round_robin.rs` | `test_edf_period_renewal` | IMPL |
| REQ-PROC-008 | Ring-3 fault (#DE/#GP/#PF) terminates process; init notified | `interrupts/handlers.rs` | TC-SYS-005 (crash-log written) | IMPL |

---

## REQ-IPC — Inter-Process Communication

| REQ-ID      | Requirement | Module | Test(s) | Status |
|-------------|-------------|--------|---------|--------|
| REQ-IPC-001 | Messages delivered in FIFO order | `ipc/message.rs` | `test_fifo_order` | IMPL |
| REQ-IPC-002 | Full queue rejects send | `ipc/message.rs` | `test_full_rejects_send` | IMPL |
| REQ-IPC-003 | Queue wraps correctly at capacity boundary | `ipc/message.rs` | `test_circular_wrap` | IMPL |
| REQ-IPC-004 | Notification word is OR'd; consume clears it | `ipc/message.rs` | `test_notification_or_consume` | IMPL |
| REQ-IPC-005 | Send requires Channel capability (no raw-PID IPC) | `process/table.rs`, `syscall.rs` | `test_cap_grant_copies_to_target`, `test_cap_revoke_clears_slot` | IMPL |
| REQ-IPC-006 | Capability table full → cap_alloc returns None | `process/table.rs` | `test_cap_table_full_returns_none` | IMPL |
| REQ-IPC-007 | SYS_LOOKUP_CAP returns Channel cap slot, not raw PID | `syscall.rs` | code-review; QEMU: PID-2 channel observable from shell | IMPL |

---

## REQ-TIMER — Timer & Real-Time

| REQ-ID        | Requirement | Module | Test(s) | Status |
|---------------|-------------|--------|---------|--------|
| REQ-TIMER-001 | HPET ACPI table is parsed correctly | `acpi/hpet.rs` | 8 unit tests (`test_valid_10mhz_qemu`, ...) | IMPL |
| REQ-TIMER-002 | HPET counter is monotonically increasing | `hpet.rs` | `test_period_fs_stored_by_init` | IMPL |
| REQ-TIMER-003 | TSC calibration produces correct kHz | `timer/tsc.rs` | `test_compute_khz_3ghz_10ms` | IMPL |
| REQ-TIMER-004 | Deadline blocked processes are woken within 1 tick | `scheduler/round_robin.rs`, `cpu/mod.rs` | `test_ticks_until_next_event_no_blocked` | IMPL |
| REQ-TIMER-005 | Watchdog kicked within 10 s window | `hal/watchdog.rs` | TC-SYS-001 (no reset in 10 s) | IMPL |

---

## REQ-SAFE — Kernel Safety & Integrity

| REQ-ID       | Requirement | Module | Test(s) | Status |
|--------------|-------------|--------|---------|--------|
| REQ-SAFE-001 | Crash log written before halt on #GP/#PF/#DF/#MC | `crash_log.rs`, `interrupts/handlers.rs` | TC-SYS-005 | IMPL |
| REQ-SAFE-002 | Crash log survives warm reset (magic header) | `crash_log.rs` | `drain()` called at Stage 1 | IMPL |
| REQ-SAFE-003 | Single-core enforcement at boot | `main.rs` | `check_cpu_features()` | IMPL |
| REQ-SAFE-004 | OOM → safe halt (no undefined behaviour) | `main.rs` (BumpAllocator) | alloc path logs + halts | IMPL |
| REQ-SAFE-005 | IOMMU DMA remapping prevents device aliasing | `iommu/mod.rs` | 4 unit tests + ACPI DMAR tests | IMPL |
| REQ-SAFE-006 | Reproducible binary (SOURCE_DATE_EPOCH + remap-path-prefix) | `scripts/build.sh` | double-build + SHA-256 compare | IMPL |

---

## REQ-ACPI — Firmware / ACPI Discovery

| REQ-ID       | Requirement | Module | Test(s) | Status |
|--------------|-------------|--------|---------|--------|
| REQ-ACPI-001 | MADT parsed; all LAPIC/IOAPIC entries found | `acpi/madt.rs` | 6 unit tests | IMPL |
| REQ-ACPI-002 | FADT parsed; PM timer and reset register valid | `acpi/fadt.rs` | 9 unit tests | IMPL |
| REQ-ACPI-003 | DMAR parsed; all DRHD units enumerated | `acpi/dmar.rs` | 9 unit tests | IMPL |
| REQ-ACPI-004 | HPET table parsed; base address and period valid | `acpi/hpet.rs` | 8 unit tests | IMPL |

---

## REQ-SYSCALL — Syscall Interface

| REQ-ID        | Requirement | Module | Test(s) | Status |
|---------------|-------------|--------|---------|--------|
| REQ-SYS-001   | User-supplied pointers validated before dereference | `syscall.rs` (`validate_user_ptr`) | code review; pointer in non-canonical range → EINVAL | IMPL |
| REQ-SYS-002   | SYS_EXIT terminates calling process cleanly | `syscall.rs` | TC-SYS-003 (shell PID exits) | IMPL |
| REQ-SYS-003   | SYS_SPAWN requires valid entry/pml4/priority | `syscall.rs` | EINVAL returned for 0 entry | IMPL |
| REQ-SYS-004   | SYS_INJECT_FAULT only available with fault-injection feature | `syscall.rs` | build without feature: syscall 25 → ENOSYS | IMPL |

---

## Formal Verification Cross-Reference

| Property | TLA+ Module | Invariant / Theorem |
|----------|-------------|---------------------|
| Scheduler liveness (no starvation) | `specs/Scheduler.tla` | `NoStarvation` |
| At most one running process | `specs/Scheduler.tla` | `AtMostOneRunning` |
| Capability confinement (IPC) | `specs/IPC.tla` | `NeverSendWithoutCap` |
| Private frame exclusivity | `specs/Memory.tla` | `FrameIsolation`, `PrivateFrameExclusive` |

Run TLC model checker:
```
java -jar tla2tools.jar -config specs/Scheduler.cfg specs/Scheduler.tla
java -jar tla2tools.jar -config specs/IPC.cfg      specs/IPC.tla
java -jar tla2tools.jar -config specs/Memory.cfg   specs/Memory.tla
```
