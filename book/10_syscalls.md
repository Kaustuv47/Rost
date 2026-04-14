# Chapter 10 — System Calls

## 10.1 The SYSCALL/SYSRET Mechanism

The SYSCALL instruction is the fast path for ring-3 → ring-0 transitions on
x86-64.  Unlike the older `INT 0x80` mechanism, SYSCALL:

- Does not push an IRETQ frame on the stack
- Does not perform a stack switch automatically
- Is about 40% faster than INT on modern CPUs

The tradeoff is that the kernel must perform the stack switch manually and must
restore the user stack before SYSRETQ.

### 10.1.1 MSR Configuration

```rust
pub fn init_syscall() {
    // Enable SYSCALL/SYSRET in EFER
    let efer = rdmsr(MSR_EFER);
    wrmsr(MSR_EFER, efer | 1);  // set SCE bit

    // STAR: ring-0 CS = 0x08, ring-3 base = 0x10
    // SYSRET: CS = 0x10+16 = 0x20, SS = 0x10+8 = 0x18
    wrmsr(MSR_STAR, (0x0008u64 << 32) | (0x0010u64 << 48));

    // LSTAR: 64-bit entry point
    wrmsr(MSR_LSTAR, syscall_entry as u64);

    // SFMASK: clear IF (bit 9) and DF (bit 10) on entry
    wrmsr(MSR_SFMASK, 0x600);
}
```

SFMASK clears IF (disabling interrupts) and DF (direction flag) on SYSCALL entry.
Interrupts are disabled until the kernel explicitly re-enables them or returns to
user space via SYSRETQ.

### 10.1.2 The Entry Stub

```asm
syscall_entry:
    ; On entry:
    ;   RCX = saved user RIP (by CPU)
    ;   R11 = saved user RFLAGS (by CPU)
    ;   RSP = user stack (NOT switched yet — SMAP risk)
    ;   RAX = syscall number
    ;   RDI, RSI, RDX, R10, R8, R9 = arguments

    ; Step 1: Save user RSP before switching stacks
    ;   (SMAP would reject a write to user stack via STAC, but we're not there yet)
    mov  qword ptr [SYSCALL_USER_RSP_SAVE], rsp

    ; Step 2: Switch to kernel stack
    ;   SYSCALL_KERN_RSP is the mirror of TSS.RSP0 for the current process
    mov  rsp, qword ptr [SYSCALL_KERN_RSP]

    ; Step 3: Save callee-saved registers + RCX/R11 (contain user RIP/RFLAGS)
    push r15
    push r14
    push r13
    push r12
    push rbx
    push rbp
    push rcx   ; user RIP
    push r11   ; user RFLAGS

    ; Step 4: Dispatch to Rust handler
    ;   RDI=arg0, RSI=arg1, RDX=arg2, R10=arg3, R8=arg4, R9=arg5 (already set)
    call dispatch_syscall

    ; Step 5: Restore registers
    pop  r11
    pop  rcx
    pop  rbp
    pop  rbx
    pop  r12
    pop  r13
    pop  r14
    pop  r15

    ; Step 6: Restore user RSP from saved location
    mov  rsp, qword ptr [SYSCALL_USER_RSP_SAVE]

    ; Step 7: Return to user space (restores RIP from RCX, RFLAGS from R11)
    sysretq
```

The key insight is step 1: user RSP must be saved BEFORE the stack switch,
because after `mov rsp, kernel_rsp`, writing to the old user RSP would be
a SMAP violation (the old address has PTE_USER set and SMAP is active).
By saving it to a static (`SYSCALL_USER_RSP_SAVE`), we avoid this problem.

### 10.1.3 SMAP-Safe User Buffer Access

After switching to the kernel stack, the kernel still needs to read from / write
to user-space buffers (e.g., the 72-byte message buffer for SYS_SEND_MSG).
These accesses are protected by the `SmapGuard`:

```rust
pub extern "C" fn dispatch_syscall(
    syscall_num: u64,
    a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64,
) -> u64 {
    let _guard = SmapGuard::new();  // STAC: allow user memory access
    // All user pointer dereferences in this function are safe under STAC

    match syscall_num {
        SYS_YIELD => /* ... */,
        SYS_EXIT  => /* ... */,
        // ...
    }
    // SmapGuard::drop() calls CLAC when the function returns
}
```

