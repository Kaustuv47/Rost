# Chapter 6 — Interrupt & Exception Handling

## 6.1 The Role of Interrupts in Rost

Interrupts and exceptions are the mechanism by which hardware events (timer tick,
keyboard press, network packet, disk completion) and software errors (divide by
zero, page fault, general protection fault) reach the kernel.

In a safety-critical system, the interrupt handling code is among the most
security-sensitive code in the entire kernel.  A bug in an interrupt handler runs
with interrupts disabled, at ring-0, with complete access to all hardware.  If the
handler corrupts kernel state, the entire system is compromised.

Rost's interrupt handling design has three guiding principles:

1. **No triple faults** — all 256 IDT vectors are handled.  A catch-all stub logs
   the vector number and continues.
2. **No silent state corruption** — every ring-0 exception halts after logging.
   A kernel bug should be visible and diagnosable, never silently ignored.
3. **User-mode fault isolation** — ring-3 exceptions terminate the faulting process
   and notify the health monitor, without halting the system.

## 6.2 Exception Frame Layout

Every exception handler begins by saving all registers.  The layout is carefully
chosen so that C-ABI functions can receive the frame as a `*const ExceptionFrame`
argument:

```rust
#[repr(C)]
pub struct ExceptionFrame {
    // Pushed by the ISR stub (reversed from push order)
    pub r15:        u64,
    pub r14:        u64,
    pub r13:        u64,
    pub r12:        u64,
    pub r11:        u64,
    pub r10:        u64,
    pub r9:         u64,
    pub r8:         u64,
    pub rdi:        u64,
    pub rsi:        u64,
    pub rdx:        u64,
    pub rcx:        u64,
    pub rbx:        u64,
    pub rbp:        u64,
    pub rax:        u64,
    // Synthesized: 0 for exceptions without a hardware error code
    pub error_code: u64,
    // Pushed by CPU on exception
    pub rip:        u64,
    pub cs:         u64,
    pub rflags:     u64,
    // rsp / ss only present on ring-3 → ring-0 transition
}
```

A naked ISR stub in assembly saves all registers, pushes either the hardware error
code (for exceptions that provide one) or 0, then calls the Rust handler:

```asm
; Example: #GP handler stub
handler_gp_stub:
    ; hardware already pushed: error_code, rip, cs, rflags
    push rax
    push rbp
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov  rdi, rsp          ; first arg = &ExceptionFrame
    call handle_gp
    ; ... restore and iretq
```

### 6.2.1 Detecting Ring-3 Faults

The CS field in the exception frame carries the CPL at the time of the fault:

```rust
#[inline(always)]
fn from_user(f: &ExceptionFrame) -> bool {
    f.cs & 3 == 3   // CPL=3 means ring-3
}
```

This single check determines whether a fault should terminate the user process
(ring-3) or halt the system (ring-0).

## 6.3 Exception Handlers

### 6.3.1 #DE — Divide Error (Vector 0)

Occurs when the divisor in a `DIV` or `IDIV` instruction is zero, or when the
quotient doesn't fit in the destination register.

```rust
pub extern "C" fn handle_de(f: &ExceptionFrame) {
    if from_user(f) {
        // Ring-3: terminate the faulting process
        terminate_faulting_process(0xDE);
    } else {
        // Ring-0: fatal — log and halt
        hal::uart::print_str("FATAL #DE: divide error in kernel\n");
        dump_registers(f);
        crash_log::write(0, TICK_COUNT.load(Ordering::Relaxed),
                         CURRENT_PID.load(Ordering::Relaxed),
                         f.rip, f.rflags, 0);
        // Draw panic screen on GOP framebuffer
        core_kernel::framebuf::panic_screen(b"#DE", f.rip, f.error_code, 0);
        loop { core::arch::asm!("cli; hlt"); }
    }
}
```

### 6.3.2 #GP — General Protection (Vector 13)

The most common kernel exception — triggered by any privilege violation: accessing
a segment beyond its limit, executing a privileged instruction from ring-3, a null
segment selector reference, misaligned stack operations, etc.

Ring-3 #GPs terminate the process.  Ring-0 #GPs are fatal — they indicate a kernel
bug and the system halts.

