# Chapter 5 — CPU Structures & Privilege Levels

## 5.1 The x86-64 Privilege Model

x86-64 defines four privilege levels (rings 0–3).  The hardware uses the CPL
(Current Privilege Level) encoded in the low 2 bits of the CS register:

| Ring | CPL | Name | Rost Usage |
|------|-----|------|-----------|
| 0 | 0 | Kernel mode | Kernel, ISRs, syscall handlers |
| 1 | 1 | (unused) | Not used in Rost |
| 2 | 2 | (unused) | Not used in Rost |
| 3 | 3 | User mode | All ring-3 server processes |

The CPU enforces the privilege boundary: ring-3 code cannot execute privileged
instructions (`LGDT`, `LIDT`, `MOV CRn`, `WRMSR`, `HLT`, I/O port access) or
access memory without `PTE_USER`.  The SYSCALL/SYSRET mechanism provides the
single controlled transition from ring-3 to ring-0.

## 5.2 The Global Descriptor Table (GDT)

The GDT is a table of segment descriptors.  On x86-64, segmentation is mostly
irrelevant (the hardware forces segment bases to 0 and limits to 2^64 in 64-bit
mode), but the GDT is still required to configure:
- The privilege level for code segments (via DPL)
- The Task State Segment (TSS) descriptor

Rost's GDT has seven entries:

```
Slot 0 (selector 0x00): Null descriptor (required by CPU)
Slot 1 (selector 0x08): Ring-0 code segment  (64-bit, DPL=0, L=1, D=0)
Slot 2 (selector 0x10): Ring-0 data segment  (DPL=0, writable)
Slot 3 (selector 0x18): Ring-3 data segment  (DPL=3, writable)
Slot 4 (selector 0x20): Ring-3 code segment  (64-bit, DPL=3, L=1, D=0)
Slots 5–6:              TSS system descriptor (16-byte double slot, DPL=0)
```

The GDT is loaded by `lgdt` after being filled:

```rust
unsafe fn load(&self) {
    let gdtr = GdtDescriptor {
        limit: (core::mem::size_of::<GlobalDescriptorTable>() - 1) as u16,
        base:  self as *const _ as u64,
    };
    core::arch::asm!(
        "lgdt [{0}]",
        "push 0x08",          // ring-0 CS
        "lea rax, [rip + 1f]",
        "push rax",
        "retfq",              // far return → reload CS
        "1:",
        "mov ax, 0x10",       // ring-0 DS/ES/SS
        "mov ds, ax",
        "mov es, ax",
        "mov ss, ax",
        in(reg) &gdtr,
    );
}
```

The `retfq` (far return) is the standard trick for reloading CS without a
far-jump instruction: push the new CS and RIP onto the stack, then `retfq` pops
both.

### 5.2.1 STAR MSR and the Selector Encoding

The SYSCALL/SYSRET mechanism uses the STAR MSR to know which GDT selectors to
use:

```rust
// STAR[47:32] = ring-0 CS base selector (0x0008)
// STAR[63:48] = ring-3 base selector (0x0010)
// SYSRET sets  CS = STAR[63:48] + 16 = 0x0010 + 16 = 0x0020 (ring-3 code)
//             SS = STAR[63:48] +  8 = 0x0010 +  8 = 0x0018 (ring-3 data)
wrmsr(MSR_STAR,
    (0x0008u64 << 32) |  // ring-0 CS
    (0x0010u64 << 48)    // ring-3 base (SS = base+8, CS = base+16)
);
```

The encoding is subtle: SYSRET automatically sets `CS = STAR[63:48] + 16` and
`SS = STAR[63:48] + 8`.  With `STAR[63:48] = 0x0010`:
- SS = 0x0010 + 8 = **0x0018** → ring-3 data (DPL=3)
- CS = 0x0010 + 16 = **0x0020** → ring-3 code (DPL=3)

This matches the GDT layout precisely.

## 5.3 The Task State Segment (TSS)

The TSS is a special CPU data structure used for:
1. **RSP0** — the kernel-mode stack pointer used when transitioning from ring-3
   to ring-0 via interrupt or SYSCALL