## 10.2 Argument Validation

Every syscall that accepts a user-space pointer validates it before use:

```rust
fn validate_user_ptr(ptr: u64, size: usize, align: usize) -> bool {
    if ptr == 0 { return false; }  // null
    if ptr % align as u64 != 0 { return false; }  // alignment
    if ptr.checked_add(size as u64).is_none() { return false; }  // overflow
    ptr + size as u64 <= USER_VA_END  // must be in user space
}

const USER_VA_END: u64 = 0x0000_8000_0000_0000;  // 128 TB, 4-level paging canonical limit
```

Even with SMAP enabled, a crafty user process could supply a kernel virtual address
(above `USER_VA_END`).  The range check prevents this.

For virtual address arguments (SYS_MAP target address):

```rust
fn validate_user_vaddr(vaddr: u64) -> bool {
    vaddr != 0
    && vaddr % 4096 == 0           // 4 KB aligned
    && vaddr + 4096 <= USER_VA_END // within user space
}
```

## 10.3 The Complete Syscall Table

### 10.3.1 Process Control

| # | Name | Arguments | Returns |
|---|------|-----------|---------|
| 0 | SYS_YIELD | — | 0 |
| 1 | SYS_EXIT | a0=exit_code | (noreturn) |
| 2 | SYS_GETPID | — | current PID |
| 8 | SYS_SPAWN | a0=entry, a1=pml4 (0=kernel), a2=priority | new PID or EINVAL |
| 15 | SYS_SETPRIO | a0=pid (0=self), a1=priority (0–255) | 0 |
| 28 | SYS_LIST_PROCS | a0=buf_ptr, a1=buf_len | count of entries |
| 26 | SYS_SPAWN_ELF | a0=elf_ptr, a1=elf_len, a2=priority | new PID or EINVAL |
| 27 | SYS_RESTART_SERVER | a0=name_ptr (16 bytes) | new PID or EINVAL |

### 10.3.2 Memory Management

| # | Name | Arguments | Returns |
|---|------|-----------|---------|
| 9 | SYS_MAP | a0=vaddr, a1=paddr (0=alloc), a2=flags | 0 or ENOMEM |
| 22 | SYS_MAP_SHARE | a0=vaddr, a1=flags | cap_slot or ENOMEM |
| 23 | SYS_MAP_CAP | a0=vaddr, a1=cap_slot, a2=flags | 0 or EPERM |
| 31 | SYS_PHYS_ADDR | a0=vaddr | phys_addr or 0 |
| 33 | SYS_GET_FRAMEBUF | a0=FbQueryResult_ptr | 0 or ENODEV |

### 10.3.3 IPC

| # | Name | Arguments | Returns |
|---|------|-----------|---------|
| 3 | SYS_SEND | a0=to_pid, a1=w0, a2=w1 | 0 or EINVAL |
| 4 | SYS_RECV | a0=timeout_ticks | word0 or MAX |
| 5 | SYS_NOTIFY | a0=to_pid, a1=word | 0 |
| 6 | SYS_RECV_MSG | a0=timeout, a1=buf_ptr | 0 or ETIMEDOUT |
| 7 | SYS_SEND_MSG | a0=to_pid, a1=buf_ptr | 0 or EAGAIN |
| 17 | SYS_CALL | a0=pid, a1=send_ptr, a2=reply_ptr, a3=timeout | 0/ETIMEDOUT/EDEADLK |

### 10.3.4 Capabilities

| # | Name | Arguments | Returns |
|---|------|-----------|---------|
| 18 | SYS_CAP_GRANT | a0=slot_idx, a1=to_pid | new slot or EPERM/EINVAL |
| 20 | SYS_CHAN_BIND | a0=target_pid, a1=rights | cap_slot or EINVAL |
| 21 | SYS_SEND_CAP | a0=cap_slot, a1=buf_ptr | 0 or EPERM |
| 24 | SYS_LOOKUP_CAP | a0=name_ptr | cap_slot or ENOENT |

### 10.3.5 Services

| # | Name | Arguments | Returns |
|---|------|-----------|---------|
| 10 | SYS_REGISTER | a0=name_ptr (16 bytes) | 0 or EINVAL |
| 11 | SYS_LOOKUP | a0=name_ptr | PID or ENOENT |

