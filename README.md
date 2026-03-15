# Rost

A minimal x86_64 UEFI microkernel written in Rust (`no_std`).

The kernel does exactly 8 things. Everything else — filesystems, drivers, applications — runs as isolated userspace processes communicating through IPC.

---

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                      User Applications                        │ Ring 3
├───────────────────────────────────────────────────────────────┤
│          Userspace Servers  (filesystem, drivers, etc.)       │ Ring 3
├───────────────────────────────────────────────────────────────┤
│  ═══════════════════  Syscall Boundary  ═══════════════════   │
├───────────────────────────────────────────────────────────────┤
│                   Rost Microkernel  (Ring 0)                  │
│                                                               │
│   1. Boot & UEFI     2. Memory mgmt     3. Interrupts         │
│   Hardware discovery  Paging / VMM       Handle HW events     │
│                                                               │
│   4. Processes       5. Scheduling      6. IPC                │
│   Create/destroy     Context switch     Send/receive msgs     │
│   address spaces     between procs      between processes     │
│                                                               │
│   7. System calls    8. Timer                                 │
│   Userspace→kernel   Drive scheduling                         │
│                                                               │
├───────────────────────────────────────────────────────────────┤
│                  UEFI Firmware / Hardware                      │
└──────────────────────────────────────────────────────────────┘
```

---

## Project Structure

Cargo workspace with 4 crates, each with a single responsibility:

```
crates/
├── kernel/                         # Binary — UEFI entry point, boot sequence, shell
│   └── src/
│       ├── main.rs                 # efi_main, BumpAllocator, BOOT_INFO static, init sequence
│       ├── boot_collector.rs       # Collects all UEFI hardware data before ExitBootServices
│       └── shell/
│           ├── mod.rs              # Interactive read loop, prompt, history navigation
│           ├── commands.rs         # Command dispatch, tab completion
│           ├── line_editor.rs      # In-place line buffer with movable cursor
│           ├── history.rs          # Circular command history (32 entries)
│           └── escape.rs           # VT100/xterm escape sequence parser → Key enum
│
├── arch-x86_64/                    # x86_64 hardware layer
│   └── src/
│       ├── context.rs              # switch_context — naked asm voluntary context switch
│       ├── cpu/
│       │   ├── gdt.rs              # GdtEntry, GlobalDescriptorTable (7 entries: ring-0/3 + TSS)
│       │   ├── idt.rs              # IdtEntry, InterruptDescriptorTable (256 gates, IST support)
│       │   ├── tss.rs              # TaskStateSegment, IST stacks, init_tss, set_rsp0, load_tss
│       │   ├── syscall.rs          # SYSCALL/SYSRET init, naked entry stub, Rust dispatcher
│       │   └── mod.rs              # enable/disable interrupts, halt, rdmsr/wrmsr,
│       │                           # read_cr2, activate_page_table, set_rsp0, load_tss
│       ├── interrupts/
│       │   ├── handlers.rs         # Naked ISR stubs + ExceptionFrame + register dump;
│       │   │                       # dedicated #DE/#GP/#PF/#DF/#NMI/#MC + 200 catch-all stubs
│       │   └── mod.rs              # TICK_COUNT (AtomicU64), init() — wires all 256 IDT vectors
│       └── timer/
│           ├── pic.rs              # PIC 8259 master/slave initialisation
│           └── mod.rs              # PIT channel 0 at 100 Hz
│
├── hal/                            # Hardware Abstraction Layer — device drivers
│   └── src/
│       └── uart.rs                 # COM1 UART: init, put_byte, read_byte,
│                                   #            print_str, print_hex, print_dec
│
└── core-kernel/                    # Architecture-independent kernel logic
    └── src/
        ├── boot_info.rs            # BootInfo + all hardware description structs
        ├── memory/
        │   ├── physical.rs         # PhysicalAllocator (4 KB-aligned bump)
        │   └── paging.rs           # 4-level PML4 page tables, map_page, translate_address
        ├── process/
        │   ├── mod.rs              # ProcessId, re-exports TaskContext
        │   ├── pcb.rs              # TaskContext (#[repr(C)]), ProcessControlBlock,
        │   │                       # KERNEL_STACKS (32 × 8 KB BSS), alloc_kernel_stack;
        │   │                       # quota fields: memory_quota_pages, cpu_budget_ticks,
        │   │                       # ipc_rate_limit, total_cpu_ticks, blocked_deadline
        │   └── table.rs            # ProcessTable (max 32); get_ready_with_priority,
        │                           # check_deadlines, reset_ipc_rate_counters; terminate frees slot
        ├── scheduler/
        │   └── round_robin.rs      # Priority scheduler (lowest number = highest priority);
        │                           # round-robin within level; IPC timeout, 64-entry audit log,
        │                           # temporal quota, set_priority, set_quotas, cpu_time_for
        └── ipc/
            └── message.rs          # Message (Copy+Clone), MessageQueue (cap 16, len()),
                                    # Notification; notify(word)/poll_notification()

