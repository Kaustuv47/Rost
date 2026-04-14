# Chapter 14 — Formal Verification & Testing

## 14.1 The Testing Pyramid

Rost uses a multi-layer testing strategy:

```
        ┌─────────────────┐
        │   QEMU System   │  TC-SYS-001 through TC-SYS-005
        │     Tests       │  Full boot, serial-capture harness
        ├─────────────────┤
        │  Fault Injection│  SYS_INJECT_FAULT feature
        │     Tests       │  Every exception path exercised
        ├─────────────────┤
        │  Unit Tests     │  123 tests in core-kernel
        │  (cargo test)   │  Run on host, no hardware needed
        ├─────────────────┤
        │  TLA+ Formal    │  Scheduler, IPC, Memory invariants
        │   Verification  │  Machine-checked proofs
        └─────────────────┘
```

IEC 61508 SIL 4 requires verification at all levels.  The combination of formal
verification, host-based unit tests, and QEMU system tests provides comprehensive
coverage.

## 14.2 Unit Tests

The `core-kernel` crate is the most testable part of the kernel.  It has no
architecture-specific code (`#![cfg_attr(not(test), no_std)]` allows `std` in
test mode), so all tests run on the development host with `cargo test`.

```bash
cargo test -p core-kernel --target x86_64-apple-darwin
# test result: ok. 123 passed; 0 failed; 0 ignored; 0 measured
```

### 14.2.1 Test Categories

**Scheduler tests** (20 tests):
- `test_priority_selection` — lowest number wins
- `test_round_robin_within_priority` — equal-priority processes alternate
- `test_edf_preempts_best_effort` — RT process preempts even priority-0 BE
- `test_edf_earliest_deadline_first` — min(rt_deadline) wins among RT
- `test_edf_period_renewal` — deadline advances by period (no drift)
- `test_priority_inheritance` — blocked client donates priority to server
- `test_deadlock_direct_cycle` — A→B→A detected as deadlock
- `test_deadlock_indirect_cycle` — A→B→C→A detected
- `test_memory_quota_unlimited` — 0 = no limit
- `test_memory_quota_enforced` — at limit, rejects further allocation
- `test_cpu_budget_frame_reset` — budget resets after 1000 ticks

**IPC tests** (5 tests):
- `test_empty_queue` — dequeue on empty returns None
- `test_fifo_order` — messages arrive in send order
- `test_full_rejects_send` — 17th send drops the message
- `test_circular_wrap` — head/tail wraparound works correctly
- `test_notification_or_consume` — OR accumulates, consume clears

**Memory tests** (19 tests in paging.rs + physical.rs):
- map+translate basic, map+translate with offset
- translate absent at PML4/PDPT/PD/PT level
- flags preserved through map/translate cycle
- allocation failure propagation
- shared intermediate table reuse (two VAs sharing a PT)
- unmap clears entry, unmap absent returns false
- huge page rejection by remap_page_flags
- remap_page_flags: preserves physical address, changes flags

**Process table tests** (18 tests):
- create, unique PIDs, get_ready_with_priority
- terminate_clears_slot, slot_reuse_after_terminate
- check_deadlines partial unblock
- 7 capability tests (alloc, find, grant, revoke, etc.)
- 5 deadline tests (earliest_blocked_deadline)
- 5 cycle detection + budget reset tests

**PCB tests** (3 tests):
- free out-of-range safe
- stack zeroed on alloc-from-reclaim
- reclaim LIFO reuse

**ACPI tests** (32 tests across madt.rs, fadt.rs, dmar.rs, hpet.rs):
- null/bad-sig/bad-checksum/too-short variants
- Valid parsing of each table type
- Edge cases (truncated entries, missing optional fields)

**Service registry tests** (7 tests):
- register_and_lookup
- lookup_nonexistent_returns_none
- register_overwrites_existing
- unregister_makes_lookup_none
- trim_name_stops_at_nul
- lookup_is_case_sensitive

**Timer/TSC tests** (11 tests):
- compute_khz (3 GHz, zero duration, 1 GHz cases)
- hpet_ticks_per_ms (10 MHz, 14 MHz, zero period)
- tsc_to_ns (zero before calibration, 3 GHz 3M ticks = 1 ms)

### 14.2.2 Test Design Principles

**No external state**: each test creates its own `ProcessTable` or
`Scheduler` instance.  Static global state (like `GLOBAL_SCHEDULER`) is never
touched by unit tests — tests use local instances.

**No architecture assumptions**: tests run on the host architecture (x86-64 macOS,
Linux, or Windows).  The `#![cfg_attr(not(test), no_std)]` pattern allows `std`
in test mode for `Vec`, `Box`, and test infrastructure.

**Deterministic behavior**: tests don't use wall-clock time or random values.
A test that relies on timing would be flaky; all tests are purely functional.

## 14.3 Code Coverage

```bash
# Generate HTML coverage report
scripts/coverage.sh --html

# Enforce minimum coverage threshold
THRESHOLD=80 scripts/coverage.sh
```

The coverage script uses `cargo llvm-cov`:

```bash
cargo llvm-cov \
    --target host \
    --package core-kernel \
    --lcov \
    --output-path target/lcov.info

if [ -n "$THRESHOLD" ]; then
    coverage=$(parse_lcov_line_rate target/lcov.info)
    if [ "$coverage" -lt "$THRESHOLD" ]; then
        echo "Coverage $coverage% is below threshold $THRESHOLD%"
        exit 1
    fi
fi
```

IEC 61508 SIL 4 requires MC/DC (Modified Condition/Decision Coverage).
MC/DC requires that each condition in a decision independently affects the outcome.
The unit tests are written to achieve this for all kernel modules.

## 14.4 QEMU System Tests