2. **IST stacks** — "Interrupt Stack Table" — dedicated stacks for certain
   exception handlers (used to handle #DF, #NMI, #MC safely even if the normal
   kernel stack is corrupted)

```rust
#[repr(C, packed)]
pub struct TaskStateSegment {
    _reserved0:  u32,
    rsp:         [u64; 3],    // RSP0, RSP1, RSP2
    _reserved1:  u64,
    ist:         [u64; 7],    // IST1..IST7
    _reserved2:  [u8; 10],
    iomap_base:  u16,
}
```

### 5.3.1 RSP0 — The Kernel Stack Switch

When a ring-3 process is interrupted (by a timer, fault, or IRQ), the CPU
automatically saves the user RSP and loads RSP0 from the TSS before pushing the
exception frame.  This ensures the interrupt handler always has a clean kernel
stack to work with.

Rost updates RSP0 on every context switch:

```rust
pub fn set_rsp0(rsp0: u64) {
    unsafe {
        (*core::ptr::addr_of_mut!(TSS)).rsp[0] = rsp0;
        SYSCALL_KERN_RSP.store(rsp0, Ordering::Relaxed);
    }
}
```

The `SYSCALL_KERN_RSP` atomic is also updated because SYSCALL (unlike INT) does
not use RSP0 from the TSS — instead, Rost's syscall entry stub reads this value
to perform the stack switch manually (see Chapter 10).

### 5.3.2 IST Stacks

The IST (Interrupt Stack Table) provides dedicated kernel stacks for exceptions
that must run even if the main kernel stack is corrupted.  Rost uses three IST
stacks:

| Exception | IST | Rationale |
|-----------|-----|-----------|
| #DF (Double Fault, vector 8) | IST1 | #DF means the kernel stack may be gone |
| #NMI (Non-Maskable Interrupt, vector 2) | IST2 | NMI can arrive at any time |
| #MC (Machine Check, vector 18) | IST3 | Hardware error — kernel state unknown |

Each IST stack is a fixed-size buffer in BSS (typically 4–8 KB).  When an
exception uses an IST entry, the CPU switches to the corresponding IST stack
unconditionally, regardless of what RSP was at the time of the fault.

## 5.4 The Interrupt Descriptor Table (IDT)

The IDT maps exception/interrupt vectors (0–255) to handler addresses.  Each
entry is an 8-byte "gate descriptor":

```
Bits [15:0]   Offset[15:0]   (handler address low word)
Bits [31:16]  Segment selector (must be ring-0 CS = 0x08)
Bits [35:32]  IST index (0 = use RSP0; 1–7 = use IST[n])
Bits [43:40]  Gate type: 0xE = 64-bit interrupt gate
Bits [45:44]  DPL (0 for kernel handlers; 3 for int3 from ring-3)
Bit  47       Present
Bits [63:48]  Offset[31:16]
Bits [95:64]  Offset[63:32]
Bits [127:96] Reserved
```

An "interrupt gate" automatically clears the IF flag (disables interrupts) on
entry.  This is the correct type for almost all Rost handlers, which run with
interrupts disabled and re-enable them explicitly (or let IRETQ restore RFLAGS).

Rost registers all 256 IDT vectors:

```rust
pub fn install_handlers(idt: &mut InterruptDescriptorTable) {
    idt.set_handler(0,  handler_de as u64);   // #DE divide error
    idt.set_handler(2,  handler_nmi as u64);  // #NMI — IST2
    idt.set_handler(8,  handler_df as u64);   // #DF — IST1
    idt.set_handler(13, handler_gp as u64);   // #GP general protection
    idt.set_handler(14, handler_pf as u64);   // #PF page fault
    idt.set_handler(18, handler_mc as u64);   // #MC — IST3
    idt.set_handler(32, handler_timer as u64);// IRQ0 timer (LAPIC vector 32)
    idt.set_handler(36, handler_uart as u64); // COM1 RX+TX (LAPIC vector 36)
    // Vectors 40–47: PCI device IRQs (GSI 8–15)
    for gsi in 0..8 {
        idt.set_handler(40 + gsi, pci_irq_handlers[gsi]);
    }
    // All other vectors: catch-all stub (log + EOI + iretq)
    for v in (1..256).filter(|v| !used.contains(v)) {
        idt.set_handler(v, catch_all_stub as u64);
    }
}
```

