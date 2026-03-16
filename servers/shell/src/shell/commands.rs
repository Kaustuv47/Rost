use crate::io as serial;
use crate::syscall::{Msg, recv_msg, send_msg};
use super::line_editor::LineEditor;
use super::history::History;

// ── VFS IPC constants ─────────────────────────────────────────────────────────

/// Well-known PID for the VFS server (init=1, uart-drv=2, vfs=3).
const VFS_PID: u64 = 3;

// Opcodes sent to VFS
const OP_LIST: u64 = 0x20;
const OP_READ: u64 = 0x22;

// Responses from VFS
const RESP_ENTRY: u64 = 0x80; // one directory entry (ls)
const RESP_DONE:  u64 = 0x81; // end of list / end of read
const RESP_DATA:  u64 = 0x82; // file data chunk (cat)
const RESP_ERROR: u64 = 0x8F; // error (word1=1 means not found)

/// Ticks to wait for a VFS response before declaring timeout (100 Hz → ~1 s).
const VFS_TIMEOUT: u64 = 100;

// ── Command registry ─────────────────────────────────────────────────────────

/// All built-in command names, sorted for binary-search tab completion.
const COMMANDS: &[&[u8]] = &[
    b"cat",
    b"clear",
    b"echo",
    b"exec",
    b"exit",
    b"halt",
    b"help",
    b"history",
    b"kill",
    b"ls",
    b"mem",
    b"ps",
    b"uptime",
];

pub enum Action {
    Continue,
    Exit(u64),  // exit code
}

// ── Tokenizer ─────────────────────────────────────────────────────────────────

const MAX_ARGS: usize = 16;

struct Args<'a> {
    items: [&'a [u8]; MAX_ARGS],
    count: usize,
}

impl<'a> Args<'a> {
    fn get(&self, i: usize) -> &'a [u8] {
        if i < self.count { self.items[i] } else { b"" }
    }
}

/// Split `line` into whitespace-separated tokens.
/// Double-quoted tokens may contain spaces; the quotes are stripped.
fn tokenize(line: &[u8]) -> Args<'_> {
    let mut args = Args { items: [b""; MAX_ARGS], count: 0 };
    let mut i = 0;

    while i < line.len() && args.count < MAX_ARGS {
        while i < line.len() && line[i] == b' ' { i += 1; }
        if i >= line.len() { break; }

        if line[i] == b'"' {
            i += 1;
            let start = i;
            while i < line.len() && line[i] != b'"' { i += 1; }
            args.items[args.count] = &line[start..i];
            args.count += 1;
            if i < line.len() { i += 1; }
        } else {
            let start = i;
            while i < line.len() && line[i] != b' ' { i += 1; }
            args.items[args.count] = &line[start..i];
            args.count += 1;
        }
    }

    args
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

pub fn dispatch(line: &[u8], history: &History) -> Action {
    let args = tokenize(line);
    let cmd = args.get(0);
    if cmd.is_empty() { return Action::Continue; }

    match cmd {
        b"cat"     => cmd_cat(&args),
        b"clear"   => cmd_clear(),
        b"echo"    => cmd_echo(&args),
        b"exec"    => cmd_exec(&args),
        b"exit"    => {
            let code = parse_u64(args.get(1));
            return Action::Exit(code);
        }
        b"halt"    => {
            serial::print_str("System halting...\n");
            return Action::Exit(0);
        }
        b"help"    => cmd_help(),
        b"history" => cmd_history(history),
        b"kill"    => cmd_kill(&args),
        b"ls"      => cmd_ls(),
        b"mem"     => cmd_mem(),
        b"ps"      => cmd_ps(),
        b"uptime"  => cmd_uptime(),
        _ => {
            serial::print_str("rost: command not found: ");
            for &b in cmd { serial::put_byte(b); }
            serial::put_byte(b'\n');
        }
    }

    Action::Continue
}

// ── Tab completion ────────────────────────────────────────────────────────────

pub fn try_complete(editor: &mut LineEditor) -> usize {
    let bytes = &editor.as_bytes()[..editor.cursor];
    if bytes.contains(&b' ') { return 0; }

    let prefix = bytes;
    let mut matches: [&[u8]; 16] = [b""; 16];
    let mut count = 0;

    for &cmd in COMMANDS {
        if cmd.starts_with(prefix) && count < 16 {
            matches[count] = cmd;
            count += 1;
        }
    }

    match count {
        0 => { serial::put_byte(0x07); }
        1 => {
            for &b in &matches[0][prefix.len()..] { editor.insert(b); }
            editor.insert(b' ');
        }
        _ => {
            serial::put_byte(b'\n');
            for i in 0..count {
                for &b in matches[i] { serial::put_byte(b); }
                serial::print_str("  ");
            }
            serial::put_byte(b'\n');
        }
    }

    count
}