```rust
pub extern "C" fn handle_gp(f: &ExceptionFrame) {
    if from_user(f) {
        hal::uart::print_str("GP fault in ring-3, PID=");
        hal::uart::print_hex(CURRENT_PID.load(Ordering::Relaxed) as u64);
        hal::uart::print_str("\n");
        terminate_faulting_process(0x0D);
    } else {
        hal::uart::print_str("FATAL #GP in kernel: rip=");
        hal::uart::print_hex(f.rip);
        hal::uart::print_str(" err=");
        hal::uart::print_hex(f.error_code);
        hal::uart::print_str("\n");
        dump_registers(f);
        crash_log::write(13, ...);
        core_kernel::framebuf::panic_screen(b"#GP", f.rip, f.error_code, 0);
        loop { core::arch::asm!("cli; hlt"); }
    }
}
```

### 6.3.3 #PF — Page Fault (Vector 14)

Page faults have additional context: the CR2 register contains the virtual address
that caused the fault.  The error code provides more detail:

| Bit | Name | Meaning |
|-----|------|---------|
| 0 | P | 0=not-present, 1=protection violation |
| 1 | W | 0=read, 1=write |
| 2 | U | 0=supervisor, 1=user |
| 3 | RSVD | Reserved bit set in PTE |
| 4 | I | Instruction fetch |

```rust
pub extern "C" fn handle_pf(f: &ExceptionFrame) {
    let cr2: u64;
    unsafe { core::arch::asm!("mov {}, cr2", out(reg) cr2); }

    if from_user(f) {
        hal::uart::print_str("PF in ring-3: cr2=");
        hal::uart::print_hex(cr2);
        hal::uart::print_str("\n");
        terminate_faulting_process(0x0E);
    } else {
        hal::uart::print_str("FATAL #PF: rip=");
        hal::uart::print_hex(f.rip);
        hal::uart::print_str(" cr2=");
        hal::uart::print_hex(cr2);
        hal::uart::print_str("\n");
        dump_registers(f);
        crash_log::write(14, TICK, PID, f.rip, f.rflags, cr2);
        core_kernel::framebuf::panic_screen(b"#PF", f.rip, f.error_code, cr2);
        loop { core::arch::asm!("cli; hlt"); }
    }
}
```

### 6.3.4 #DF — Double Fault (Vector 8, IST1)

A double fault occurs when an exception fires while the CPU is already handling
another exception.  The most common cause is a kernel stack overflow: the #GP
for a stack fault fires, but the stack is corrupt so the #GP handler itself
faults, causing #DF.

Because #DF uses IST1, it always runs on a fresh, dedicated stack — even if all
kernel stacks are corrupted.  The handler logs and halts:

```rust
pub extern "C" fn handle_df(f: &ExceptionFrame) {
    crash_log::write(8, ...);
    core_kernel::framebuf::panic_screen(b"#DF", f.rip, f.error_code, 0);
    loop { core::arch::asm!("cli; hlt"); }
}
```

### 6.3.5 #NMI — Non-Maskable Interrupt (Vector 2, IST2)

Non-maskable interrupts cannot be disabled by `cli`.  They typically indicate
hardware errors (memory ECC correction, thermal throttling).  Rost's NMI handler
simply logs and continues — it does not halt, because NMIs are often informational.

### 6.3.6 #MC — Machine Check (Vector 18, IST3)

Machine check exceptions indicate uncorrectable hardware errors (bad memory,
unrecoverable cache errors).  The handler halts — there is no safe way to continue
after an uncorrectable hardware error.

## 6.4 The Timer ISR (Vector 32)

The timer ISR is the heartbeat of the scheduler.  It runs at 100 Hz (every 10 ms),
increments `TICK_COUNT`, sends the LAPIC EOI, and calls `tick_scheduler_isr()`.