build/
└── efi/boot/
    └── bootx64.efi                 # Deployed UEFI binary (output of scripts/build.sh)

scripts/
├── build.sh                        # cargo build + copy .efi to build/
└── run.sh                          # QEMU launch command
```

**Crate dependency graph:**
```
kernel  ──►  arch-x86_64
        ──►  hal
        ──►  core-kernel
        ──►  uefi  (0.26)

arch-x86_64  ──►  hal
             ──►  core-kernel
hal          ──►  (nothing)
core-kernel  ──►  (nothing, uses alloc)
```

`core-kernel` has no architecture or hardware dependencies — its logic can be unit-tested on the host with `cargo test -p core-kernel --target <host>`.

---

## Boot Sequence

On every boot, the kernel runs an 8-stage initialisation sequence printed over serial:

```
[0/7] UEFI Hardware Discovery      ← collects all hardware info before UEFI exits; CPU feature check
[1/7] Memory Management            ← PhysicalAllocator seeded from real UEFI map; CR3 loaded
[2/7] CPU Setup (GDT/IDT)          ← GDT (7 entries incl. TSS), TSS loaded (ltr 0x28), SYSCALL MSRs
[3/7] Interrupt Handlers           ← All 256 IDT vectors: dedicated + catch-all + IST for #DF/#NMI/#MC
[4/7] System Timer                 ← PIT 100 Hz + PIC; TICK_COUNT AtomicU64
[5/7] Process Management           ← create_process, kernel stacks, quota fields, TSS.RSP0 set
[6/7] Scheduler                    ← Priority-based; send/receive demo; audit log
[7/7] Inter-Process Communication  ← Notification; blocking receive with timeout
```

Stage 0 example output:
```
[0/7] UEFI Hardware Discovery
      ├─ Firmware:        American Megatrends UEFI 2.70
      ├─ CPU vendor:      GenuineIntel
      ├─ CPU brand:       Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz
      ├─ CPU addr bits:   phys=39 virt=48
      ├─ Memory regions:  47 entries (12 usable)
      ├─ Usable RAM:      512 MiB
      ├─ Display (GOP):   1024x768 @ 0x00000000C0000000  [1 output(s)]
      ├─ ACPI RSDP:       0x000000007FE14014 (v2)
      ├─ SMBIOS:          0x000000007F3B7000 (v3)
      ├─ Secure Boot:     Disabled
      ├─ Boot time:       2026-03-10 05:08:31
      └─ Status:          ✓ OK
