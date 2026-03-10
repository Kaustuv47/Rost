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
│       │   ├── gdt.rs              # GdtEntry, GlobalDescriptorTable (5 selectors: ring-0/3)
│       │   ├── idt.rs              # IdtEntry, InterruptDescriptorTable (256 gates)
│       │   ├── syscall.rs          # SYSCALL/SYSRET init + naked entry stub
│       │   └── mod.rs              # enable/disable interrupts, halt, rdmsr/wrmsr,
│       │                           # read_cr2, activate_page_table
│       ├── interrupts/
│       │   ├── handlers.rs         # Naked ISR stubs + ExceptionFrame + register dump
│       │   └── mod.rs              # TICK_COUNT (AtomicU64), init() — wires IDT
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
        │   │                       # KERNEL_STACKS (32 × 8 KB in BSS), alloc_kernel_stack
        │   └── table.rs            # ProcessTable (max 32 processes)
        ├── scheduler/
        │   └── round_robin.rs      # Round-robin Scheduler: schedule, timer_tick,
        │                           # send_message, blocking_receive, terminate_process
        └── ipc/
            └── message.rs          # Message (Copy+Clone), MessageQueue (capacity 16)

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
[0/7] UEFI Hardware Discovery      ← collects all hardware info before UEFI exits
[1/7] Memory Management            ← PhysicalAllocator seeded from real UEFI map
[2/7] CPU Setup (GDT/IDT)
[3/7] Interrupt Handlers
[4/7] System Timer
[5/7] Process Management
[6/7] Scheduler
[7/7] Inter-Process Communication
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
- GDT: 5 selectors — null, ring-0 code (`0x08`), ring-0 data (`0x10`), ring-3 data (`0x18`), ring-3 code (`0x20`)
- IDT: 256 gates, loaded from a `static` (not the stack)
- SYSCALL/SYSRET MSRs (EFER, STAR, LSTAR, SFMASK) configured at stage 2

**What's done:** UEFI boot, full hardware discovery, GDT/IDT, SYSCALL setup, serial console, interactive shell
**What's missing:** `ExitBootServices()` call (UEFI firmware still running in background)

---

### 2. Memory management — `crates/core-kernel/src/memory/`

Owns all physical memory and virtual address spaces.

- Physical allocator: 4 KB-aligned bump (`physical.rs`), seeded from the largest usable region in the UEFI memory map
- Page tables: full 4-level PML4→PDPT→PD→PT (`paging.rs`); intermediate tables allocated on demand from `PhysicalAllocator`
- `map_page(pml4, virt, phys, writable, alloc)` — walks/allocates all four levels
- `translate_address(pml4, virt)` — walks the live page table to resolve virtual → physical
- `activate_page_table(pml4_phys)` in `arch-x86_64::cpu` — loads CR3

**What's done:** bump allocator, 4-level page tables with dynamic allocation
**What's missing:** CR3 actually loaded (identity map not yet activated), buddy allocator, per-process address spaces

---

### 3. Interrupt handling — `crates/arch-x86_64/src/interrupts/`

Handles all CPU exceptions and hardware interrupts.

| Vector | Source | Handler |
|--------|--------|---------|
| 0 | #DE Division by zero | Full GPR dump → serial → halt |
| 13 | #GP General protection fault | Full GPR dump → serial → halt |
| 14 | #PF Page fault | CR2 + full GPR dump → serial → halt |
| 32 | IRQ0 PIT timer (100 Hz) | `lock inc TICK_COUNT` + EOI → `iretq` |

All handlers use `#[unsafe(naked)]` — required on stable Rust (no `abi_x86_interrupt`).

Exception stubs push all 15 GPRs plus a dummy/real error code, forming an `ExceptionFrame` on the stack, then call a regular `extern "C"` Rust handler that prints the dump and halts.

Timer ISR saves the 9 caller-saved registers, atomically increments `TICK_COUNT`, sends EOI, and returns — no function call, no calling-convention concerns.

**What's done:** All handlers registered; exceptions dump all 15 GPRs + rip + rflags + error code over serial before halting; TICK_COUNT (`AtomicU64`) incremented at 100 Hz
**What's missing:** Preemptive context switch from timer ISR (deferred — scheduler is invoked cooperatively)

---

### 4. Process management — `crates/core-kernel/src/process/`

Creates and destroys processes; owns per-process state.

- `TaskContext` (`pcb.rs`): `#[repr(C)]` struct with all 15 GPRs + rsp + rip + rflags; field offsets are load-bearing — `switch_context` asm indexes by byte offset
- `ProcessControlBlock`: holds `TaskContext`, kernel stack ID + top, `page_table_base`, time-slice counters, and a per-process `MessageQueue` mailbox
- `KERNEL_STACKS`: `static mut [[u8; 8192]; 32]` — 256 KB in BSS, allocated atomically with `AtomicUsize`; entry point written to `[kern_rsp]` so `switch_context`'s `ret` jumps there on first run
- `ProcessTable` (`table.rs`): fixed array of up to 32 slots
- `create_process(entry, stack)`, `terminate_process(pid)`, `get_ready_processes()`

**What's done:** Full `TaskContext` (all 15 GPRs + rsp + rip + rflags), per-process kernel stack (32 × 8 KB in BSS), per-process `MessageQueue` mailbox, create/terminate
**What's missing:** Address space isolation (CR3 per process), ring-3 entry, ELF loader

---