```asm
handler_timer:
    ; CPU pushed: rip, cs, rflags, [rsp, ss]
    push rax
    push rcx
    push rdx
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11

    ; Increment tick counter
    lock inc qword ptr [TICK_COUNT]

    ; Send LAPIC EOI (required before re-enabling hardware to allow
    ; the next interrupt; must be done BEFORE calling tick_scheduler_isr
    ; so that if tick_scheduler_isr takes a long time, the next timer
    ; interrupt is not lost)
    mov  rax, qword ptr [LAPIC_EOI_ADDR]
    test rax, rax
    jz   .skip_eoi
    mov  dword ptr [rax], 0
.skip_eoi:

    ; Preemptive context switch
    call tick_scheduler_isr

    pop  r11
    pop  r10
    pop  r9
    pop  r8
    pop  rdi
    pop  rsi
    pop  rdx
    pop  rcx
    pop  rax
    iretq
```

`tick_scheduler_isr()` calls `timer_tick()` on the global scheduler, which
advances the tick counter, checks deadlines, accounts CPU time, and may preempt
the current process.  If preemption occurs, `tick_scheduler_isr` calls
`switch_context_noints` to perform the context switch.

The key detail: `IRETQ` restores RFLAGS from the interrupted task's saved state,
including the IF (interrupt flag).  This means interrupts are automatically re-enabled
when control returns to a process that had them enabled — true preemption, not a
cooperative yield.

## 6.5 The UART ISR (Vector 36)

The UART ISR handles COM1 serial port events.  A single ISR handles both
transmit (TX) and receive (RX) by reading the Interrupt Identification Register
(IIR) to determine the cause:

```rust
pub extern "C" fn handle_uart_rx(/* ... */) {
    let iir = hal::uart::read_iir();  // read IIR (also clears the interrupt)

    match iir & 0x0F {
        0x02 => {
            // IIR_THRE: Transmit Holding Register Empty
            // The TX ring buffer has bytes to send — drain it
            hal::uart::tx_isr();
        }
        0x04 | 0x0C => {
            // IIR_RDA or IIR_CTI: Receive Data Available / Character Timeout
            // Drain the RX FIFO and forward each byte to uart-drv
            while let Some(byte) = hal::uart::try_read_byte() {
                // Send IPC to uart-drv (PID 3)
                notify_uart_drv(byte);
            }
            // Context-switch immediately to uart-drv so keystroke
            // latency is within one scheduler quantum
            tick_scheduler_isr();
        }
        _ if iir & 1 != 0 => {
            // No interrupt pending (spurious) — just return
        }
        _ => {}
    }
    send_eoi();
}
```

### 6.5.1 Interrupt-Driven TX

The UART TX interrupt eliminates the busy-wait that previous COM1 implementations
use.  Instead:

1. `put_byte(b)` in `hal::uart` pushes the byte to a 255-byte SPSC ring buffer
2. If the TX holding register (THRE) is currently empty, it writes one byte directly
   (pump-priming)
3. It arms ETBEI (TX interrupt enable) so the UART will interrupt when THRE is empty again
4. When THRE fires (vector 36, IIR=0x02), `tx_isr()` pops the next byte from the ring
5. When the ring is empty, `tx_isr()` disables ETBEI to stop spurious interrupts

This pattern converts a busy-wait (spin on THRE) into an interrupt-driven pipeline —
zero CPU cycles wasted waiting for the UART.

## 6.6 PCI Device IRQs (Vectors 40–47)

Rost routes hardware IRQs from PCI devices (GSI 8–15) to vectors 40–47 in the IDT.
A macro generates the ISR stub for each GSI:

```rust
macro_rules! pci_irq_stub {
    ($gsi:expr) => {
        // Save caller-saved registers
        // Call handle_pci_irq($gsi)
        // Send PIC + LAPIC EOI
        // call tick_scheduler_isr (so the owner process runs promptly)
        // Restore + iretq
    }
}
```

`handle_pci_irq(gsi)` looks up the device owner in the `irq_registry`:

```rust
pub fn handle_pci_irq(gsi: u8) {
    if let Some((owner_pid, isr_port)) = irq_registry::lookup(gsi) {
        // Read device ISR status to acknowledge the interrupt
        let status = in_byte(isr_port);
        // Deliver notification to owner process
        send_message(KERNEL_PID, owner_pid, 0xFFFF_0000 | gsi as u64);
    }
}
```

The owner process receives the notification in its next `SYS_RECV_MSG` and handles
the device event in user space.  This is the microkernel driver model: the kernel
delivers the interrupt as an IPC message, and the ring-3 driver handles the rest.

