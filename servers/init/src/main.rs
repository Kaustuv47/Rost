//! rost-init — PID 1 health monitor.
//!
//! This is the first user-space process spawned by the kernel (PID 1).
//! It owns the system lifecycle: it receives fault notifications from the
//! kernel, monitors registered services via heartbeat, and initiates an
//! ordered shutdown if a critical service fails unrecoverably.
//!
//! # IPC Protocol
//!
//! **Kernel → init (fault notification):**
//! ```text
//!   recv_msg: data[0] = fault_code (0xDE / 0x0D / 0x0E)
//!             data[1] = faulting PID
//!             sender  = kernel-stamped PID of faulting process
//! ```
//!
//! **Server → init (heartbeat):**
//! ```text
//!   send_msg: data[0] = OP_HEARTBEAT (0x0001)
//!             data[1] = sender's own PID (informational)
//! ```
//!
//! **Any process → init (shutdown request):**
//! ```text
//!   send_msg: data[0] = OP_SHUTDOWN (0x00FF)
//! ```
//!
//! # Restart Policy
//!
//! On a crash of a named server, init calls `SYS_RESTART_SERVER` (27) to ask
//! the kernel to re-spawn the server from its embedded ELF image.  Up to
//! `MAX_RESTARTS` attempts are made; after that init falls through to an
//! ordered shutdown.
//!
//! Critical services (uart-drv, vfs): crash → restart; if restart fails → halt.
//! Non-critical services (shell):     crash → restart; if restart fails → warn.

#![no_std]
#![no_main]

mod syscall;

// ── IPC opcodes ───────────────────────────────────────────────────────────────

/// Heartbeat opcode — servers send this periodically to prove they are alive.
const OP_HEARTBEAT: u64 = 0x0001;

/// Boot-log read request — any process can call SYS_CALL(init_pid, OP_LOG_READ, seq).
///
/// Request:  data[0] = OP_LOG_READ, data[1] = seq (entry to fetch, 0-based)
/// Reply:    data[0] = OP_LOG_READ_REPLY
///           data[1] = echoed seq
///           data[2] = next_seq (total events logged; seq in [next_seq-16, next_seq) are valid)
///           data[3] = timestamp_ns (0 when seq is out of range / no entry)
///           data[4..7] = 32 bytes of message text packed little-endian into 4 x u64
const OP_LOG_READ:       u64 = 0x0002;
const OP_LOG_READ_REPLY: u64 = 0x8002;

/// Shutdown request — any trusted process can ask init to start a clean shutdown.
const OP_SHUTDOWN: u64 = 0x00FF;

// Fault codes sent by the kernel (data[0] of fault notification messages).
// Values match x86 exception vectors — below 0x100, so distinct from opcodes.
const FAULT_DE: u64 = 0xDE; // #DE — divide by zero
const FAULT_GP: u64 = 0x0D; // #GP — general protection
const FAULT_PF: u64 = 0x0E; // #PF — page fault

// ── Service registry ──────────────────────────────────────────────────────────

/// Service name this process registers under.
const MY_NAME: &[u8] = b"init\0\0\0\0\0\0\0\0\0\0\0\0";

/// Maximum number of restart attempts per service before giving up.
const MAX_RESTARTS: u8 = 3;

/// Names and restart-policy flags for critical services.
/// `critical = true` → system halts if this service dies and cannot be restarted.
struct ServiceEntry {
    name:          &'static [u8],
    critical:      bool,
    pid:           u64, // u64::MAX = not yet resolved
    restart_count: u8,
}

impl ServiceEntry {
    const fn new(name: &'static [u8], critical: bool) -> Self {
        ServiceEntry { name, critical, pid: u64::MAX, restart_count: 0 }
    }
}

// ── Heartbeat tracking ────────────────────────────────────────────────────────

/// Deadline table: last heartbeat timestamp (ns) per service slot.
/// A slot is "missed" when `clock() - last_beat > HEARTBEAT_TIMEOUT_NS`.
/// Initialised to u64::MAX so no slot is considered missed before the first beat.
const MAX_SERVICES: usize = 8;
const HEARTBEAT_TIMEOUT_NS: u64 = 5_000_000_000; // 5 seconds

// ── Boot log ring buffer ──────────────────────────────────────────────────────

const LOG_ENTRIES: usize = 16;

#[derive(Copy, Clone)]
struct LogEntry {
    seq:  u64,
    ts:   u64,
    text: [u8; 32],
}

impl LogEntry {
    const fn zeroed() -> Self {
        LogEntry { seq: u64::MAX, ts: 0, text: [0u8; 32] }
    }
}