// ── Individual commands ───────────────────────────────────────────────────────

fn cmd_echo(args: &Args<'_>) {
    for i in 1..args.count {
        if i > 1 { serial::put_byte(b' '); }
        for &b in args.items[i] { serial::put_byte(b); }
    }
    serial::put_byte(b'\n');
}

fn cmd_help() {
    serial::print_str("Built-in commands:\n");
    serial::print_str("  cat <file>         print file contents from VFS\n");
    serial::print_str("  clear              clear the screen\n");
    serial::print_str("  echo <args...>     print arguments\n");
    serial::print_str("  exec <path>        load and run an ELF binary  [needs ELF loader]\n");
    serial::print_str("  exit [code]        exit the shell\n");
    serial::print_str("  halt               halt the system\n");
    serial::print_str("  help               show this message\n");
    serial::print_str("  history            list command history\n");
    serial::print_str("  kill <pid>         send SIGTERM notification to a process\n");
    serial::print_str("  ls                 list files in VFS\n");
    serial::print_str("  mem                show memory usage             [needs kernel API]\n");
    serial::print_str("  ps                 list running processes        [needs kernel API]\n");
    serial::print_str("  uptime             show system tick count        [needs kernel API]\n");
    serial::print_str("\nLine editing:\n");
    serial::print_str("  Left / Right       move cursor\n");
    serial::print_str("  Home / End         jump to start or end of line\n");
    serial::print_str("  Backspace / Del    delete character\n");
    serial::print_str("  Up / Down          browse history\n");
    serial::print_str("  Tab                complete command name\n");
    serial::print_str("  Ctrl+C             cancel current line\n");
    serial::print_str("  Ctrl+L             clear screen\n");
}

fn cmd_clear() {
    serial::print_str("\x1b[2J\x1b[H");
}

fn cmd_history(history: &History) {
    if history.is_empty() {
        serial::print_str("No history yet.\n");
        return;
    }
    let n = history.len();
    for age in (0..n).rev() {
        let num = n - age;
        print_usize(num);
        serial::print_str("  ");
        if let Some(line) = history.get(age) {
            for &b in line { serial::put_byte(b); }
        }
        serial::put_byte(b'\n');
    }
}

/// `ls` — list files in the VFS RAM disk.
fn cmd_ls() {
    let mut req = Msg::zeroed();
    req.data[0] = OP_LIST;
    if !send_msg(VFS_PID, &req) {
        serial::print_str("ls: failed to reach VFS\n");
        return;
    }

    let mut found = false;
    loop {
        let mut resp = Msg::zeroed();
        if !recv_msg(VFS_TIMEOUT, &mut resp) {
            serial::print_str("ls: VFS timeout\n");
            return;
        }
        match resp.data[0] {
            RESP_DONE => break,
            RESP_ENTRY => {
                found = true;
                let flags = resp.data[1];
                let size  = resp.data[2];
                // name packed into data[3..7] (40 bytes, null-terminated)
                print_packed_name(&resp.data[3..8]);
                if flags & 1 != 0 { serial::put_byte(b'*'); } // executable marker
                serial::print_str("\t");
                print_u64(size);
                serial::put_byte(b'\n');
            }
            _ => {
                serial::print_str("ls: unexpected response from VFS\n");
                return;
            }
        }
    }
    if !found {
        serial::print_str("(empty)\n");
    }
}

/// `cat <file>` — print the contents of a file from the VFS.
fn cmd_cat(args: &Args<'_>) {
    let path = args.get(1);
    if path.is_empty() {
        serial::print_str("usage: cat <file>\n");
        return;
    }

    let (nw0, nw1) = pack_name(path);
    let mut offset: u64 = 0;
    let mut total:  u64 = u64::MAX;

    loop {
        let mut req = Msg::zeroed();
        req.data[0] = OP_READ;
        req.data[1] = offset;
        req.data[2] = nw0;
        req.data[3] = nw1;
        if !send_msg(VFS_PID, &req) {
            serial::print_str("cat: failed to reach VFS\n");
            return;
        }

        let mut resp = Msg::zeroed();
        if !recv_msg(VFS_TIMEOUT, &mut resp) {
            serial::print_str("cat: VFS timeout\n");
            return;
        }

        match resp.data[0] {
            RESP_DATA => {
                if total == u64::MAX { total = resp.data[1]; }
                let chunk_len = resp.data[2] as usize;
                if chunk_len == 0 { break; }
                // data packed into data[3..7] (up to 40 bytes)
                print_packed_bytes(&resp.data[3..8], chunk_len);
                offset += chunk_len as u64;
                if offset >= total { break; }
            }
            RESP_DONE => break,
            RESP_ERROR => {
                serial::print_str("cat: ");
                for &b in path { serial::put_byte(b); }
                serial::print_str(": no such file\n");
                return;
            }
            _ => {
                serial::print_str("cat: unexpected response from VFS\n");
                return;
            }
        }
    }
}