## 6.7 The `terminate_faulting_process()` Function

When a ring-3 exception occurs, the kernel:
1. Terminates the faulting process via `Scheduler::terminate_process(current_pid)`
2. Sends an IPC notification to PID 1 (init) with the fault code and PID
3. Calls `tick_scheduler_isr()` to immediately schedule another process

```rust
fn terminate_faulting_process(fault_code: u64) {
    let pid = ProcessId::new(CURRENT_PID.load(Ordering::Relaxed));
    if let Some(sched) = core_kernel::scheduler::get_global() {
        sched.terminate_process(pid);
        // Notify init (PID 1) about the fault
        let mut msg = Message::zero();
        msg.data[0] = fault_code;
        msg.data[1] = pid.as_u32() as u64;
        let _ = sched.send_message(pid, ProcessId::new(1), msg);
    }
    // Force a context switch to another ready process
    tick_scheduler_isr();
}
```

This design means a crashing ring-3 process does not take down the entire system —
it is terminated, init is notified, and the scheduler picks a different process
to run.  If init determines the crash is unrecoverable, it initiates an ordered
shutdown (letting the watchdog reset the hardware).

## 6.8 The Persistent Crash Log

Before halting, ring-0 fault handlers write an error record to a fixed physical
address (0x4000) that survives a warm reset:

```rust
pub struct ErrorRecord {
    magic:      u64,   // 0xDEAD_C0DE_CAFE_BABE
    vector:     u8,
    _pad:       [u8; 7],
    tick:       u64,   // scheduler tick at fault time
    pid:        u32,   // process ID at fault time
    _pad2:      u32,
    rip:        u64,   // instruction pointer
    rflags:     u64,
    cr2:        u64,   // page fault address (or 0)
}
```

The log holds up to 16 records.  On the next boot, Stage 1 calls
`crash_log::drain()` which reads and prints all records, then zeros the region.
This provides post-mortem visibility into crashes even when there is no debugger.

IEC 61508 §7.4.6 requires that safety-critical systems provide post-mortem data
for failure analysis.  The crash log satisfies this requirement.

## 6.9 The GOP Panic Screen

In addition to the serial crash log, ring-0 exception handlers draw a panic screen
on the GOP framebuffer:

```rust
pub fn panic_screen(label: &[u8], rip: u64, err: u64, cr2: u64) {
    // Fill screen with dark red background
    // Draw label at top: "KERNEL PANIC: #GP"
    // Draw register values in white text
    // Draw "System halted" message
}
```

The panic screen is implemented in `core_kernel::framebuf` rather than in the
GOP server (which is a ring-3 process).  This is necessary because by the time
a ring-0 exception handler runs, the scheduler may have been stopped and the
GOP server may not be running.  The kernel draws directly to the MMIO framebuffer
using a built-in 8×8 bitmap font (96 glyphs, 768 bytes).

## 6.10 Catch-All Stub

For all vectors not explicitly handled, Rost installs a catch-all stub:

```asm
catch_all_stub:
    ; Log "unexpected interrupt vector=N" over serial
    ; EOI (both PIC and LAPIC)
    iretq
```

This ensures the system never triple-faults due to an unhandled interrupt.
Spurious interrupts (vector 255, sent by the LAPIC for deasserted interrupts)
are handled by an even simpler stub that just executes `iretq` with no EOI.

## 6.11 Summary

Rost's interrupt and exception handling provides:

- **Fault isolation** — ring-3 faults terminate the faulting process, not the system
- **Diagnostic visibility** — full register dump + crash log + GOP panic screen for
  all ring-0 faults
- **No triple faults** — all 256 IDT vectors handled, catch-all stub for unknown vectors
- **Preemptive scheduling** — timer ISR calls `tick_scheduler_isr` for true preemption
- **Interrupt-driven I/O** — UART TX uses interrupt-driven ring buffer; no busy-wait
- **IRQ delegation** — PCI device IRQs delivered as IPC to ring-3 driver owners
- **IEC 61508 compliance** — persistent crash log, watchdog supervision, IST stacks
  for NMI/#DF/#MC
