# Chapter 1 — Introduction: What Rost Is and Why It Exists

## 1.1 The Problem with General-Purpose Kernels

Modern general-purpose kernels — Linux, Windows, macOS — are engineering marvels.
They run billions of devices and have accumulated decades of optimisation.
But they were not designed to be certified to safety standards.  They were designed
to be fast and featureful.

When you deploy Linux in a medical device, an automotive safety controller, or an
avionics flight management unit, you face a fundamental mismatch.  The kernel you
are shipping contains millions of lines of C that were written over 35 years by
thousands of contributors with no traceability to any safety requirement.  The
scheduler has edge cases that are documented in bug trackers rather than formally
proven.  Memory is allocated and freed on hot paths in ways that can lead to
fragmentation and out-of-memory conditions at arbitrary times.

IEC 61508 (Functional Safety of Electrical/Electronic/Programmable Electronic
Safety-related Systems) and its automotive derivative ISO 26262 impose requirements
that are structurally incompatible with how monolithic kernels are built:

- **Deterministic worst-case execution time** — a monolithic kernel can call
  arbitrary subsystems on any code path; the timing of a syscall is not bounded.
- **Spatial isolation** — a buggy driver in a monolithic kernel can corrupt kernel
  memory and take down the entire system.
- **Resource partitioning** — processes must not be able to starve each other of
  CPU, memory, or IPC bandwidth.
- **Formal traceability** — every line of code in the safety-critical software
  must be traceable to a requirement that is traceable to a hazard analysis.

These requirements motivate the microkernel design.

## 1.2 The Microkernel Idea

The core idea of a microkernel is simple: put as little code as possible in ring 0
(the highest privilege level, where a bug can compromise the entire system), and
move everything else into ring 3 (user space), where a bug causes at worst a
process crash.

The minimum ring-0 services are:
1. **Memory management** — mapping virtual pages to physical frames
2. **Process management** — creating and destroying processes
3. **Scheduling** — deciding which process runs next
4. **IPC** — a secure channel for ring-3 processes to communicate
5. **Interrupt delivery** — routing hardware interrupts to their owner processes

Everything else — file systems, device drivers, network stacks, even POSIX system
calls — runs as ordinary user-space processes.  If a device driver crashes, the
kernel notices, and the init process restarts it.  The rest of the system keeps
running.

This is exactly the architecture of Rost.

## 1.3 Rost's Design Goals

Rost was designed with four goals in equal priority:

**Goal 1: Safety-critical certification readiness**
Every kernel module is written to IEC 61508 SIL-4 requirements:
temporal partitioning (no process can hog the CPU), spatial isolation
(no process can read or write another's memory), resource reclaim on termination,
bounded wait times for every blocking operation, watchdog supervision, and a
persistent crash log that survives warm resets.

**Goal 2: Formal verifiability**
The core scheduling and IPC invariants are modeled in TLA+ and checked with
TLC model checking.  Unit tests exercise every module to MC/DC branch coverage.
A requirements traceability matrix links every requirement to its implementation
and its tests.

**Goal 3: No unsafe code outside precisely-marked boundaries**
Rust's type system and borrow checker are the primary safety net.  All `unsafe`
blocks are isolated to well-defined, narrow interfaces — hardware register access,
inline assembly, and the handful of places where raw pointers are truly unavoidable.
Every `unsafe` block has a `# Safety` comment explaining exactly why it is correct.

**Goal 4: Real-world usability**
Rost boots on real x86-64 hardware (and QEMU), runs an interactive shell, mounts
a FAT32 filesystem from a virtio block device, runs a TCP/IP network stack, and
can execute ring-3 ELF binaries.  It is not a toy.

## 1.4 The Hardware Platform

Rost targets 64-bit x86 (x86_64) hardware.  The reasons are practical:

- UEFI boot firmware is universal on modern x86 hardware
- QEMU provides excellent emulation including virtio devices and a GPU framebuffer
- x86_64 has well-documented, stable hardware interfaces (I/O ports, LAPIC, IOAPIC)
- The Rust compiler has excellent x86_64 support and a mature ecosystem for bare-metal

The kernel binary is itself a UEFI application — there is no separate bootloader.
This is an intentional design choice: the firmware loads the kernel directly, the
kernel collects all hardware information it needs while UEFI boot services are still
running, then calls `ExitBootServices()` to take exclusive hardware ownership.

## 1.5 The Rust Choice

Writing a kernel in Rust rather than C is not just an aesthetic choice.  It has
concrete safety consequences:

**Memory safety** — Rust eliminates entire categories of C bugs: buffer overflows,
use-after-free, double-free, null pointer dereference, and data races.  These are
the bugs that historically have caused the most critical kernel vulnerabilities.

**`no_std` support** — Rust has first-class support for `no_std` environments where
there is no operating system underneath.  The standard library is replaced with
`core` (safe primitives with no OS dependencies) and `alloc` (heap allocation
without a specific allocator).

**Inline assembly** — Rust's `asm!` macro provides type-safe inline assembly with
explicit clobber lists.  It is safer than GCC's inline asm syntax and integrates
cleanly with the borrow checker.

**Type-level correctness** — Rust's type system lets us encode invariants that C
cannot.  For example, `ProcessId` is a newtype wrapper around `u32` — you cannot
accidentally pass a raw `u32` to a function expecting a process identifier.

**The cost** — Rust has a steeper learning curve than C for low-level work.  Some
patterns that are trivial in C (raw pointer arithmetic, mutable global state) require
deliberate `unsafe` blocks in Rust.  This is a feature, not a bug: it forces you to
think carefully about every place where you bypass the type system.

## 1.6 Codebase Structure

The Rost repository is organized as two Cargo workspaces:

```
crates/           ← Ring-0 kernel code (target: x86_64-unknown-uefi)
  kernel/         ← Main kernel binary (efi_main entry point)
  arch-x86_64/    ← CPU structures, GDT, IDT, syscall, context switch
  core-kernel/    ← Scheduler, memory, IPC, process table, service registry
  hal/            ← Hardware abstraction layer (UART, watchdog)

servers/          ← Ring-3 user-space servers (target: x86_64-unknown-none)
  init/           ← PID 1: health monitor and watchdog
  uart-drv/       ← UART driver (COM1 I/O)
  vfs/            ← Virtual filesystem server
  shell/          ← Interactive Zsh-compatible shell
  net/            ← TCP/IP network stack with virtio-net driver
  pci-bus/        ← PCI bus scanner
  block-drv/      ← virtio-blk block device driver
  gop/            ← GOP framebuffer terminal (full VT100 emulator)
  ps2-kbd/        ← PS/2 keyboard driver
  libc/           ← POSIX compatibility layer (malloc, string, stdio, etc.)
  hello-world/    ← Minimal ring-3 demo binary
```

The two workspaces use different compilation targets:
- `crates/` uses `x86_64-unknown-uefi` — produces a PE32+ UEFI application
- `servers/` uses `x86_64-unknown-none` — produces bare-metal ELF binaries

The kernel embeds all server ELFs at compile time using `include_bytes!`.
When the kernel boots, it loads each server from its embedded bytes and spawns
it as a ring-3 process.

## 1.7 The Boot Sequence in One Paragraph

The firmware loads `Rost.efi`.  The kernel's `efi_main` function runs in ring 0.
It calls `boot_collector::collect()` to harvest UEFI hardware data (memory map,
GOP framebuffer, ACPI tables, CPU features), then calls `ExitBootServices()` to
take exclusive hardware ownership.  It installs its own page tables (identity-mapping
all physical memory), sets up the GDT, TSS, and IDT, initializes the LAPIC and
IOAPIC, starts the scheduler, and spawns nine ring-3 server processes — init,
uart-drv, vfs, shell, net, pci-bus, block-drv, gop, and ps2-kbd — each from its
embedded ELF binary.  From that point on, the kernel only runs in ring 0 to handle
system calls, interrupts, and exceptions; all real work happens in ring 3.

## 1.8 Chapter Roadmap

This book proceeds as follows:

| Chapter | Topic |
|---------|-------|
| 2 | Boot & UEFI Hardware Discovery |
| 3 | Physical Memory Management |
| 4 | Virtual Memory & Paging |
| 5 | CPU Structures & Privilege Levels |
| 6 | Interrupt & Exception Handling |
| 7 | Process Management |
| 8 | The Scheduler |
| 9 | Inter-Process Communication |
| 10 | System Calls |
| 11 | Timer Subsystem |
| 12 | Service Registry |
| 13 | Kernel Safety & Integrity |
| 14 | Formal Verification & Testing |
| 15 | The ELF Loader |
| 16 | The Init Process |
| 17 | The Shell Server |
| 18 | The VFS Server |
| 19 | Device Drivers |
| 20 | The GOP Framebuffer Console |
| 21 | The Network Stack |
| 22 | Interrupt-Driven I/O |
| 23 | The POSIX Compatibility Layer |
| 24 | Building, Running & Debugging |
| 25 | Future Work |