### 10.3.6 Hardware Access (Ring-3 Drivers)

| # | Name | Arguments | Returns |
|---|------|-----------|---------|
| 12 | SYS_UART_WRITE | a0=byte | 0 |
| 13 | SYS_UART_READ | — | byte or MAX |
| 29 | SYS_IOPORT_OUT | a0=port, a1=value | 0 |
| 30 | SYS_IOPORT_IN | a0=port | byte/word/dword |
| 32 | SYS_IRQ_REGISTER | a0=GSI (8–15), a1=isr_port | 0 or EINVAL |

### 10.3.7 Scheduling

| # | Name | Arguments | Returns |
|---|------|-----------|---------|
| 14 | SYS_CLOCK | — | nanoseconds since boot |
| 16 | SYS_SETRT | a0=pid, a1=period_ticks | 0 |
| 19 | SYS_SETQUOTA | a0=pid, a1=pages, a2=cpu_ticks, a3=ipc_rate | 0 |

## 10.4 SYS_MAP in Detail

`SYS_MAP` is one of the most complex syscalls because it touches several kernel
subsystems:

```rust
SYS_MAP => {
    let vaddr = a0;
    let paddr = a1;   // 0 = allocate a fresh frame
    let flags = a2;   // bit 0 = writable, bit 2 = user (always forced true)

    // 1. Validate virtual address
    if !validate_user_vaddr(vaddr) { return EINVAL; }

    // 2. Check memory quota
    if !sched.check_memory_quota(current_pid) { return ENOMEM; }

    // 3. Allocate physical frame if not provided
    let phys = if paddr == 0 {
        match global_alloc_4k() {
            Some(p) => { unsafe { write_bytes(p as *mut u8, 0, 4096); } p }
            None    => return ENOMEM,
        }
    } else {
        paddr
    };

    // 4. Compute PTE flags
    let mut pte_flags = PTE_PRESENT | PTE_USER;
    if flags & 1 != 0 { pte_flags |= PTE_WRITABLE; }
    if flags & 4 == 0 { pte_flags |= PTE_NO_EXECUTE; }

    // 5. Map into process's PML4
    let pml4_phys = sched.get_process_pml4(current_pid).unwrap_or(0);
    let pml4 = unsafe { &mut *(pml4_phys as *mut PageTable) };
    if !map_page_global(pml4, vaddr, phys, pte_flags) { return ENOMEM; }

    // 6. Update memory usage counter
    sched.use_memory_page(current_pid);

    0  // success
}
```

The memory quota check (step 2) is what implements IEC 61508 §7.4.5.  A ring-3
process with `memory_quota_pages = 256` can never map more than 1 MB of physical
memory, regardless of how many `SYS_MAP` calls it makes.

## 10.5 SYS_YIELD in Detail

Yield is deceptively simple but critically important:

```rust
SYS_YIELD => {
    if let Some(sched) = get_global() {
        sched.yield_switch();
    }
    0
}
```

`yield_switch()`:
1. Resets the current process's quantum to 0
2. Sets the current process to Ready
3. Calls `pick_next_priority()` to find the next process
4. Performs `switch_context_noints()` — immediate context switch
5. Programs the LAPIC one-shot timer for the next deadline

Without the `arm_oneshot(1)` call, a process that yields in a tight loop could
prevent deadline-based wakeups from firing.

## 10.6 SYS_SPAWN_ELF in Detail

```rust
SYS_SPAWN_ELF => {
    let elf_ptr = a0;
    let elf_len = a1 as usize;
    let priority = a2 as u8;

    // Validate the user pointer
    if !validate_user_ptr(elf_ptr, elf_len, 1) { return EINVAL; }

    // Call the kernel ELF spawn hook (registered at Stage 6)
    let new_pid = unsafe { (ELF_SPAWN_FN)(elf_ptr as *const u8, elf_len, priority) };

    if new_pid == u32::MAX { EINVAL } else { new_pid as u64 }
}
```