Having a handler for all 256 vectors is a safety requirement: an unhandled
interrupt that finds a null IDT entry causes a #GP or triple fault.  Rost's
catch-all stub logs the vector number and continues — no triple fault is possible.

## 5.5 The TSS Descriptor in the GDT

The TSS descriptor is a 16-byte entry (spans two 8-byte GDT slots) because it
needs to hold a 64-bit base address:

```rust
pub fn install_tss(gdt: &mut GlobalDescriptorTable, tss: &TaskStateSegment) {
    let base = tss as *const _ as u64;
    let limit = (core::mem::size_of::<TaskStateSegment>() - 1) as u32;

    // Low 8 bytes (slot 5)
    gdt.entries[5] =
        ((limit & 0xFFFF) as u64) |              // limit[15:0]
        (((base & 0xFFFFFF) as u64) << 16) |     // base[23:0]
        (0x89u64 << 40) |                        // type=TSS64-available, P=1
        (((limit >> 16) & 0xF) as u64 << 48) |  // limit[19:16]
        (((base >> 24) & 0xFF) as u64 << 56);    // base[31:24]

    // High 8 bytes (slot 6)
    gdt.entries[6] = (base >> 32) as u64;        // base[63:32]

    // Load the Task Register
    core::arch::asm!("ltr ax", in("ax") 0x28u16); // selector 0x28
}
```

After `ltr 0x28`, the CPU reads TSS.RSP0 when transitioning from ring-3 to ring-0.

## 5.6 CR4.FSGSBASE

Setting `CR4.FSGSBASE` (bit 16) enables the `RDFSBASE`/`WRFSBASE` instructions
for ring-3 code.  The FS and GS segment base registers are the conventional
mechanism for thread-local storage in System V ABI code.  Without FSGSBASE,
updating these registers requires a syscall; with FSGSBASE they can be updated
directly in user space.

This is important for the POSIX compatibility layer (Chapter 22) and any future
work supporting multi-threaded processes.

## 5.7 The Context Save Area

When the kernel performs a context switch, it saves and restores all 18 architectural
registers.  The layout is precisely specified in `TaskContext`:

```rust
/// Layout is load-bearing — switch_context indexes this struct
/// with hard-coded byte offsets.
#[repr(C)]
pub struct TaskContext {
    // Callee-saved (System V AMD64 ABI)
    pub rbx:    u64,  //   0
    pub rbp:    u64,  //   8
    pub r12:    u64,  //  16
    pub r13:    u64,  //  24
    pub r14:    u64,  //  32
    pub r15:    u64,  //  40
    // Caller-saved (populated by full preemptive save; zero for voluntary)
    pub rax:    u64,  //  48
    pub rcx:    u64,  //  56
    pub rdx:    u64,  //  64
    pub rsi:    u64,  //  72
    pub rdi:    u64,  //  80
    pub r8:     u64,  //  88
    pub r9:     u64,  //  96
    pub r10:    u64,  // 104
    pub r11:    u64,  // 112
    // Key state registers
    pub rsp:    u64,  // 120
    pub rip:    u64,  // 128
    pub rflags: u64,  // 136
}
```

The comment "layout is load-bearing" is important.  The `switch_context` function
in `arch_x86_64::context` is a naked function (no Rust prologue/epilogue) that
uses hard-coded offsets to access fields.  If you add a field before `rbx` or
change the field order, the offsets will shift and `switch_context` will save/restore
the wrong registers — a silent, catastrophic bug.

The `#[repr(C)]` attribute ensures the compiler does not reorder fields, and the
all-`u64` field types prevent hidden padding.

## 5.8 The `switch_context` Naked Function