### 5. Scheduling — `crates/core-kernel/src/scheduler/`

Decides which process runs next; performs the context switch.

- Round-robin over all `Ready` processes (`round_robin.rs`)
- `schedule()` — selects the next ready PID in round-robin order
- `timer_tick()` — advances the current process's `cpu_time`; when the quantum expires marks it `Ready` and returns `(*mut TaskContext, *const TaskContext)` for the arch layer to call `switch_context`
- `send_message(to, msg)` — deposits a message and transitions `Blocked` → `Ready`
- `blocking_receive(pid)` — tries the mailbox; if empty sets state to `Blocked` (process won't be scheduled until a message arrives)
- `terminate_process(pid)` — marks process `Terminated`, clears `current_process`

The actual register save/restore is in `arch_x86_64::context::switch_context` — a naked asm function that saves callee-saved registers + rsp into the old context, restores from the new context, and executes `ret` to jump to the saved return address (or entry point on first run).

**What's done:** Round-robin selection, quantum tracking, voluntary context switch, send/receive with blocking, terminate
**What's missing:** Preemptive switch from timer ISR (requires ISR to save full GPR frame into TaskContext), CR3 switch per process, TSS.RSP0 setup

---

### 6. IPC — `crates/core-kernel/src/ipc/`

Passes messages between processes — the only way userspace servers communicate.

- `Message` (`message.rs`): sender PID + 8×u64 payload (72 bytes)
- `MessageQueue`: circular buffer, capacity 16, non-blocking `send`/`receive`

**What's done:** `Message` (Copy+Clone), circular `MessageQueue`; queues embedded in each PCB (`mailbox` field); blocking receive integrated with scheduler state machine
**What's missing:** Syscall integration (send/recv system calls), kernel→userspace message delivery

---

### 7. System calls — `crates/arch-x86_64/src/cpu/syscall.rs`

The only legal crossing point from ring 3 into ring 0.

Implementation:
- `SYSCALL`/`SYSRET` via EFER.SCE, STAR, LSTAR, SFMASK MSRs — initialised at boot
- Naked assembly `syscall_entry` stub — saves callee-saved + rcx/r11, stubs ENOSYS return
- GDT extended to 5 entries so SYSRET loads correct ring-3 CS (0x20) and SS (0x18)

**What's done:** MSRs configured, entry stub compiles and would be invoked on `syscall`
**What's missing:** TSS.RSP0 stack switch, Rust dispatcher, actual syscall implementations

Minimum viable syscall table:

| # | Name | Description |
|---|------|-------------|
| 0 | `send(dst, msg*)` | Deposit message into target queue; unblock if waiting |
| 1 | `recv(msg*)` | Receive from own queue; block if empty |
| 2 | `mmap(len, flags)` | Map anonymous pages into caller's address space |
| 3 | `munmap(addr, len)` | Unmap pages |
| 4 | `exit(code)` | Terminate calling process |
| 5 | `yield()` | Voluntarily give up the CPU |

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
  [ ] Load kernel PML4 into CR3 (identity-map all RAM first)
  [ ] Buddy allocator replacing bump allocator
  [ ] Per-process address spaces (CR3 per process)

Phase 2 — CPU Mechanics
  [x] Exception handlers: full register dump (all 15 GPRs + rip/rflags/error_code) over serial
  [x] Voluntary context switch: save/restore all callee-saved GPRs + rsp in assembly
  [x] TICK_COUNT incremented atomically at 100 Hz by timer ISR
  [x] Scheduler: timer_tick() advances quantum; returns (old_ctx, new_ctx) for caller to switch
  [ ] Preemptive switch from timer ISR (save all GPRs inside ISR, switch CR3)
  [ ] GOP framebuffer console (visible output in QEMU window)

Phase 3 — Userspace Boundary
  [x] r8–r15 + all caller-saved in PCB TaskContext
  [x] Per-process kernel stacks (32 × 8 KB in BSS, allocated with AtomicUsize)
  [x] SYSCALL/SYSRET MSRs configured; naked entry stub
  [ ] TSS.RSP0 kernel-stack switch on syscall entry
  [ ] Rust syscall dispatcher; actual implementations of 6 core syscalls
  [ ] ELF loader (ring-3 process from image)

Phase 4 — IPC
  [x] Per-process MessageQueue embedded in PCB (mailbox field)
  [x] blocking_receive: parks process until a message arrives
  [x] send_message: delivers message + unblocks receiver
  [ ] Wire send/recv through syscall table

Phase 5 — Servers
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
`switch_context(old, new)` is a naked asm function (System V AMD64 ABI: `rdi=old`, `rsi=new`):
1. Saves callee-saved registers (rbx, rbp, r12–r15) and rsp into `*old`.
2. Restores callee-saved registers and rsp from `*new`.
3. Executes `ret`, which pops the return address from the new stack.

For a **new process**, `ProcessControlBlock::new()` writes the entry point at
`[kern_rsp]` (the top of its 8 KB kernel stack), so the first `ret` jumps there.
For a **resumed process**, the saved rsp still points to the return address left by
the original `call switch_context`, so `ret` resumes after that call.

**Why static GDT and IDT?**
The CPU reads the GDT on every segment reload and the IDT on every interrupt.
Stack-allocated descriptors would be overwritten by interrupt nesting. `static` gives
them a fixed address for the kernel lifetime. GDT is a plain `static` (immutable);
IDT is `static mut` accessed via raw pointer to avoid UB on the mutable reference.

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