```

---

## The 8 Kernel Modules

### 1. Boot & UEFI — `crates/kernel/src/main.rs`, `boot_collector.rs`

Brings the CPU to a known state and captures hardware information before UEFI exits.

- UEFI entry via `#[entry]` — the `.efi` binary IS the kernel, no bootloader needed
- `boot_collector::collect()` queries every UEFI protocol while boot services are live
- Result stored in `static mut BOOT_INFO` — accessible to every subsystem forever
- Serial console: COM1 at 38400 baud 8N1, port I/O — works in long mode unconditionally
- CPU feature verification: `check_cpu_features()` asserts SMEP and MSR availability at boot; halts if missing
- GDT: **7 entries** — null, ring-0 code (`0x08`), ring-0 data (`0x10`), ring-3 data (`0x18`), ring-3 code (`0x20`), TSS low (`0x28`), TSS high — 16-byte system descriptor encoding TSS base + limit
- TSS: `TaskStateSegment` (#[repr(C, packed)], 104 bytes); IST1/IST2 stacks for #DF/#NMI; loaded via `ltr 0x28`; `set_rsp0(rsp0)` called after process creation for ring-3 safety
- IDT: 256 gates; dedicated handlers for #DE, #NMI (IST2), #DF (IST1), #GP, #PF, #MC (IST3), IRQ0, spurious (255); macro-generated catch-all stubs for all remaining vectors
- SYSCALL/SYSRET MSRs (EFER, STAR, LSTAR, SFMASK) configured at stage 2

**What's done:** UEFI boot, full hardware discovery, 7-entry GDT, TSS loaded, all 256 IDT vectors, SYSCALL setup, CPU feature checks, serial console, interactive shell
**What's missing:** `ExitBootServices()` call (UEFI firmware still running in background)

---

### 2. Memory management — `crates/core-kernel/src/memory/`

Owns all physical memory and virtual address spaces.

- Physical allocator: 4 KB-aligned bump (`physical.rs`), seeded from the largest usable UEFI region
- Page tables: full 4-level PML4→PDPT→PD→PT (`paging.rs`); intermediate tables allocated on demand from `PhysicalAllocator`
- `map_page(pml4, virt, phys, writable, alloc)` — walks/allocates all four levels (4 KB granularity)
- `identity_map_region(pml4, base, size, flags, alloc)` — maps a physical region using **2 MB huge pages** (PD-level PS bit); a 4 GB system needs only ~20 KB of page-table space
- `translate_address(pml4, virt)` — walks the live page table to resolve virtual → physical
- `activate_page_table(pml4_phys)` — writes CR3; `init_protection()` — sets EFER.NXE, CR0.WP, CR4.SMEP, CR4.SMAP
- `static mut KERNEL_PML4` — 4 KB-aligned BSS static; all UEFI memory regions identity-mapped at Stage 1; CR3 loaded before Stage 2
- `switch_context(old, new, new_pml4)` — third argument conditionally reloads CR3 (`test rdx/jz/mov cr3,rdx`); pass `0` when tasks share an address space

**What's done:** bump allocator, 4-level page tables, 2 MB huge-page identity mapping, CR3 loaded at boot, NXE + WP + SMEP + SMAP enabled, CR3 switch on context switch
**What's missing:** buddy/free-list allocator, per-process PML4 (plumbing in place — all processes share kernel PML4 until ELF loader lands), NX bit applied to data pages

---

### 3. Interrupt handling — `crates/arch-x86_64/src/interrupts/`

Handles all CPU exceptions and hardware interrupts — all 256 IDT vectors populated.

| Vector | Source | Handler | Stack |
|--------|--------|---------|-------|
| 0 | #DE Division by zero | Full GPR dump → serial → halt | normal |
| 2 | #NMI Non-maskable interrupt | Log + return (non-fatal) | IST2 |
| 8 | #DF Double fault | Log fatal → halt | IST1 |
| 13 | #GP General protection fault | Full GPR dump → serial → halt | normal |
| 14 | #PF Page fault | CR2 + full GPR dump → serial → halt | normal |
| 18 | #MC Machine check | Log + halt | IST3 |
| 32 | IRQ0 PIT timer (100 Hz) | `lock inc TICK_COUNT` + EOI → `iretq` | normal |
| 255 | Spurious interrupt | Single `iretq` — no EOI | normal |
| 1–254 (others) | Unexpected vector | Log vector + frame + EOI → `iretq` (catch-all) | normal |

IST (Interrupt Stack Table) stacks are static 8 KB BSS regions dedicated to #DF (IST1) and #NMI (IST2). They guarantee a valid stack even when the kernel stack itself is corrupt.

All handlers use `#[unsafe(naked)]` — required on stable Rust (no `abi_x86_interrupt`). Exception stubs push all 15 GPRs plus a dummy/real error code, forming an `ExceptionFrame` on the stack. The Rust handler inspects `frame.cs & 3 == 3` to distinguish user-mode from kernel-mode faults before printing the dump.

Timer ISR saves 9 caller-saved registers, atomically increments `TICK_COUNT`, sends EOI, restores — no function call overhead.

**What's done:** All 256 IDT vectors registered; dedicated handlers for #DE/#GP/#PF/#DF/#NMI/#MC; IST stacks for #DF and #NMI; user/kernel fault origin printed; catch-all stubs for every unhandled vector; spurious IRQ handler; TICK_COUNT incremented at 100 Hz
**What's missing:** Preemptive context switch from timer ISR (deferred — scheduler invoked cooperatively from main loop)

---

### 4. Process management — `crates/core-kernel/src/process/`

Creates and destroys processes; owns per-process state.

- `TaskContext` (`pcb.rs`): `#[repr(C)]` struct with all 15 GPRs + rsp + rip + rflags; field offsets are load-bearing — `switch_context` asm indexes by byte offset
- `ProcessControlBlock`: holds `TaskContext`, kernel stack ID + top, `page_table_base`, time-slice counters, per-process `MessageQueue` mailbox, and resource quotas:
  - `memory_quota_pages: u32` — max physical pages the process may hold
  - `cpu_budget_ticks: u32` / `cpu_budget_used: u32` — temporal partition budget (0 = unlimited)
  - `ipc_rate_limit: u16` / `ipc_rate_used: u16` — IPC sends per 100-tick window (0 = unlimited)
  - `total_cpu_ticks: u64` — lifetime CPU accounting
  - `blocked_deadline: u64` — tick at which a blocked process is auto-unblocked (u64::MAX = no timeout)
  - `priority: u8` — 0 = highest, 255 = idle; default 128
- `KERNEL_STACKS`: `static mut [[u8; 8192]; 32]` — 256 KB in BSS, allocated atomically with `AtomicUsize`; entry point written to `[kern_rsp]` so `switch_context`'s `ret` jumps there on first run
- `ProcessTable` (`table.rs`): fixed array of up to 32 slots; `terminate_process` sets slot to `None` (reclaims table slot immediately); `get_ready_with_priority()` returns `(pid, priority)` pairs for the scheduler; `check_deadlines(tick)` auto-unblocks timed-out processes; `reset_ipc_rate_counters()` clears per-window counters every 100 ticks

**What's done:** Full `TaskContext`, per-process kernel stack (32 × 8 KB), per-process mailbox, create/terminate (slot reclaim), resource quota fields, deadline unblocking, IPC rate counters, `page_table_base` through to `switch_context`
**What's missing:** Per-process PML4 allocation (all processes share kernel PML4 until ELF loader), ring-3 entry, guard pages per kernel stack

---

### 5. Scheduling — `crates/core-kernel/src/scheduler/`

Decides which process runs next; performs the context switch.

- **Priority-based preemptive scheduler** (`round_robin.rs`): lowest priority number wins (0 = highest). Round-robin within the same priority level via `queue_index`. Idle process convention: register with priority 255, runs only when nothing else is ready.
- `schedule()` — calls `pick_next_priority()`: finds minimum priority among all `Ready`/`Running` processes, then selects the next in round-robin order within that level
- `timer_tick()` — increments internal tick; calls `check_deadlines` to auto-unblock timed-out processes; resets IPC rate counters every 100 ticks; advances `cpu_time` and `total_cpu_ticks`; preempts when quantum **or** `cpu_budget_ticks` expires; returns `(*mut TaskContext, *const TaskContext, u64 new_pml4)` for the arch layer to call `switch_context`
- `set_priority(pid, u8)` — adjusts process priority at runtime
- `set_quotas(pid, memory_pages, cpu_budget, ipc_rate)` — configures per-process resource limits
- `send_message(from_pid, to_pid, msg)` — stamps `msg.sender = from_pid` (prevents forgery); checks sender rate limit; deposits message; transitions `Blocked` → `Ready`; records audit entry
- `blocking_receive(pid, timeout_ticks)` — tries the mailbox; if empty, sets `Blocked` + `blocked_deadline = tick + timeout_ticks`; records audit entry
- `terminate_process(pid)` — reclaims table slot; clears `current_process`; records audit entry
- `audit_entries()` — returns a snapshot of the 64-entry IPC ring buffer (`AuditEntry { tick, kind, sender, target }`)
- `cpu_time_for(pid)` — returns lifetime CPU ticks consumed
- `#[cfg(debug_assertions)] check_invariants()` — verifies every ready PID is in `Ready`/`Running` state and `current_process` is present in the table

The actual register save/restore is in `arch_x86_64::context::switch_context` — a naked asm function that saves callee-saved registers + rsp into the old context, restores from the new context, and conditionally reloads CR3.

**What's done:** Priority-based round-robin, temporal partitioning (cpu_budget), IPC timeout + auto-unblock, CPU accounting, 64-entry audit log, sender-PID stamping, IPC rate limiting, invariant assertions; global scheduler (`GLOBAL_SCHEDULER` static, `CURRENT_PID` AtomicU32), idle process (priority 255, hlt loop), deferred preemption at hlt boundaries via `tick_scheduler()`; TSS.RSP0 updated on every context switch
**What's missing:** True ISR-level preemption (saving full GPR frame inside timer ISR); per-process PML4 activation

---

### 6. IPC — `crates/core-kernel/src/ipc/`

Passes messages between processes — the only way userspace servers communicate.

- `Message` (`message.rs`): `sender: ProcessId` + `payload: [u64; 8]` (72 bytes, Copy+Clone)
- `MessageQueue`: circular buffer, capacity 16; `send(msg)`, `receive() -> Option<Message>`, `len()`, `is_full()`
- **Sender-PID stamping**: the kernel overwrites `msg.sender` in `send_message(from_pid, ...)` before enqueue — userspace cannot forge the sender identity
- **IPC rate limiting**: each sender has an `ipc_rate_limit/ipc_rate_used` counter pair; `send_message` rejects sends that exceed the limit (counters reset every 100 ticks)
- **Notification object**: `MessageQueue::notify(word: u64)` — ORs bits into `pending_notification` (lightweight signal without mailbox slot); `poll_notification() -> Option<u64>` atomically consumes the word
- **Blocking receive with timeout**: `blocking_receive(pid, timeout_ticks)` parks the process; `check_deadlines(tick)` in `timer_tick` auto-unblocks it when the deadline passes
- **IPC audit log**: 64-entry ring buffer in `Scheduler`; records `Send`, `Receive`, `Block`, `Unblock`, `Terminate` events with tick, sender u32, target u32

**What's done:** Message + circular queue, sender stamping, rate limiting, notification word, IPC timeout, 64-entry audit log; blocking receive integrated with scheduler; `notify_process()` for lightweight event signalling
**What's missing:** Capability-based endpoints (raw-PID addressing currently used), synchronous call/reply

---

### 7. System calls — `crates/arch-x86_64/src/cpu/syscall.rs`

The only legal crossing point from ring 3 into ring 0.

Implementation:
- `SYSCALL`/`SYSRET` via EFER.SCE, STAR, LSTAR, SFMASK MSRs — initialised at boot
- Naked assembly `syscall_entry` stub — saves callee-saved + rcx/r11, moves r10 → rcx (4th arg), calls `dispatch_syscall`, restores registers, executes `sysretq`
- GDT extended to 7 entries (incl. TSS) so SYSRET loads ring-3 CS (0x20) and SS (0x18)
- `dispatch_syscall(number, a0..a5) -> u64` — `extern "C"` Rust function; routes rax to handler; returns value placed in rax
- Error codes: `ENOSYS = u64::MAX` (−1), `EINVAL = u64::MAX−1` (−2), `EPERM = u64::MAX−2` (−3)

Current syscall table:

| # | Name | Status | Description |
|---|------|--------|-------------|
| 0 | `sys_yield` | `[x]` stub | Cooperative yield — returns 0 immediately |
| 1 | `sys_exit(code)` | `[x]` stub | Logs exit code over serial; scheduler will deschedule |
| 2 | `sys_getpid` | `[x]` stub | Returns 0 (placeholder until per-CPU GSBASE) |
| 3 | `sys_send(to_pid, msg_ptr)` | `[~]` ENOSYS | Not yet wired to Scheduler |
| 4 | `sys_recv(timeout_ticks)` | `[~]` ENOSYS | Not yet wired to Scheduler |
| 5 | `sys_notify(to_pid, word)` | `[~]` ENOSYS | Not yet wired to Scheduler |

**What's done:** MSRs configured, naked entry stub, `dispatch_syscall` dispatcher, all 6 syscalls implemented and wired to global Scheduler; `CURRENT_PID` used for SYS_GETPID; TSS.RSP0 updated per context switch
**What's missing:** Syscall argument pointer validation against user address range; TSS.RSP0 kernel-stack switch at the `syscall` instruction boundary (requires ring-3 process entry); SYS_RECV uses retry-on-unblock rather than true kernel blocking

---

### 8. Timer — `crates/arch-x86_64/src/timer/`

Provides the heartbeat that drives the scheduler.

- PIT channel 0 at 100 Hz (10 ms ticks), divisor 11932 (`mod.rs`)
- PIC master (0x20) and slave (0xA0) initialised, IRQ0 unmasked (`pic.rs`)
- IRQ0 → IDT vector 32 → `timer_interrupt_handler`

**What's done:** PIT + PIC fully configured, timer ISR fires at 100 Hz; atomically increments `TICK_COUNT` (`arch_x86_64::interrupts::TICK_COUNT`)
**What's missing:** Preemptive context switch directly from ISR (ISR currently saves only caller-saved registers — full GPR save + CR3 switch is the next step)

---

## Hardware Discovery (`BootInfo`)

`core_kernel::boot_info::BootInfo` is the single source of truth for all hardware
information gathered from UEFI. It lives in a `static` and is valid for the entire
kernel lifetime. Every future subsystem reads from it directly — no re-querying UEFI.

| Field | Type | Contents |
|-------|------|----------|
| `memory_map` | `MemoryMap` | All physical regions with `MemoryKind` classification |
| `total_memory_bytes` | `u64` | Sum of all `Usable` bytes (convenience cache) |
| `displays` | `DisplayList` | Up to 4 GOP framebuffers (base, size, resolution, stride, format) |
| `acpi` | `Option<AcpiInfo>` | RSDP physical address + ACPI version (1 or 2) |
| `smbios` | `Option<SmbiosInfo>` | Entry-point address + version (2=32-bit, 3=64-bit) |
| `firmware` | `FirmwareInfo` | OEM vendor string, UEFI revision (major.minor), firmware revision |
| `cpu` | `CpuInfo` | Vendor, brand string, family/model/stepping, address bits, feature flags |
| `secure_boot` | `SecureBootState` | `Enabled` / `Disabled` / `SetupMode` / `Unknown` |
| `load_options` | `LoadOptions` | Kernel command line from boot manager |
| `boot_time` | `Option<BootTime>` | Wall-clock time at kernel entry |

`CpuFeatures` exposes individual bit-flag helpers: `has_sse()`, `has_sse2()`,
`has_avx()`, `has_avx2()`, `has_aes()`, `has_smep()`, `has_smap()`, `has_sha()`, and more.

---

## Shell

An interactive shell starts after the init sequence completes. Connect via `-serial stdio` in QEMU.

**Built-in commands:**
```
rost > help
Built-in commands:
  clear              clear the screen
  echo <args...>     print arguments to the console
  halt               halt the system
  help               show this help message
  history            list command history

Line editing:
  Left / Right       move cursor one character
  Home / End         jump to start or end of line
  Backspace          delete character before cursor
  Delete             delete character at cursor
  Up / Down          browse command history
  Tab                complete command name
  Ctrl+C             cancel current line
  Ctrl+L             clear screen
```

**Features:**
- Full in-place line editing: insert/delete at any cursor position
- ANSI/VT100 escape sequence parser handles arrow keys, Home, End, Delete
- Persistent command history (32 entries, circular, skips duplicates)
- Tab completion for all built-in commands (shows candidates on ambiguous prefix)
- `echo` tokenizer supports double-quoted strings with spaces
- CPU idles between keystrokes (`hlt`) — no busy-waiting

---

## Requirements

| Tool | Notes |
|------|-------|
| Rust stable ≥ 1.85 | `#[unsafe(naked)]` stabilised in 1.85 |
| `x86_64-unknown-uefi` target | `rustup target add x86_64-unknown-uefi` |
| QEMU (MacPorts) | `sudo port install qemu` |
| EDK2 firmware | Bundled with MacPorts QEMU at `/opt/local/share/qemu/edk2-x86_64-code.fd` |

---

## Build & Run

**Build and deploy the EFI binary:**

```sh
./scripts/build.sh
# Compiles all workspace crates and copies Rost.efi → build/efi/boot/bootx64.efi

./scripts/build.sh --release
# Release build (LTO, stripped)
```

**Run in QEMU:**

```sh
./scripts/run.sh
```

Which expands to:

```sh
qemu-system-x86_64 \
  -machine q35 \
  -accel hvf \
  -cpu host \
  -m 512M \
  -drive if=pflash,format=raw,readonly=on,file=/opt/local/share/qemu/edk2-x86_64-code.fd \
  -drive format=raw,file=fat:rw:build/ \
  -net none \
  -serial stdio
```

Kernel output and the shell appear in the **terminal** (serial stdio). The QEMU window
will be blank until a GOP framebuffer console is implemented.

Exit QEMU: `Ctrl+A` then `X`.

**Real hardware:** Copy `build/efi/boot/bootx64.efi` to a FAT32 USB drive at
`EFI/BOOT/BOOTX64.EFI` and boot with Secure Boot disabled.

---

## Roadmap

Ordered by dependency — each phase unlocks the next.

```
Phase 1 — Memory  (unblocks everything else)
  [ ] Call ExitBootServices; take full ownership of physical memory
  [x] 4-level page tables (PML4→PDPT→PD→PT) — structures + map/translate implemented
  [x] identity_map_region — 2 MB huge pages, all UEFI memory regions
  [x] KERNEL_PML4 loaded into CR3 at boot (Stage 1)
  [x] Hardware protection: EFER.NXE + CR0.WP + CR4.SMEP + CR4.SMAP enabled
  [x] OOM safe-state: BumpAllocator logs over serial and halts on alloc failure
  [ ] Buddy/free-list allocator replacing bump allocator
  [~] Per-process address spaces — plumbing done; per-process PML4 deferred to ELF loader

Phase 2 — CPU Mechanics
  [x] Exception handlers: full register dump (all 15 GPRs + rip/rflags/error_code) over serial
  [x] All 256 IDT vectors populated: dedicated + macro catch-all stubs
  [x] #NMI (IST2), #DF (IST1), #MC (IST3) — dedicated IST stacks; never corrupt on double-fault
  [x] User-mode vs kernel-mode fault origin printed (cs & 3 == 3 check)
  [x] Spurious IRQ handler (vector 255) — iretq with no EOI
  [x] TSS (#repr(C, packed), 104 B); GDT extended to 7 entries; ltr 0x28 at boot
  [x] Voluntary context switch: save/restore all callee-saved GPRs + rsp in assembly
  [x] switch_context takes new_pml4; conditionally reloads CR3 on address-space change
  [x] TICK_COUNT incremented atomically at 100 Hz by timer ISR
  [x] CPU feature check at boot (SMEP, MSR); halts if missing
  [~] Preemptive switch from timer ISR (ISR fires; global SCHEDULER not yet wired)
  [ ] GOP framebuffer console (visible output in QEMU window)

Phase 3 — Userspace Boundary
  [x] r8–r15 + all caller-saved in PCB TaskContext
  [x] Per-process kernel stacks (32 × 8 KB in BSS, allocated with AtomicUsize)
  [x] SYSCALL/SYSRET MSRs configured; naked entry stub; dispatch_syscall dispatcher
  [x] All 6 syscalls implemented: SYS_YIELD/EXIT/GETPID/SEND/RECV/NOTIFY
  [x] TSS.RSP0 updated on every context switch via tick_scheduler()
  [x] Global scheduler (GLOBAL_SCHEDULER static, init_global / get_global)
  [x] CURRENT_PID AtomicU32 — updated per switch; read by SYS_GETPID
  [ ] ELF loader (ring-3 process from image)
  [ ] Guard pages per kernel stack

Phase 4 — Process & Scheduling
  [x] Priority-based scheduler (lowest priority number = highest priority)
  [x] Round-robin within same priority level
  [x] Temporal partitioning: cpu_budget_ticks per process; preempted when budget exhausted
  [x] Lifetime CPU accounting: total_cpu_ticks per process
  [x] Resource quota fields: memory_quota_pages, ipc_rate_limit in PCB
  [x] terminate_process reclaims table slot (sets slot to None)
  [x] IPC deadline: blocked_deadline; check_deadlines auto-unblocks expired processes
  [x] IPC rate counters reset every 100 ticks
  [x] Invariant assertions (#[cfg(debug_assertions)])
  [x] Idle process (priority 255, hlt loop, registered at boot)
  [~] Deferred preemption: tick_scheduler() after hlt — not true ISR preemption
  [ ] Capability table in PCB
  [ ] Priority inheritance protocol

Phase 5 — IPC
  [x] Per-process MessageQueue embedded in PCB (mailbox field)
  [x] blocking_receive with timeout: parks process; auto-unblocked on deadline
  [x] send_message: kernel stamps sender PID (forgery prevention), checks rate limit
  [x] Notification word: notify(word) / poll_notification() — lightweight signal
  [x] notify_process(to_pid, word) on Scheduler — unblocks receiver
  [x] 64-entry IPC audit ring buffer (Send/Receive/Block/Unblock/Terminate events)
  [x] SYS_SEND/RECV/NOTIFY wired to global Scheduler

Phase 6 — Servers
  [ ] Init process (PID 1)
  [ ] VFS server (userspace)
```

---

## Design Notes

**Why UEFI, no bootloader?**
The `.efi` binary IS the kernel. UEFI provides memory map, framebuffer, ACPI, SMBIOS,
and FAT32 for free — no GRUB, no Limine, no second stage.

**Why capture hardware info upfront?**
UEFI boot services are only valid until `ExitBootServices()` is called. The
`boot_collector` queries everything while UEFI is alive and stores it in `BOOT_INFO`.
Every future subsystem (ACPI parser, framebuffer driver, memory manager) reads from
that static — no UEFI access needed after stage 0.

**Why serial for console?**
UEFI `SimpleTextOutput` is gone after `ExitBootServices()`. Serial port I/O works in
long mode unconditionally. GOP framebuffer driving is planned but not yet implemented.

**Why `#[unsafe(naked)]` for ISRs and context switch?**
`extern "x86-interrupt"` is nightly-only. Naked functions are stable since Rust 1.85
and give exact control over the `iretq` sequence and register save/restore order.
`switch_context` is naked for the same reason: Rust's calling convention must not
add any prologue/epilogue that would disturb the stack pointer before the `ret`.

**How does voluntary context switch work?**
`switch_context(old, new, new_pml4)` is a naked asm function (System V AMD64 ABI: `rdi=old`, `rsi=new`, `rdx=new_pml4`):
1. Saves callee-saved registers (rbx, rbp, r12–r15) and rsp into `*old`.
2. Restores callee-saved registers and rsp from `*new` (rdx is untouched by this step).
3. If `rdx != 0`: executes `mov cr3, rdx` to switch address spaces and flush the TLB.  Pass `0` when both tasks share the same PML4 to avoid a needless TLB shootdown.
4. Executes `ret`, which pops the return address from the new stack.

For a **new process**, `ProcessControlBlock::new()` writes the entry point at
`[kern_rsp]` (the top of its 8 KB kernel stack), so the first `ret` jumps there.
For a **resumed process**, the saved rsp still points to the return address left by
the original `call switch_context`, so `ret` resumes after that call.

**Why a TSS and IST stacks?**
The x86_64 Task State Segment (TSS) serves two purposes in Rost. First, it provides `RSP0` — the kernel-stack pointer that the CPU loads automatically when `SYSCALL` transitions from ring 3 to ring 0; without it, the kernel would execute on the *user* stack, a security hole. Second, the Interrupt Stack Table (IST) fields point to *dedicated* 8 KB stacks for critical vectors: #DF uses IST1 so a double-fault caused by a corrupt kernel stack still has a valid stack to run on; #NMI uses IST2 so a non-maskable interrupt never accidentally re-enters a partially-saved register frame.

**How does priority scheduling work?**
`Scheduler::pick_next_priority()` calls `ProcessTable::get_ready_with_priority()` to collect `(pid, priority)` pairs for all `Ready`/`Running` processes, finds the minimum priority number (0 = highest), then applies round-robin within that group via `queue_index`. This gives real-time-style preemption of lower-priority work while remaining fair within a level. Temporal isolation is enforced separately via `cpu_budget_ticks`: a process that exhausts its budget is preempted even if its quantum has not expired.

**Why static GDT and IDT?**
The CPU reads the GDT on every segment reload and the IDT on every interrupt.
Stack-allocated descriptors would be overwritten by interrupt nesting. `static` gives
them a fixed address for the kernel lifetime. GDT is `static mut` because `install_tss()`
must write the TSS base address at runtime; all accesses are wrapped in `unsafe` with
`addr_of_mut!`. IDT is `static mut` accessed via raw pointer.

**Why a Cargo workspace?**
Each crate has a single responsibility and a clean dependency boundary. `core-kernel`
has no architecture or hardware dependencies — its logic (scheduler, IPC, memory) can
be unit-tested on the host without QEMU. Adding a second architecture (`aarch64`)
requires only a new `arch-aarch64` crate; nothing else changes.

**Why only 8 kernel modules?**
Microkernel principle: a kernel bug is a security hole. Filesystems, drivers, and
protocols all crash in userspace — they can be restarted without rebooting. Only the
8 modules above require ring 0.

---

## References

- [OSDev Wiki](https://wiki.osdev.org) — GDT, IDT, PIC, PIT, paging, context switch
- [Intel SDM Vol. 3](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html) — SYSCALL/SYSRET, TSS, MSRs, CR3, CPUID
- [UEFI Specification](https://uefi.org/specifications) — GOP, memory map, config table, boot/runtime services
- [ACPI Specification](https://uefi.org/specifications) — RSDP, XSDP, MADT, FADT
- [SMBIOS Specification](https://www.dmtf.org/standards/smbios) — Entry-point structure, type 0/1/4 tables
- [seL4 whitepaper](https://sel4.systems/About/seL4-whitepaper.pdf) — microkernel IPC design
- [Rust Embedded Book](https://docs.rust-embedded.org/book/) — `no_std` patterns