```rust
#[naked]
pub unsafe extern "C" fn switch_context(
    old: *mut TaskContext,
    new: *const TaskContext,
    new_pml4: u64,
) {
    core::arch::naked_asm!(
        // Save callee-saved registers of old context
        "mov [rdi + 0],   rbx",
        "mov [rdi + 8],   rbp",
        "mov [rdi + 16],  r12",
        "mov [rdi + 24],  r13",
        "mov [rdi + 32],  r14",
        "mov [rdi + 40],  r15",
        "mov [rdi + 120], rsp",
        "lea rax, [rip + 1f]",
        "mov [rdi + 128], rax",  // save return address as rip
        "mov [rdi + 136], 0x202", // IF=1

        // Load new CR3 if needed (non-zero new_pml4)
        "test rdx, rdx",
        "jz 2f",
        "mov cr3, rdx",

        // Restore callee-saved registers of new context
        "2:",
        "mov rbx, [rsi + 0]",
        "mov rbp, [rsi + 8]",
        "mov r12, [rsi + 16]",
        "mov r13, [rsi + 24]",
        "mov r14, [rsi + 32]",
        "mov r15, [rsi + 40]",
        "mov rsp, [rsi + 120]",

        // Return to new context's instruction pointer
        "ret",   // pops saved RIP (entry point for first-run, or saved "1f")
        "1:",
        "ret",   // old context resumes here after being switched back to
    );
}
```

The `#[naked]` attribute tells the compiler to emit no prologue (no `push rbp`,
no stack alignment, no red-zone adjustments).  The function is entirely under the
programmer's control — necessary because we are manually managing the kernel stack
during context switch.

For first-time execution of a new process, the "return address" at the top of the
kernel stack is the entry point of that process (written by `PCB::new()`).  The
`ret` pops it and jumps there.  For a process being resumed, `1f` is the return
address saved in the old context's `rip` field, so it resumes after the `ret`.

## 5.9 The Ring-3 Entry Trampoline

```rust
#[naked]
pub unsafe extern "C" fn ring3_entry_trampoline() {
    core::arch::naked_asm!(
        // r12 = user entry point (set by PCB::new_ring3)
        // r13 = user stack top   (set by PCB::new_ring3)
        "mov rcx, r12",          // RCX = user RIP for IRETQ
        "mov rsp, r13",          // RSP = user stack for IRETQ
        "push 0x1B",             // SS = ring-3 data (0x18 | 3)
        "push r13",              // user RSP
        "pushfq",                // RFLAGS (with IF=1)
        "or  qword ptr [rsp], 0x200",  // ensure IF=1
        "push 0x23",             // CS = ring-3 code (0x20 | 3)
        "push rcx",              // user RIP
        "iretq",                 // switch to ring-3!
    );
}
```

This trampoline runs in ring-0 as the "entry point" of a newly-spawned ring-3
process.  It reads the user entry point from `r12` and user stack from `r13`
(placed there by `PCB::new_ring3`), constructs an IRETQ frame, and executes
`iretq` to atomically switch to ring-3 (CPL=3).

The IRETQ frame has five words: RIP, CS, RFLAGS, RSP, SS.  After IRETQ:
- CS = 0x23 = ring-3 code segment (selector 0x20 with RPL=3)
- SS = 0x1B = ring-3 data segment (selector 0x18 with RPL=3)
- RIP = user entry point
- RSP = user stack top
- RFLAGS = saved flags with IF=1 (interrupts enabled)

The process is now in ring-3 at its ELF entry point with a fresh stack.

## 5.10 Summary

Rost's CPU structure setup establishes:

1. **GDT** with ring-0 and ring-3 code/data segments, and a TSS descriptor
2. **TSS** with RSP0 (kernel stack for ring-3→ring-0 transitions) and IST stacks
   for #DF, #NMI, #MC
3. **IDT** with handlers for all 256 vectors, including exception handlers,
   timer ISR, UART ISR, and PCI device IRQs
4. **Memory protection** (CR0.WP, CR4.SMEP, CR4.SMAP, EFER.NXE, CR4.FSGSBASE)
5. **`TaskContext`** and `switch_context` — the core of cooperative and preemptive
   context switching
6. **`ring3_entry_trampoline`** — the gateway from ring-0 to ring-3 for newly
   spawned processes

Together, these structures implement the hardware security boundary between kernel
and user space, and between different user processes.