struct LogBuf {
    entries:  [LogEntry; LOG_ENTRIES],
    /// Next write position in the ring (0..LOG_ENTRIES).
    write:    usize,
    /// Number of valid entries currently stored (0..=LOG_ENTRIES).
    count:    usize,
    /// Sequence number to assign to the *next* entry.
    next_seq: u64,
}

impl LogBuf {
    const fn new() -> Self {
        LogBuf {
            entries: [LogEntry::zeroed(); LOG_ENTRIES],
            write:    0,
            count:    0,
            next_seq: 0,
        }
    }

    fn append(&mut self, ts: u64, text: &[u8]) {
        let e = &mut self.entries[self.write];
        e.seq = self.next_seq;
        e.ts  = ts;
        e.text = [0u8; 32];
        let n = text.len().min(31);
        e.text[..n].copy_from_slice(&text[..n]);
        self.write = (self.write + 1) % LOG_ENTRIES;
        if self.count < LOG_ENTRIES { self.count += 1; }
        self.next_seq += 1;
    }

    /// Return the entry whose seq matches `seq`, if it is still in the ring.
    fn get(&self, seq: u64) -> Option<&LogEntry> {
        // Determine index of the oldest entry in the ring.
        let oldest_idx = if self.count < LOG_ENTRIES {
            0
        } else {
            self.write // write points at the slot *about to be overwritten* = oldest
        };
        for i in 0..self.count {
            let idx = (oldest_idx + i) % LOG_ENTRIES;
            if self.entries[idx].seq == seq {
                return Some(&self.entries[idx]);
            }
        }
        None
    }
}

// ── Text-builder (no alloc / no format!) ─────────────────────────────────────

struct TextBuf {
    buf: [u8; 32],
    pos: usize,
}

impl TextBuf {
    fn new() -> Self { TextBuf { buf: [0u8; 32], pos: 0 } }

    fn push(&mut self, s: &[u8]) {
        for &b in s {
            if self.pos >= 31 { break; }
            self.buf[self.pos] = b;
            self.pos += 1;
        }
    }

    fn push_dec(&mut self, mut v: u64) {
        if v == 0 { self.push(b"0"); return; }
        let mut tmp = [0u8; 20];
        let mut i = 20usize;
        while v > 0 { i -= 1; tmp[i] = b'0' + (v % 10) as u8; v /= 10; }
        self.push(&tmp[i..]);
    }

    fn as_bytes(&self) -> &[u8] { &self.buf[..self.pos] }
}

// ── Serial output helpers ─────────────────────────────────────────────────────

fn print(s: &str) {
    for b in s.bytes() { syscall::uart_write(b); }
}

