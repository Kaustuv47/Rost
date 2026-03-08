# Rost

A minimal x86_64 UEFI microkernel written in Rust (`#![no_std]`).

## Features

- **Memory management** — physical allocator and 4-level page tables
- **CPU setup** — GDT and IDT initialization
- **Interrupt handling** — exception handlers (divide-by-zero, page fault, GPF) and timer ISR
- **Timer** — PIT configured for 100 Hz; PIC initialization
- **Process management** — process control blocks and a fixed-capacity process table
- **Scheduler** — simple round-robin scheduler
- **IPC** — fixed-size message queues

## Project Structure

```
src/
├── main.rs        # Entry point, global allocator, module declarations
├── memory.rs      # Physical allocator and page table management
├── cpu.rs         # GDT, IDT, and CPU control primitives
├── interrupts.rs  # Exception and interrupt handlers
├── timer.rs       # PIT and PIC configuration
├── process.rs     # Process control blocks and process table
├── scheduler.rs   # Round-robin scheduler
├── ipc.rs         # Inter-process communication (message queues)
└── console.rs     # Serial/BIOS console output
```

## Build

Requires a nightly Rust toolchain with the `x86_64-unknown-uefi` target.

```sh
# Install the target
rustup target add x86_64-unknown-uefi

# Build
cargo build --release
```

The compiled UEFI application will be at:
`target/x86_64-unknown-uefi/release/Rost.efi`

## Running

Copy `Rost.efi` to a FAT32 USB drive at `EFI/BOOT/BOOTX64.EFI` and boot from it,
or use QEMU with OVMF firmware.