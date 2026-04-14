# Chapter 2 — Boot & UEFI Hardware Discovery

## 2.1 Why UEFI Instead of a Traditional Bootloader

Traditional OS kernels — Linux, BSDs, Windows — are loaded by a separate
bootloader such as GRUB, syslinux, or the Windows Boot Manager.  The bootloader
runs in the firmware environment, sets up a minimal C runtime, loads the kernel
image from disk, and jumps to it.  The kernel then has to re-discover hardware
from scratch.

Rost takes a different approach: **the kernel binary is itself a UEFI application**.
There is no separate bootloader.  The UEFI firmware loads `Rost.efi` from the EFI
System Partition, hands it a `SystemTable` reference that provides access to all
firmware services, and calls `efi_main(image_handle, system_table)`.

The advantages of this design are significant:

1. **No bootloader attack surface** — a traditional bootloader is itself privileged
   code that can be compromised before the kernel even starts.  Removing it reduces
   the trusted computing base.

2. **Rich hardware information at startup** — UEFI provides a complete, structured
   hardware description that would otherwise require complex ACPI parsing to
   reconstruct.  We harvest everything we need in one call to `collect()`.

3. **Secure Boot integration** — UEFI Secure Boot verifies the signature of every
   EFI application before executing it.  Since Rost.efi IS the first thing loaded,
   the entire boot chain from firmware to kernel is covered by Secure Boot.

4. **Simplicity** — one binary, one boot stage.  The kernel owns everything from
   the moment `efi_main` is called.

## 2.2 The `efi_main` Entry Point

The Rust `uefi` crate provides the `#[entry]` macro that marks the UEFI application
entry point.  When the firmware jumps to `Rost.efi`, it calls:

```rust
#[entry]
fn efi_main(image_handle: Handle, system_table: SystemTable<Boot>) -> Status {
    hal::uart::init();
    // ...
}
```

`SystemTable<Boot>` is a typed Rust handle to the firmware's boot-time service
table.  The `<Boot>` type parameter means boot services are still available — it
is impossible (at the type level) to accidentally call a boot service after
`ExitBootServices()` has been called, because that call consumes the typed handle
and returns a `SystemTable<Runtime>` instead.

This is a beautiful example of Rust's type system enforcing a protocol invariant.
In C, calling a boot service after `ExitBootServices()` is undefined behavior with
no compiler protection.

The first thing `efi_main` does is initialize the UART so that subsequent log
output is visible on the serial port.  UART I/O works before any UEFI services and
continues to work after boot services exit — it is the kernel's unconditional
diagnostic channel.

## 2.3 The Eight Initialization Stages

The boot sequence is divided into eight numbered stages.  Each stage is logged
on the UART so that if boot hangs, the last logged stage tells you exactly where.

```
Stage 0: UEFI Hardware Discovery
Stage 1: Physical Memory Management + Page Tables
Stage 2: CPU Structures (GDT, TSS, IDT)
Stage 3: APIC, IOAPIC, UART IRQ routing
Stage 4: Timer (HPET, LAPIC, TSC calibration), Watchdog
Stage 5: Kernel safety protections (WP, SMEP, SMAP, NXE, stack canaries)
Stage 6: Scheduler + ELF spawn hook
Stage 7: Spawn ring-3 servers
Stage 8: Kernel ready — idle loop
```

This staged approach means that every hardware subsystem is initialized in
dependency order.  The page tables must be live before the IDT can be loaded
(the IDT handlers access kernel data structures that require virtual memory).
The APIC must be initialized before the timer, which must be initialized before
the scheduler (the scheduler needs a tick source).

## 2.4 Stage 0: Collecting Hardware Information

The `boot_collector::collect()` function harvests everything the kernel needs
from UEFI while boot services are still available.  It returns a `BootInfo`
structure that is stored in a `static mut` and remains valid for the kernel's
entire lifetime.

### 2.4.1 What `BootInfo` Contains

