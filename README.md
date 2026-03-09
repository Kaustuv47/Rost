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
│   1. Boot code       2. Memory mgmt     3. Interrupts         │
│   Start the CPU      Paging / VMM       Handle HW interrupts  │
│                                                               │
│   4. Processes       5. Scheduling      6. IPC                │
│   Create/destroy     Context switch     Send/receive msgs      │
│   address spaces     between procs      between processes      │
│                                                               │
│   7. System calls    8. Timer                                 │
│   Userspace→kernel   Trigger scheduling                       │
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
├── kernel/                     # Binary — UEFI entry point, shell
│   └── src/
│       ├── main.rs             # efi_main, BumpAllocator, panic handler, init sequence
│       └── shell/
│           ├── mod.rs          # Interactive read loop, prompt
│           └── commands.rs     # Command dispatch (echo, help)
│
├── arch-x86_64/                # x86_64 hardware layer
│   └── src/
│       ├── cpu/
│       │   ├── gdt.rs          # GdtEntry, GlobalDescriptorTable
│       │   ├── idt.rs          # IdtEntry, InterruptDescriptorTable
│       │   └── mod.rs          # enable_interrupts, disable_interrupts, halt
│       ├── interrupts/
│       │   ├── handlers.rs     # Naked ISR stubs (#[unsafe(naked)])
│       │   └── mod.rs          # init() — wires handlers into IDT
│       └── timer/
│           ├── pic.rs          # PIC 8259 master/slave config
│           └── mod.rs          # PIT 100 Hz init
│
├── hal/                        # Hardware Abstraction Layer — device drivers
│   └── src/
│       └── uart.rs             # COM1 UART: init, put_byte, read_byte, print_str, print_hex
│
└── core-kernel/                # Architecture-independent kernel logic
    └── src/
        ├── memory/
        │   ├── physical.rs     # PhysicalAllocator (bump)
        │   └── paging.rs       # PageTable, map_page, translate_address
        ├── process/
        │   ├── mod.rs          # ProcessId
        │   ├── pcb.rs          # ProcessControlBlock, ProcessState
        │   └── table.rs        # ProcessTable (max 32 processes)
        ├── scheduler/
        │   └── round_robin.rs  # Round-robin Scheduler
        └── ipc/
            └── message.rs      # Message, MessageQueue (capacity 16)

build/
└── efi/boot/
    └── bootx64.efi             # Deployed UEFI binary (output of scripts/build.sh)

scripts/
├── build.sh                    # cargo build + copy .efi to build/
└── run.sh                      # QEMU launch command

.cargo/
└── config.toml                 # Default target: x86_64-unknown-uefi
```

**Crate dependency graph:**
```
kernel  ──►  arch-x86_64
        ──►  hal
        ──►  core-kernel
        ──►  uefi

arch-x86_64  ──►  (nothing)
hal          ──►  (nothing)
core-kernel  ──►  (nothing, uses alloc)
```

`core-kernel` has no architecture or hardware dependencies — its logic can be unit-tested on the host with `cargo test -p core-kernel --target <host>`.

---

## The 8 Kernel Modules

### 1. Boot & CPU setup — `crates/kernel/src/main.rs`, `crates/arch-x86_64/`

Brings the CPU to a known state after UEFI hands over control.

- UEFI entry point via `#[entry]` (uefi crate, no bootloader needed)
- Global Descriptor Table: 3 selectors — null, 64-bit code (`0x08`), data (`0x10`)
- Interrupt Descriptor Table: 256 gates, loaded from a `static` (not the stack)
- Serial console: COM1 at 38400 baud 8N1, port I/O — works in long mode

**What's done:** UEFI boot, GDT, IDT, serial console, interactive shell
**What's missing:** GOP framebuffer (QEMU window is blank; serial is the console)

---

### 2. Memory management — `crates/core-kernel/src/memory/`

Owns all physical memory and virtual address spaces.