The indirection through `ELF_SPAWN_FN` (a function pointer registered at Stage 6)
is a deliberate architectural decision.  The `arch-x86_64` crate (which contains
the syscall handler) does not directly depend on the `kernel` crate (which contains
the ELF loader).  Instead, the `kernel` crate registers a hook at boot that the
syscall handler calls through the function pointer.  This breaks what would
otherwise be a circular dependency.

## 10.7 SYS_IRQ_REGISTER in Detail

`SYS_IRQ_REGISTER` lets ring-3 drivers claim hardware IRQs:

```rust
SYS_IRQ_REGISTER => {
    let gsi = a0 as u8;
    let isr_port = a1 as u16;

    // Only GSIs 8–15 are delegatable (0–7 are PIC-managed, below IOAPIC range)
    if gsi < 8 || gsi > 15 { return EINVAL; }

    // Get the IOAPIC base (set during Stage 3)
    let ioapic_base = get_ioapic_base();
    if ioapic_base == 0 { return EINVAL; }

    // Register in irq_registry
    irq_registry::register(gsi, current_pid, isr_port);

    // Program the IOAPIC to route GSI → vector 32+GSI → LAPIC → kernel ISR
    ioapic::route_irq(ioapic_base, gsi - 0, 32 + gsi, 0 /* LAPIC ID */);

    0
}
```

After this, when the device raises its IRQ:
1. IOAPIC routes it to LAPIC vector `32 + gsi`
2. IDT entry `32 + gsi` fires the kernel's `pci_irq_gsiN` stub
3. The stub calls `handle_pci_irq(gsi)` which delivers an IPC to the owner
4. The owner process receives `data[0] = 0xFFFF_0000 | gsi` in its next `SYS_RECV_MSG`

## 10.8 Error Codes

```rust
pub const EINVAL:    u64 = u64::MAX;       // -1 (invalid argument)
pub const EPERM:     u64 = u64::MAX - 1;   // -2 (permission denied)
pub const ENOMEM:    u64 = u64::MAX - 2;   // -3 (out of memory)
pub const EIO:       u64 = u64::MAX - 3;   // -4 (I/O error)
pub const EAGAIN:    u64 = u64::MAX - 4;   // -5 (try again)
pub const ETIMEDOUT: u64 = u64::MAX - 5;   // -6 (timed out)
pub const ENODEV:    u64 = u64::MAX - 6;   // -7 (no such device)
pub const EDEADLK:   u64 = u64::MAX - 7;   // -8 (deadlock detected)
pub const ENOENT:    u64 = u64::MAX - 8;   // -9 (not found)
pub const ENOSYS:    u64 = u64::MAX - 9;   // -10 (not implemented)
```

Errors are encoded as `u64::MAX - N` so they sit at the very top of the u64
range, well away from valid return values (PIDs, addresses, counts).

## 10.9 The Fault-Injection Syscall

For testing purposes, a special syscall exists in `--features fault-injection` builds:

```rust
#[cfg(feature = "fault-injection")]
SYS_INJECT_FAULT => {
    let vector = a0 as u8;
    match vector {
        3  => unsafe { core::arch::asm!("int3"); }   // #BP
        6  => unsafe { core::arch::asm!("ud2"); }    // #UD
        13 => unsafe { core::arch::asm!("mov rax, cr4; or rax, 0; mov cr4, rax"); } // GP
        14 => unsafe { /* null deref */ *(0u64 as *mut u64) = 0; }
        _  => {}
    }
    0
}
```

This syscall is absent in production builds (falls through to ENOSYS).
It is used by the QEMU-based system test suite to verify that every exception
handler path is reachable and produces the correct behavior.

IEC 61508 §7.4.7 requires that every exception handler path is tested.  Fault
injection provides a systematic way to exercise those paths without waiting for
natural faults to occur.

## 10.10 Summary

Rost's syscall interface provides:

- **34 syscalls** covering process management, memory, IPC, capabilities, hardware
  access, and scheduling
- **SMAP-safe entry** — user RSP saved before stack switch; all user buffer
  accesses bracketed by STAC/CLAC
- **Argument validation** — all user pointers validated before use
- **Function-pointer hooks** — ELF spawn and server restart are decoupled from
  the syscall handler via registered hooks
- **Fault injection** — debug-build feature for testing all exception paths
- **Consistent error encoding** — errors at the top of u64 range (u64::MAX – N)