```bash
scripts/test-qemu.sh
# or: scripts/test-qemu.sh --no-build
```

The system test harness:
1. Builds the kernel and servers
2. Starts QEMU in headless mode with `-serial file:qemu_serial.log`
3. Monitors the log file for expected strings within a timeout
4. Reports PASS or FAIL for each test case

Test cases:
```
TC-SYS-001: boot-completes
  Assert: "Stage 1:" ... "Stage 8:" ... "Rost kernel ready"
  
TC-SYS-002: timer-hardware-init
  Assert: "HPET" AND "TSC calibrated"
  
TC-SYS-003: ring3-servers-spawned
  Assert: "PID 2" AND "PID 3" AND "PID 4"
  
TC-SYS-004: scheduler-ticks
  Assert: kernel reaches "ready" state (implies timer ISR is working)
  
TC-SYS-005: crash-log-drain
  Assert: "Stage 1:" AND "crash" (requires manually writing a crash record
          to 0x4000 before the test, or the assertion is skipped)
```

The `-watchdog-action poweroff` QEMU option ensures the VM terminates after the
watchdog timeout, preventing hung test runs.

## 14.5 Fault Injection Testing

```bash
# Build with fault injection enabled
cargo build --features fault-injection -p kernel
```

The fault-injection build adds `SYS_INJECT_FAULT` (syscall 25) which can trigger:
- `3` → #BP (breakpoint, caught by the catch-all stub)
- `6` → #UD (undefined instruction, caught by the catch-all stub)
- `13` → #GP (general protection fault)
- `14` → #PF (page fault via null pointer dereference)

A ring-3 test process calls `SYS_INJECT_FAULT(14)` to verify:
1. The #PF handler fires
2. Ring-3 is correctly detected (`cs & 3 == 3`)
3. The process is terminated via `terminate_faulting_process(0x0E)`
4. Init receives the fault notification
5. The system continues running with the remaining processes

IEC 61508 §7.4.7: every fault handler path must be tested.

## 14.6 TLA+ Formal Models

The `specs/` directory contains TLA+ specifications for the most critical kernel
invariants.

### 14.6.1 Scheduler.tla

```tla
VARIABLES processes, running, tick

TypeOK ==
    /\ processes \in [PID -> [state: StateType, priority: 0..255]]
    /\ running \in PID \union {None}
    /\ tick \in Nat

AtMostOneRunning == Cardinality({p \in PID : processes[p].state = "Running"}) <= 1

RunningConsistent ==
    running = None \/ processes[running].state = "Running"

NoStarvation ==
    \A p \in PID :
        processes[p].state = "Ready" ~> processes[p].state = "Running"
```

`NoStarvation` is a liveness property: every Ready process eventually runs.
This is checked with TLC's liveness verification using "weak fairness on Tick"
(the timer fires infinitely often).

### 14.6.2 IPC.tla

```tla
NeverSendWithoutCap ==
    \A sender, receiver \in PID :
        [](SendCap(sender, receiver) =>
            HasChannelCapFor(sender, receiver))
```

`NeverSendWithoutCap` is a safety property: a process using `SYS_SEND_CAP`
never sends a message without holding a valid Channel capability for the target.

### 14.6.3 Memory.tla

```tla
FrameIsolation ==
    \A p1, p2 \in PID :
        p1 # p2 =>
            frame_owner[p1] \intersect frame_owner[p2] = {}

PrivateFrameExclusive ==
    \A frame \in private_frames :
        Cardinality({p \in PID : frame \in frame_owner[p]}) <= 1
```

`FrameIsolation` states that no two processes share any physical frame
(unless shared via `SYS_MAP_SHARE`, which the model excludes for simplicity).

### 14.6.4 Running the Models

```bash
java -jar tla2tools.jar -config specs/Scheduler.cfg specs/Scheduler.tla
java -jar tla2tools.jar -config specs/IPC.cfg specs/IPC.tla
java -jar tla2tools.jar -config specs/Memory.cfg specs/Memory.tla
```

TLC explores all reachable states of the model and checks every invariant and
liveness property.  For the current model size (up to 4 processes), TLC typically
completes in under a minute.

## 14.7 Requirements Traceability Matrix

The `docs/traceability.md` file maps every requirement to its implementation
and tests:

```
REQ-ID      | Requirement                    | Module           | Tests
------------|--------------------------------|------------------|--------------------
REQ-MEM-001 | Physical frame reclaim         | pcb.rs::Drop     | test_pcb_drop_*
REQ-MEM-002 | Memory quota enforcement       | scheduler/rr.rs  | test_quota_*
REQ-PROC-001| Process termination cleanup    | table.rs         | test_terminate_*
REQ-IPC-001 | Sender PID unforgeable         | round_robin.rs   | test_sender_pid_*
REQ-SAFE-001| Watchdog supervision           | hal/watchdog.rs  | TC-SYS-001
REQ-SAFE-002| Persistent crash log           | crash_log.rs     | TC-SYS-005
...
```

The matrix covers 37 requirements across REQ-MEM, REQ-PROC, REQ-IPC, REQ-TIMER,
REQ-SAFE, REQ-ACPI, and REQ-SYSCALL domains.  Each requirement is classified by
IEC 61508 section and mapped to the implementation module(s) and test(s).

## 14.8 Summary

Rost's verification strategy provides:

- **123 unit tests** in `core-kernel` — run on host, no hardware needed
- **Code coverage** enforcement via `cargo llvm-cov`
- **5 QEMU system tests** — full boot verification in headless QEMU
- **Fault injection** — every exception handler path exercised
- **3 TLA+ models** — formal verification of scheduler, IPC, and memory invariants
- **Requirements traceability** — 37 requirements mapped to code and tests
- **Reproducible builds** — identical SHA-256 from identical sources