- Physical allocator: bump allocator (`physical.rs`), 4 KB page-aligned
- Page tables: simplified single-level (`paging.rs`) — placeholder for 4-level PML4
- `map_page`, `translate_address` exist but page table is not wired to CR3

**What's done:** allocator skeleton, page table types
**What's missing:** Buddy allocator, 4-level PML4 page tables, per-process address spaces, UEFI memory map integration

---

### 3. Interrupt handling — `crates/arch-x86_64/src/interrupts/`

Handles all CPU exceptions and hardware interrupts.

| Vector | Source | Handler |
|--------|--------|---------|
| 0 | #DE Division by zero | Halt loop |
| 13 | #GP General protection fault | Halt loop |
| 14 | #PF Page fault | Halt loop |
| 32 | IRQ0 PIT timer (100 Hz) | EOI to PIC, iretq |

Handlers use `#[unsafe(naked)]` with hand-written `iretq` — required on stable Rust (no `abi_x86_interrupt`).

**What's done:** All handlers registered, ISRs don't crash the kernel
**What's missing:** Exception handlers print nothing before halting — register dump over serial needed for debugging

---

### 4. Process management — `crates/core-kernel/src/process/`

Creates and destroys processes; owns per-process state.

- `ProcessControlBlock` (`pcb.rs`): holds GPRs (rax–rdi, rsp, rbp, rip, rflags), `page_table_base`, time slice
- `ProcessTable` (`table.rs`): fixed array of up to 32 slots
- `create_process(entry, stack)`, `terminate_process(pid)`, `get_ready_processes()`

**What's done:** PCB data structure, create/terminate, process table
**What's missing:** r8–r15 missing from PCB, per-process kernel stack, address space isolation, ring-3 execution, ELF loader

---

### 5. Scheduling — `crates/core-kernel/src/scheduler/`

Decides which process runs next; performs the context switch.

- Round-robin over all `Ready` processes (`round_robin.rs`)
- `schedule()` returns the next PID
- `context_switch()` calls `restore_context()` — **currently a stub**

**What's done:** Round-robin selection logic
**What's missing:** Actual CPU state save/restore in assembly, CR3 switch, per-process kernel stack setup (TSS.RSP0)

---

### 6. IPC — `crates/core-kernel/src/ipc/`

Passes messages between processes — the only way userspace servers communicate.

- `Message` (`message.rs`): sender PID + 8×u64 payload (72 bytes)
- `MessageQueue`: circular buffer, capacity 16, non-blocking `send`/`receive`

**What's done:** Message and queue data structures, circular buffer logic
**What's missing:** Queues not attached to processes, no blocking receive, no integration with syscall layer

---

### 7. System calls — _(not started)_

The only legal crossing point from ring 3 into ring 0.

Planned implementation:
- `SYSCALL`/`SYSRET` via STAR, LSTAR, SFMASK MSRs
- Naked assembly entry stub — switches to kernel stack (TSS.RSP0), saves user registers
- Rust dispatcher in `crates/kernel/src/syscall.rs`

Minimum viable syscall table:

| # | Name | Description |
|---|------|-------------|
| 0 | `send(dst, msg*)` | Deposit message into target's queue; unblock if waiting |
| 1 | `recv(msg*)` | Receive from own queue; block process if empty |
| 2 | `mmap(len, flags)` | Map anonymous pages into caller's address space |
| 3 | `munmap(addr, len)` | Unmap pages |
| 4 | `exit(code)` | Terminate calling process |
| 5 | `yield()` | Voluntarily give up CPU |

---

### 8. Timer — `crates/arch-x86_64/src/timer/`

Provides the heartbeat that drives the scheduler.

- PIT channel 0 at 100 Hz (10 ms ticks), divisor 11932 (`mod.rs`)
- PIC master (0x20) and slave (0xA0) initialised, IRQ0 unmasked (`pic.rs`)
- IRQ0 → IDT vector 32 → `timer_interrupt_handler`

**What's done:** PIT + PIC hardware fully configured, timer ISR fires
**What's missing:** ISR does not call the scheduler — timer ticks but no context switch happens