fn print_dec(mut v: u64) {
    if v == 0 { syscall::uart_write(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut i = 20usize;
    while v > 0 { i -= 1; buf[i] = b'0' + (v % 10) as u8; v /= 10; }
    for b in &buf[i..] { syscall::uart_write(*b); }
}

// ── Message handler ───────────────────────────────────────────────────────────

fn handle_msg(
    msg:       &mut syscall::Msg,
    services:  &mut [ServiceEntry],
    last_beat: &mut [u64; MAX_SERVICES],
    log:       &mut LogBuf,
) {
    let op  = msg.data[0];
    let arg = msg.data[1];

    if op == FAULT_DE || op == FAULT_GP || op == FAULT_PF {
        let fault_pid = arg;
        let fault_name = match op {
            FAULT_DE => "#DE(div-by-zero)",
            FAULT_GP => "#GP(general-protection)",
            FAULT_PF => "#PF(page-fault)",
            _        => "#??(unknown)",
        };
        print("[init] FAULT ");
        print(fault_name);
        print(" in PID ");
        print_dec(fault_pid);
        print("\n");

        // Append to boot log.
        {
            let ts = syscall::clock();
            let mut t = TextBuf::new();
            t.push(b"FAULT ");
            t.push(fault_name.as_bytes());
            t.push(b" pid=");
            t.push_dec(fault_pid);
            log.append(ts, t.as_bytes());
        }

        // Find the matching service and attempt restart.
        let mut matched_idx: Option<usize> = None;
        for (i, svc) in services.iter().enumerate() {
            if svc.pid == fault_pid {
                matched_idx = Some(i);
                break;
            }
        }

        if let Some(i) = matched_idx {
            let svc = &mut services[i];
            let name_str = svc.name;
            let critical  = svc.critical;

            if svc.restart_count < MAX_RESTARTS {
                svc.restart_count += 1;
                print("[init] attempting restart of ");
                for &b in name_str.iter().take_while(|&&c| c != 0) {
                    syscall::uart_write(b);
                }
                print(" (attempt ");
                print_dec(svc.restart_count as u64);
                print("/");
                print_dec(MAX_RESTARTS as u64);
                print(")\n");

                match syscall::restart_server(name_str) {
                    Some(new_pid) => {
                        svc.pid = new_pid as u64;
                        print("[init] restarted -> PID ");
                        print_dec(new_pid as u64);
                        print("\n");

                        let ts = syscall::clock();
                        let mut t = TextBuf::new();
                        t.push(b"RESTART OK pid=");
                        t.push_dec(new_pid as u64);
                        t.push(b" ");
                        t.push(&name_str[..name_str.iter().position(|&c| c == 0).unwrap_or(name_str.len())]);
                        log.append(ts, t.as_bytes());
                    }
                    None => {
                        print("[init] restart FAILED for ");
                        for &b in name_str.iter().take_while(|&&c| c != 0) {
                            syscall::uart_write(b);
                        }
                        print("\n");

                        let ts = syscall::clock();
                        let mut t = TextBuf::new();
                        t.push(b"RESTART FAIL ");
                        t.push(&name_str[..name_str.iter().position(|&c| c == 0).unwrap_or(name_str.len())]);
                        log.append(ts, t.as_bytes());

                        if critical {
                            print("[init] CRITICAL service unrecoverable — halting\n");
                            ordered_shutdown();
                        }
                    }
                }
            } else {
                // Restart budget exhausted.
                print("[init] restart budget exhausted for ");
                for &b in name_str.iter().take_while(|&&c| c != 0) {
                    syscall::uart_write(b);
                }
                print("\n");
                svc.pid = u64::MAX; // mark as gone so we stop reporting on it

                let ts = syscall::clock();
                let mut t = TextBuf::new();
                t.push(b"BUDGET EXHAUSTED ");
                t.push(&name_str[..name_str.iter().position(|&c| c == 0).unwrap_or(name_str.len())]);
                log.append(ts, t.as_bytes());

                if critical {
                    print("[init] CRITICAL service permanently down — initiating ordered shutdown\n");
                    ordered_shutdown();
                } else {
                    print("[init] non-critical service permanently down; continuing\n");
                }
            }
        }

    } else if op == OP_HEARTBEAT {
        let sender_pid = arg;
        for (i, svc) in services.iter().enumerate() {
            if svc.pid == sender_pid && i < MAX_SERVICES {
                last_beat[i] = syscall::clock();
                break;
            }
        }

    } else if op == OP_LOG_READ {
        // Diagnostic client asked for boot log entry at seq=arg.
        // Reply via send_msg so the SYS_CALL(17) caller is unblocked.
        let seq = arg;
        let mut reply = syscall::Msg::zeroed();
        reply.data[0] = OP_LOG_READ_REPLY;
        reply.data[1] = seq;
        reply.data[2] = log.next_seq; // one past the last valid seq

        if let Some(e) = log.get(seq) {
            reply.data[3] = e.ts;
            // Pack 32 bytes of text into data[4..8] (4 × 8 bytes, little-endian).
            for chunk in 0..4usize {
                let off = chunk * 8;
                let mut word = 0u64;
                for byte_idx in 0..8usize {
                    word |= (e.text[off + byte_idx] as u64) << (byte_idx * 8);
                }
                reply.data[4 + chunk] = word;
            }
        }
        // If seq is out of range: data[3]=0, data[4..8]=0 — caller checks data[2] (next_seq).
        syscall::send_msg(msg.sender as u64, &reply);

    } else if op == OP_SHUTDOWN {
        print("[init] shutdown requested by PID ");
        print_dec(msg.sender as u64);
        print("\n");
        ordered_shutdown();
    }
    // Unknown opcodes are silently ignored.
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Register so SYS_LOOKUP("init") resolves to us.
    syscall::register(MY_NAME);

    let my_pid = syscall::getpid();
    print("\n[init] PID 1 health monitor started (pid=");
    print_dec(my_pid as u64);
    print(")\n");

    // Boot log — ring buffer exposed via OP_LOG_READ IPC to diagnostic clients.
    let mut log = LogBuf::new();
    {
        let ts = syscall::clock();
        let mut t = TextBuf::new();
        t.push(b"init started pid=");
        t.push_dec(my_pid as u64);
        log.append(ts, t.as_bytes());
    }

    // Critical services we watch.  Populated by name lookup after boot.
    let mut services: [ServiceEntry; 3] = [
        ServiceEntry::new(b"uart-drv\0\0\0\0\0\0\0\0", true),
        ServiceEntry::new(b"rost-vfs\0\0\0\0\0\0\0\0", true),
        ServiceEntry::new(b"rost-shell\0\0\0\0\0\0",   false), // shell crash → warn, don't halt
    ];

    // Last heartbeat timestamps — u64::MAX means "no beat yet received".
    let mut last_beat: [u64; MAX_SERVICES] = [u64::MAX; MAX_SERVICES];

    // Give the other servers a few ticks to register before we start checking.
    let mut warmup_ticks: u32 = 30; // ~300 ms at 100 Hz

    let mut msg = syscall::Msg::zeroed();

    loop {
        // ── Resolve service PIDs lazily ───────────────────────────────────────
        for svc in services.iter_mut() {
            if svc.pid == u64::MAX {
                let pid = syscall::lookup(svc.name);
                if pid != u64::MAX {
                    svc.pid = pid;
                    print("[init] resolved ");
                    for &b in svc.name.iter().take_while(|&&c| c != 0) {
                        syscall::uart_write(b);
                    }
                    print(" -> PID ");
                    print_dec(pid);
                    print("\n");

                    let ts = syscall::clock();
                    let mut t = TextBuf::new();
                    t.push(b"resolved ");
                    t.push(&svc.name[..svc.name.iter().position(|&c| c == 0).unwrap_or(svc.name.len())]);
                    t.push(b" pid=");
                    t.push_dec(pid);
                    log.append(ts, t.as_bytes());
                }
            }
        }

        // ── Drain any already-queued IPC messages (non-blocking) ─────────────
        while syscall::recv_msg(0, &mut msg) {
            handle_msg(&mut msg, &mut services, &mut last_beat, &mut log);
        }

        // ── Block waiting for the next message (heartbeat watchdog timeout) ───
        //
        // CRITICAL: init must BLOCK here, not yield.  Yielding keeps init in
        // the Ready state at priority 32.  The scheduler always picks the
        // highest-priority Ready process, so yielding would starve uart-drv
        // (64) and shell (128) — they never get CPU time.
        //
        // Blocking with a timeout transitions init to Blocked.  While Blocked,
        // init is invisible to the scheduler and uart-drv / shell run freely.
        // After HEARTBEAT_CHECK_TICKS the deadline wakes init to check
        // for missed heartbeats.
        //
        // 50 ticks = 500 ms at 100 Hz — well inside the 5-second timeout.
        const HEARTBEAT_CHECK_TICKS: u64 = 50;
        if syscall::recv_msg(HEARTBEAT_CHECK_TICKS, &mut msg) {
            handle_msg(&mut msg, &mut services, &mut last_beat, &mut log);
        }

        // ── Heartbeat watchdog ────────────────────────────────────────────────
        if warmup_ticks == 0 {
            let now = syscall::clock();
            for (i, svc) in services.iter().enumerate() {
                if svc.pid == u64::MAX { continue; }
                if last_beat[i] == u64::MAX { continue; }
                if now.wrapping_sub(last_beat[i]) > HEARTBEAT_TIMEOUT_NS {
                    print("[init] WARN heartbeat timeout for PID ");
                    print_dec(svc.pid);
                    print(" (");
                    for &b in svc.name.iter().take_while(|&&c| c != 0) {
                        syscall::uart_write(b);
                    }
                    print(") missed for ");
                    print_dec(now.wrapping_sub(last_beat[i]) / 1_000_000_000);
                    print("s\n");
                    last_beat[i] = now; // reset to suppress repeat spam

                    let ts = syscall::clock();
                    let mut t = TextBuf::new();
                    t.push(b"HB TIMEOUT pid=");
                    t.push_dec(svc.pid);
                    t.push(b" ");
                    t.push(&svc.name[..svc.name.iter().position(|&c| c == 0).unwrap_or(svc.name.len())]);
                    log.append(ts, t.as_bytes());
                }
            }
        } else {
            warmup_ticks -= 1;
        }
    }
}

// ── Ordered shutdown ──────────────────────────────────────────────────────────

fn ordered_shutdown() -> ! {
    print("[init] ordered shutdown — system halting\n");
    // Future: send OP_SHUTDOWN to each registered service and wait for ACK.
    // For now: exit with code 1; the kernel will terminate us and the watchdog
    // will reset the system after the timeout fires.
    syscall::exit(1);
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    print("[init] PANIC — halting\n");
    syscall::exit(255);
}
