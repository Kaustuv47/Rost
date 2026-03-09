# Rost

A minimal x86_64 UEFI microkernel written in Rust (`no_std`).

The kernel does exactly 8 things. Everything else — filesystems, drivers, shells, applications — runs as isolated userspace processes communicating through IPC.

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
└───────────────────────────────────────────────────────────────┘
```

---

## The 8 Kernel Modules

### 1. Boot code — `src/main.rs`, `src/cpu.rs`, `src/console.rs`

Brings the CPU to a known state after UEFI hands over control.

- UEFI entry point via `#[entry]` (uefi crate, no bootloader needed)
- Global Descriptor Table: 3 selectors — null, 64-bit code (`0x08`), data (`0x10`)
- Interrupt Descriptor Table: 256 gates, loaded from a `static` (not the stack)
- Console: COM1 serial at 38400 baud 8N1, port I/O — works in long mode

**What's done:** UEFI boot, GDT, IDT, serial console
**What's missing:** GOP framebuffer (QEMU window is blank; serial works)

---

### 2. Memory management — `src/memory.rs`

Owns all physical memory and virtual address spaces.

- Physical allocator: bump allocator (no deallocation yet)
- Page tables: simplified single-level (placeholder)
- `map_page`, `translate_address` exist but page table is not wired to CR3

**What's done:** allocator skeleton, page table types
**What's missing:** Buddy allocator, 4-level PML4 page tables, per-process address spaces, UEFI memory map integration

---

### 3. Interrupt handling — `src/interrupts.rs`

Handles all CPU exceptions and hardware interrupts.

| Vector | Source | Handler |
|--------|--------|---------|
| 0 | #DE Division by zero | Halt loop |
| 13 | #GP General protection fault | Halt loop |
| 14 | #PF Page fault | Halt loop |
| 32 | IRQ0 PIT timer (100 Hz) | EOI to PIC, iretq |

Handlers use `#[unsafe(naked)]` with hand-written `iretq` — required on stable Rust (no `abi_x86_interrupt`).

**What's done:** All handlers registered, ISRs don't crash the kernel
**What's missing:** Exception handlers print nothing before halting — serial output needed for debugging

---

### 4. Process management — `src/process.rs`

Creates and destroys processes; owns per-process state.

- `ProcessControlBlock`: holds GPRs (rax–rdi, rsp, rbp, rip, rflags), `page_table_base`, time slice
- `ProcessTable`: fixed array of up to 32 slots
- `create_process(entry, stack)`, `terminate_process(pid)`, `get_ready_processes()`

**What's done:** PCB data structure, create/terminate, process table
**What's missing:** r8–r15 missing from PCB, per-process kernel stack, address space isolation (page_table_base = 0), ring-3 execution, ELF loader

---

### 5. Scheduling — `src/scheduler.rs`

Decides which process runs next; performs the context switch.

- Round-robin over all `Ready` processes
- `schedule()` returns the next PID
- `context_switch()` calls `restore_context()` — **currently a stub**

**What's done:** Round-robin selection logic
**What's missing:** Actual CPU state save/restore in assembly, CR3 switch, per-process kernel stack setup (TSS.RSP0)

---

### 6. IPC — `src/ipc.rs`

Passes messages between processes — the only way userspace servers communicate.

- `Message`: sender PID + 8×u64 payload (72 bytes)
- `MessageQueue`: circular buffer, capacity 16, non-blocking `send`/`receive`

**What's done:** Message and queue data structures, circular buffer logic
**What's missing:** Queues not attached to processes, no blocking receive (process should block on empty queue, not busy-wait), no integration with syscall layer

---

### 7. System calls — _(not started)_

The only legal crossing point from ring 3 into ring 0.

Planned implementation:
- `SYSCALL`/`SYSRET` via STAR, LSTAR, SFMASK MSRs
- Naked assembly entry stub — switches to kernel stack (TSS.RSP0), saves user registers
- Rust dispatcher in `src/syscall.rs`

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

### 8. Timer — `src/timer.rs`

Provides the heartbeat that drives the scheduler.

- PIT channel 0 configured for 100 Hz (10 ms ticks), divisor 11932
- PIC master (0x20) and slave (0xA0) initialised, IRQ0 unmasked
- IRQ0 → IDT vector 32 → `timer_interrupt_handler`