---

## Shell

The kernel exposes a serial shell after boot. Connect via `-serial stdio` in QEMU.

```
rost> echo "Hello, Rost!"
Hello, Rost!

rost> echo Hello World
Hello World

rost> help
Commands:
  echo <text>   print text to console
  help          show this message
```

Backspace and line editing are supported. The shell polls COM1 and falls back to `hlt` between keystrokes so the CPU is not busy-waiting.

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

Kernel output and the shell appear in the **terminal** (serial stdio). The QEMU window will be blank until a GOP framebuffer console is implemented.

Exit QEMU: `Ctrl+A` then `X`.

**Real hardware:** Copy `build/efi/boot/bootx64.efi` to a FAT32 USB drive at `EFI/BOOT/BOOTX64.EFI`.

---

## Roadmap

Ordered by dependency — each phase unlocks the next.

```
Phase 1 — Memory  (unblocks everything else)
  [ ] 4-level page tables (PML4→PDPT→PD→PT), per-process CR3
  [ ] Buddy allocator + UEFI memory map integration

Phase 2 — CPU Mechanics
  [ ] GOP framebuffer console (visible output in QEMU window)
  [ ] Exception handlers: print register dump over serial before halting
  [ ] Real context switch: save/restore all GPRs in timer ISR, switch CR3
  [ ] Wire timer ISR to scheduler (time-slice expiry → context switch)

Phase 3 — Userspace Boundary
  [ ] Process management: r8–r15 in PCB, kernel stack, ELF loader, ring-3 entry
  [ ] System calls: SYSCALL/SYSRET, TSS.RSP0, minimal dispatcher (6 syscalls)

Phase 4 — IPC
  [ ] Per-process message queues with blocking recv (integrate with scheduler)

Phase 5 — Servers
  [ ] VFS server (userspace)
  [ ] Init process (PID 1)
```

---

## Design Notes

**Why UEFI, no bootloader?**
The `.efi` binary IS the kernel. UEFI provides memory map, framebuffer, and FAT32 for free. No GRUB, no Limine.

**Why serial for console?**
UEFI `SimpleTextOutput` is gone after `ExitBootServices()`. Serial port I/O works in long mode unconditionally. GOP framebuffer needs to be driven directly — planned but not yet implemented.

**Why `#[unsafe(naked)]` for ISRs?**
`extern "x86-interrupt"` is nightly-only. Naked functions are stable since Rust 1.85 and give exact control over the `iretq` sequence.

**Why static GDT and IDT?**
The CPU reads the GDT on every segment reload and the IDT on every interrupt. Stack-allocated GDT/IDT will be overwritten by interrupt nesting. `static` puts them at a fixed address for the kernel lifetime. GDT is a plain `static` (immutable); IDT is `static mut` accessed via raw pointer to avoid UB on the mutable reference.

**Why a Cargo workspace?**
Each crate has a single responsibility and a clean dependency boundary. `core-kernel` has no architecture or hardware dependencies, so its logic (scheduler, IPC, memory) can be unit-tested on the host without QEMU. Adding a second architecture (`aarch64`) would only require a new `arch-aarch64` crate — nothing else changes.

**Why only 8 kernel modules?**
Microkernel principle: a kernel bug is a security hole. Filesystems, drivers, and protocols all crash in userspace — they can be restarted without rebooting. Only the 8 modules above require ring 0.

---

## References

- [OSDev Wiki](https://wiki.osdev.org) — GDT, IDT, PIC, PIT, paging, context switch
- [Intel SDM Vol. 3](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html) — SYSCALL/SYSRET, TSS, MSRs, CR3
- [UEFI Specification](https://uefi.org/specifications) — GOP, memory map, boot services
- [seL4 whitepaper](https://sel4.systems/About/seL4-whitepaper.pdf) — microkernel IPC design
- [Rust Embedded Book](https://docs.rust-embedded.org/book/) — `no_std` patterns