/// `exec <path>` — load and run an ELF binary from the VFS.
/// Requires: VFS server + ELF loader.
fn cmd_exec(args: &Args<'_>) {
    let path = args.get(1);
    if path.is_empty() {
        serial::print_str("usage: exec <path>\n");
        return;
    }
    // TODO: SYS_SEND(VFS_PID, OP_EXEC, path_ptr, path_len)
    // The VFS server opens the file, the ELF loader maps it,
    // and the kernel creates a new process with the entry point.
    serial::print_str("exec: VFS not yet implemented — cannot load ");
    for &b in path { serial::put_byte(b); }
    serial::put_byte(b'\n');
}

/// `kill <pid>` — send a termination notification to a process.
fn cmd_kill(args: &Args<'_>) {
    let pid_bytes = args.get(1);
    if pid_bytes.is_empty() {
        serial::print_str("usage: kill <pid>\n");
        return;
    }
    let pid = parse_u64(pid_bytes);
    // SYS_NOTIFY(pid, SIGTERM_WORD) — receiver checks pending_notification
    // and calls SYS_EXIT if it sees the SIGTERM bit.
    let result = crate::syscall::notify(pid, 0x0000_0001); // bit 0 = SIGTERM
    if result == 0 {
        serial::print_str("kill: signal sent to PID ");
        print_u64(pid);
        serial::put_byte(b'\n');
    } else {
        serial::print_str("kill: no such process\n");
    }
}

/// `ps` — list processes.
/// TODO: needs a sys_ps syscall or a process info IPC endpoint.
fn cmd_ps() {
    serial::print_str("ps: process list not yet available\n");
    serial::print_str("    (requires kernel IPC endpoint for process info)\n");
    serial::print_str("    own PID: ");
    print_u64(crate::syscall::getpid() as u64);
    serial::put_byte(b'\n');
}

/// `mem` — show memory usage.
/// TODO: needs a sys_meminfo syscall or a memory info IPC endpoint.
fn cmd_mem() {
    serial::print_str("mem: memory info not yet available\n");
    serial::print_str("    (requires kernel IPC endpoint for allocator state)\n");
}

/// `uptime` — show TICK_COUNT.
/// TODO: needs sys_clock_gettime or an uptime IPC endpoint.
fn cmd_uptime() {
    serial::print_str("uptime: clock API not yet available\n");
    serial::print_str("       (requires sys_clock_gettime syscall)\n");
}

// ── VFS name packing helpers ──────────────────────────────────────────────────

/// Pack up to 16 bytes of a filename into two u64 words (little-endian bytes).
/// Matches `unpack_name` in the VFS server.
fn pack_name(name: &[u8]) -> (u64, u64) {
    let mut w0 = 0u64;
    let mut w1 = 0u64;
    for (i, &b) in name.iter().enumerate().take(8)  { w0 |= (b as u64) << (i * 8); }
    for (i, &b) in name.iter().skip(8).enumerate().take(8) { w1 |= (b as u64) << (i * 8); }
    (w0, w1)
}

/// Print a null-terminated name packed as little-endian bytes in `words`.
fn print_packed_name(words: &[u64]) {
    'outer: for &w in words {
        for i in 0..8 {
            let b = (w >> (i * 8)) as u8;
            if b == 0 { break 'outer; }
            serial::put_byte(b);
        }
    }
}

/// Print `byte_count` bytes packed little-endian in `words`.
fn print_packed_bytes(words: &[u64], byte_count: usize) {
    let mut remaining = byte_count;
    'outer: for &w in words {
        for i in 0..8 {
            if remaining == 0 { break 'outer; }
            serial::put_byte((w >> (i * 8)) as u8);
            remaining -= 1;
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn print_usize(mut n: usize) {
    if n == 0 { serial::put_byte(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut pos = 20usize;
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    for &b in &buf[pos..] { serial::put_byte(b); }
}

fn print_u64(mut n: u64) {
    if n == 0 { serial::put_byte(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut pos = 20usize;
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    for &b in &buf[pos..] { serial::put_byte(b); }
}

fn parse_u64(s: &[u8]) -> u64 {
    let mut n: u64 = 0;
    for &b in s {
        if b >= b'0' && b <= b'9' {
            n = n.saturating_mul(10).saturating_add((b - b'0') as u64);
        } else {
            break;
        }
    }
    n
}