**What's done:** PIT + PIC hardware fully configured, timer ISR fires
**What's missing:** ISR does not call the scheduler — timer ticks but no context switch happens

---

## Project Structure

```
src/
├── main.rs        Boot entry, global allocator, static GDT/IDT
├── console.rs     COM1 serial output (UART port I/O)
├── cpu.rs         GDT, IDT structs + load; sti/cli/hlt wrappers
├── interrupts.rs  Naked ISR stubs for exceptions + timer
├── timer.rs       PIT 100 Hz + PIC master/slave init
├── memory.rs      Physical allocator, page table types
├── process.rs     ProcessControlBlock, ProcessTable
├── scheduler.rs   Round-robin Scheduler
└── ipc.rs         Message, MessageQueue

build/
└── efi/boot/
    └── bootx64.efi   UEFI boot binary (copy of Rost.efi after build)

.cargo/
└── config.toml       default target = x86_64-unknown-uefi
```

---

## Requirements

| Tool | Notes |
|------|-------|
| Rust stable ≥ 1.85 | `#[unsafe(naked)]` stabilised in 1.85 |
| `x86_64-unknown-uefi` target | `rustup target add x86_64-unknown-uefi` |
| QEMU (MacPorts) | `sudo port install qemu` |
| EDK2 firmware | Bundled with MacPorts QEMU at `/opt/local/share/qemu/edk2-x86_64-code.fd` |

---

## Build

```sh
cargo build
# Output: target/x86_64-unknown-uefi/debug/Rost.efi

cargo build --release
# Output: target/x86_64-unknown-uefi/release/Rost.efi
```

Copy to boot directory after building:

```sh
cp target/x86_64-unknown-uefi/debug/Rost.efi build/efi/boot/bootx64.efi
```

---

## Run (QEMU, macOS with MacPorts)

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

Kernel output appears in the **terminal** (serial stdio). The QEMU window will be blank until the GOP framebuffer console is implemented.

Exit QEMU: `Ctrl+A` then `X`.

**Real hardware:** Copy `build/efi/boot/bootx64.efi` to a FAT32 USB drive at `EFI/BOOT/BOOTX64.EFI`.

---

## Roadmap

Ordered by dependency — each phase unlocks the next.

```
Phase 1 — Memory  (unblocks everything else)
  [ ] #3  4-level page tables (PML4→PDPT→PD→PT), per-process CR3
  [ ] #4  Buddy allocator + UEFI memory map integration

Phase 2 — CPU Mechanics
  [ ] #1  GOP framebuffer console (visible output in QEMU window)
  [ ] #12 Exception handlers: print register dump over serial before halting
  [ ] #2  Real context switch: save/restore all GPRs in timer ISR, switch CR3
  [ ] #14 Wire timer ISR to scheduler (time-slice expiry → context switch)

Phase 3 — Userspace Boundary
  [ ] #6  Process management: r8–r15 in PCB, kernel stack, ELF loader, ring-3 entry
  [ ] #5  System calls: SYSCALL/SYSRET, TSS.RSP0, minimal dispatcher (6 syscalls)

Phase 4 — IPC
  [ ] #13 Per-process message queues with blocking recv (integrate with scheduler)

Phase 5 — Tooling
  [ ] #10 Makefile: build, run, run-serial, release targets
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
The CPU reads the GDT on every segment reload and the IDT on every interrupt. Stack-allocated GDT/IDT will be overwritten by interrupt nesting. `static mut` puts them at a fixed address for the kernel lifetime.

**Why only 8 kernel modules?**
Microkernel principle: a kernel bug is a security hole. Filesystems, drivers, and protocols all crash in userspace — they can be restarted without rebooting. Only the 8 modules above require ring 0.

---

## References

- [OSDev Wiki](https://wiki.osdev.org) — GDT, IDT, PIC, PIT, paging, context switch
- [Intel SDM Vol. 3](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html) — SYSCALL/SYSRET, TSS, MSRs, CR3
- [UEFI Specification](https://uefi.org/specifications) — GOP, memory map, boot services
- [seL4 whitepaper](https://sel4.systems/About/seL4-whitepaper.pdf) — microkernel IPC design
- [Rust Embedded Book](https://docs.rust-embedded.org/book/) — `no_std` patterns