```rust
pub struct BootInfo {
    pub firmware:    FirmwareInfo,    // vendor string, UEFI version
    pub cpu:         CpuInfo,         // CPUID data, address bits, feature flags
    pub memory_map:  MemoryMap,       // physical memory regions with types
    pub framebuffer: GopInfo,         // up to 4 GOP outputs with base/size/stride/format
    pub acpi_rsdp:   Option<u64>,     // ACPI Root System Description Pointer
    pub smbios:      SmbiosInfo,      // SMBIOS entry points (v2 and v3)
    pub secure_boot: SecureBootState, // Enabled / Disabled / SetupMode / Unknown
    pub wall_clock:  WallClock,       // UEFI real-time clock reading
    pub cmdline:     [u8; 256],       // kernel command line (UCS-2 → ASCII)
    pub image_base:  u64,             // kernel PE32+ load address
    pub image_size:  u64,             // kernel PE32+ image size
}
```

### 2.4.2 Physical Memory Map

The UEFI memory map is a list of memory descriptors, each describing a physical
region with a `MemoryType`.  Rost classifies these types into its own `MemoryKind`
enum:

| UEFI Type | Rost Kind | Meaning |
|-----------|-----------|---------|
| `CONVENTIONAL_MEMORY` | `Free` | Available for the kernel to use |
| `LOADER_CODE` / `LOADER_DATA` | `Kernel` | The kernel's own image pages |
| `BOOT_SERVICES_CODE` / `_DATA` | `Reclaimable` | Freed after ExitBootServices |
| `ACPI_RECLAIM_MEMORY` | `Reclaimable` | ACPI tables — can be freed after parse |
| `RUNTIME_SERVICES_*` | `Firmware` | Must not be touched |
| `MEMORY_MAPPED_IO` | `Mmio` | Device registers — must identity-map |

The physical memory manager uses this map to find the largest contiguous
`CONVENTIONAL_MEMORY` region, which becomes the kernel's physical frame pool.

### 2.4.3 GOP Framebuffer

UEFI's Graphics Output Protocol (GOP) provides the kernel with the physical base
address, byte size, resolution (width × height), stride (bytes per row), and
pixel format of each attached display.  Rost supports up to four displays but
typically uses only the first one (the primary display).

The GOP info is captured before `ExitBootServices()` because the UEFI framebuffer
remains valid after boot services exit — the memory is not reclaimed.  We store
the physical base address in `FB_PHYS_ADDR` and explicitly map it into the kernel
PML4 in Stage 4.

### 2.4.4 ACPI RSDP

The Advanced Configuration and Power Interface (ACPI) Root System Description
Pointer tells us where the ACPI table hierarchy lives in physical memory.  We store
the physical address and parse the tables in Stage 1:

- **MADT** (Multiple APIC Description Table) — discovers Local APIC and I/O APIC
  physical addresses and IRQ override entries
- **DMAR** (DMA Remapping) — discovers Intel VT-d IOMMU units
- **FADT** (Fixed ACPI Description Table) — discovers PM timer I/O port and
  the ACPI system reset register
- **HPET** (High Precision Event Timer) — discovers HPET MMIO base address

### 2.4.5 Secure Boot State

The kernel reads the UEFI Secure Boot state variable and stores it in
`boot_info.secure_boot`.  In `--features safety-mode` builds, the kernel halts
with a `FATAL` message if Secure Boot is not `Enabled`.  In development builds,
it logs a warning and continues.  This implements IEC 61508's requirement for
chain-of-trust from hardware initialization to software execution.

### 2.4.6 CPU Feature Detection (CPUID)

The `CpuInfo` field is populated by running the `CPUID` instruction at several
leaf values:

- **Leaf 0** — CPU vendor string (e.g., "GenuineIntel", "AuthenticAMD")
- **Leaf 1** — Family/model/stepping; feature flags (HTT, SMEP, SSE4.2, etc.)
- **Leaf 7** — Extended features (SMAP, etc.)
- **Leaf 0x80000002–4** — CPU brand string (e.g., "Intel Core i7-11800H")
- **Leaf 0x80000008** — Physical and virtual address bits

The kernel later checks that SMEP and MSR support are available, and halts if
the CPU has more than one logical core (a SIL-4 requirement for single-core
enforcement — see Chapter 13).

## 2.5 Calling `ExitBootServices()`

After `collect()` returns, `efi_main` calls:

```rust
let (_runtime_table, _final_mmap) =
    system_table.exit_boot_services(MemoryType::LOADER_DATA);
```

This call is irreversible.  After it:
- All UEFI boot services are gone forever
- Firmware timer callbacks stop running
- The memory map returned is the final authoritative map

The `system_table` value is consumed by the call — Rust's ownership system makes
it impossible to accidentally call any boot service after this point.

IEC 61508 §7.2.1 requires the safety-critical system to have exclusive control of
all hardware it uses.  Calling `ExitBootServices()` satisfies this requirement.

## 2.6 ACPI Parsing

After boot services exit, the kernel parses the ACPI tables it needs.  All ACPI
parsing in Rost is implemented in `core-kernel/src/acpi/`.

### 2.6.1 The XSDT/RSDT Walk

ACPI tables form a tree.  The RSDP points to the XSDT (Extended System Description
Table, 64-bit pointers) for ACPI ≥ 2.0, or the RSDT (32-bit pointers) for ACPI 1.0.
Both tables contain an array of physical addresses pointing to other tables.

The generic helper `find_table(rsdp_phys, b"MADT")` implements this walk:
1. Read and validate the RSDP structure (magic `"RSD PTR "`, checksum)
2. Follow the XSDT or RSDT pointer
3. Validate the SDT header (signature + length + checksum)
4. Scan the entry array for a table whose 4-byte signature matches the target
5. Return the physical address of the found table, or `None`

### 2.6.2 MADT Parsing

The Multiple APIC Description Table tells the kernel about interrupt controllers.
Rost's `parse_madt()` function scans MADT entry records to find:

- **Local APIC entries** — one per logical CPU.  Rost uses only the first one
  (single-core enforcement).  The LAPIC physical address is stored in
  `LAPIC_PHYS_ADDR`.
- **I/O APIC entries** — one per I/O APIC.  The primary IOAPIC address and GSI base
  are stored in `IOAPIC_PHYS_ADDR` and `IOAPIC_GSI_BASE`.
- **IRQ Override entries** — ISA IRQs that are re-routed to different GSIs.
  For example, on QEMU, IRQ 0 (PIT timer) is re-routed to GSI 2.  The `irq_to_gsi()`
  function applies these overrides.

### 2.6.3 FADT Parsing

The Fixed ACPI Description Table contains the PM timer I/O port (used for TSC
calibration) and the ACPI reset register (a second safe-state path alongside the
hardware watchdog).

### 2.6.4 HPET Parsing

The HPET table contains the physical base address and period of the High Precision
Event Timer.  Rost uses the HPET as a reference clock for TSC calibration.

## 2.7 CPU Feature Validation

Before activating the MMU, the kernel calls `check_cpu_features()` which:

1. Reads `CPUID.01H` feature flags and verifies SMEP support
2. Reads `CPUID.07H` and verifies SMAP support
3. Checks `CPUID.01H:EDX[28]` (HTT bit) and `max_logical_cpus`.  If more than
   one logical CPU is detected, the kernel halts with a clear diagnostic.

The single-core enforcement is not a limitation we want to remove — it is a
deliberate SIL-4 architectural constraint.  Multicore execution introduces
races that would require spinlocks on every scheduler hot path, adding complexity
and potential for priority inversion.  On a single core, all non-preemptible
critical sections are safe with just `cli`/`sti`.

## 2.8 The `BootInfo` Lifetime

The `BootInfo` is stored as a `static mut BOOT_INFO: BootInfo`.  After Stage 0,
it is read-only for the rest of the kernel's lifetime.  All modules that need
hardware information read it through a shared reference obtained via
`core::ptr::addr_of!(BOOT_INFO)`.

This is one of the few places in the kernel where a `static mut` is justified:
the value is written exactly once (during boot collection) and read-only thereafter,
so there are no race conditions.  In a more complex system we might protect it with
a `Once<BootInfo>`, but since the kernel is single-core and writes it before
enabling interrupts, the current approach is correct.

## 2.9 Summary

The boot and hardware discovery phase establishes the foundation on which
everything else rests.  By the time Stage 0 is complete:

- All hardware information has been collected from UEFI
- UEFI boot services have been surrendered (exclusive hardware ownership acquired)
- All ACPI tables have been parsed (LAPIC, IOAPIC, HPET, PM timer addresses known)
- CPU features have been verified (SMEP, SMAP, single-core)
- The serial port is running and all subsequent output is logged

The subsequent stages build on this foundation to construct the complete kernel
execution environment.
