use crate::io as serial;
use crate::syscall::{Msg, recv_msg, send_msg};
use super::line_editor::LineEditor;
use super::history::History;

// ── VFS IPC constants ─────────────────────────────────────────────────────────

const VFS_PID:    u64 = 3;
const VFS_TIMEOUT: u64 = 100; // ticks (~1 s at 100 Hz)

// Opcodes  (must match servers/vfs/src/proto.rs)
const OP_READDIR: u64 = 0x20;
const OP_READ:    u64 = 0x22;
const OP_STAT:    u64 = 0x23;
const OP_MOUNT:   u64 = 0x24;

// Responses
const RESP_ENTRY: u64 = 0x80;
const RESP_DONE:  u64 = 0x81;
const RESP_DATA:  u64 = 0x82;
const RESP_STAT:  u64 = 0x83;
const RESP_MOUNT: u64 = 0x84;
const RESP_ERROR: u64 = 0x8F;

// ── Command registry ─────────────────────────────────────────────────────────

/// All built-in command names — must stay sorted (binary-search completion).
const COMMANDS: &[&[u8]] = &[
    b"cat",
    b"cd",
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
    b"mount",
    b"ps",
    b"pwd",
    b"uptime",
];

pub enum Action {
    Continue,
    Exit(u64),
    /// Shell should update its cwd to the given path (len bytes of buf).
    Cd([u8; 64], usize),
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

/// `cwd` is the shell's current working directory (e.g. b"/home/user").
pub fn dispatch(line: &[u8], history: &History, cwd: &[u8]) -> Action {
    let args = tokenize(line);
    let cmd  = args.get(0);
    if cmd.is_empty() { return Action::Continue; }

    match cmd {
        b"cat"     => cmd_cat(&args, cwd),
        b"cd"      => cmd_cd(&args, cwd),
        b"clear"   => { cmd_clear(); Action::Continue }
        b"echo"    => { cmd_echo(&args); Action::Continue }
        b"exec"    => { cmd_exec(&args); Action::Continue }
        b"exit"    => {
            let code = parse_u64(args.get(1));
            Action::Exit(code)
        }
        b"halt"    => {
            serial::print_str("System halting...\n");
            Action::Exit(0)
        }
        b"help"    => { cmd_help(); Action::Continue }
        b"history" => { cmd_history(history); Action::Continue }
        b"kill"    => { cmd_kill(&args); Action::Continue }
        b"ls"      => { cmd_ls(&args, cwd); Action::Continue }
        b"mem"     => { cmd_mem(); Action::Continue }
        b"mount"   => { cmd_mount(); Action::Continue }
        b"ps"      => { cmd_ps(); Action::Continue }
        b"pwd"     => { cmd_pwd(cwd); Action::Continue }
        b"uptime"  => { cmd_uptime(); Action::Continue }
        _ => {
            serial::print_str("rost: command not found: ");
            for &b in cmd { serial::put_byte(b); }
            serial::put_byte(b'\n');
            Action::Continue
        }
    }
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
    serial::print_str("  cat <path>         print file contents\n");
    serial::print_str("  cd [path]          change directory  (default: /)\n");
    serial::print_str("  clear              clear the screen\n");
    serial::print_str("  echo <args...>     print arguments\n");
    serial::print_str("  exec <path>        load and run an ELF binary  [needs ELF loader]\n");
    serial::print_str("  exit [code]        exit the shell\n");
    serial::print_str("  halt               halt the system\n");
    serial::print_str("  help               show this message\n");
    serial::print_str("  history            list command history\n");
    serial::print_str("  kill <pid>         send SIGTERM to a process\n");
    serial::print_str("  ls [path]          list directory  (default: cwd)\n");
    serial::print_str("  mem                show memory info  [needs kernel API]\n");
    serial::print_str("  mount              show filesystem mount table\n");
    serial::print_str("  ps                 list processes  [needs kernel API]\n");
    serial::print_str("  pwd                print working directory\n");
    serial::print_str("  uptime             show tick count  [needs kernel API]\n");
    serial::print_str("\nLine editing:\n");
    serial::print_str("  Left/Right  Home/End   move cursor\n");
    serial::print_str("  Up/Down                browse history\n");
    serial::print_str("  Tab                    complete command name\n");
    serial::print_str("  Backspace / Del        delete character\n");
    serial::print_str("  Ctrl+C                 cancel line\n");
    serial::print_str("  Ctrl+L                 clear screen\n");
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

// ── VFS commands ──────────────────────────────────────────────────────────────

/// `ls [path]` — list directory contents via VFS OP_READDIR.
fn cmd_ls(args: &Args<'_>, cwd: &[u8]) {
    let raw = args.get(1);
    let (target, _) = if raw.is_empty() {
        path_as_words(cwd)
    } else {
        let (buf, len) = resolve(cwd, raw);
        path_as_words(&buf[..len])
    };

    let mut req = Msg::zeroed();
    req.data[0] = OP_READDIR;
    req.data[2..8].copy_from_slice(&target);

    if !send_msg(VFS_PID, &req) {
        serial::print_str("ls: failed to reach VFS (PID 3)\n");
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
            RESP_DONE  => break,
            RESP_ENTRY => {
                found = true;
                let flags = resp.data[1];
                let size  = resp.data[2];
                let is_dir = flags & 1 != 0;
                let is_exe = flags & 2 != 0;
                // Colour: bold-blue for dirs, bold-green for executables, white for files.
                if is_dir       { serial::print_str("\x1b[1;34m"); }
                else if is_exe  { serial::print_str("\x1b[1;32m"); }
                print_packed_name(&resp.data[3..8]);
                if is_dir { serial::put_byte(b'/'); }
                serial::print_str("\x1b[0m\t");
                if is_dir { serial::print_str("dir"); }
                else      { print_u64(size); serial::print_str(" B"); }
                serial::put_byte(b'\n');
            }
            RESP_ERROR => {
                let errno = resp.data[1];
                serial::print_str("ls: ");
                match errno {
                    2 => serial::print_str("not a directory\n"),
                    1 => serial::print_str("no such file or directory\n"),
                    _ => serial::print_str("error\n"),
                }
                return;
            }
            _ => { serial::print_str("ls: unexpected VFS response\n"); return; }
        }
    }
    if !found { serial::print_str("(empty)\n"); }
}

/// `cat <path>` — stream file contents via VFS OP_READ.
fn cmd_cat(args: &Args<'_>, cwd: &[u8]) -> Action {
    let raw = args.get(1);
    if raw.is_empty() {
        serial::print_str("usage: cat <path>\n");
        return Action::Continue;
    }
    let (res_buf, res_len) = resolve(cwd, raw);
    let (pw, _)  = path_as_words(&res_buf[..res_len]);

    let mut offset: u64 = 0;
    let mut total:  u64 = u64::MAX;

    loop {
        let mut req = Msg::zeroed();
        req.data[0] = OP_READ;
        req.data[1] = offset;
        req.data[2..8].copy_from_slice(&pw);

        if !send_msg(VFS_PID, &req) {
            serial::print_str("cat: failed to reach VFS\n");
            return Action::Continue;
        }

        let mut resp = Msg::zeroed();
        if !recv_msg(VFS_TIMEOUT, &mut resp) {
            serial::print_str("cat: VFS timeout\n");
            return Action::Continue;
        }

        match resp.data[0] {
            RESP_DATA => {
                if total == u64::MAX { total = resp.data[1]; }
                let chunk = resp.data[2] as usize;
                if chunk == 0 { break; }
                print_packed_bytes(&resp.data[3..8], chunk);
                offset += chunk as u64;
                if offset >= total { break; }
            }
            RESP_DONE  => break,
            RESP_ERROR => {
                let errno = resp.data[1];
                serial::print_str("cat: ");
                for &b in raw { serial::put_byte(b); }
                match errno {
                    1 => serial::print_str(": no such file or directory\n"),
                    3 => serial::print_str(": is a directory\n"),
                    _ => serial::print_str(": error\n"),
                }
                return Action::Continue;
            }
            _ => { serial::print_str("cat: unexpected VFS response\n"); return Action::Continue; }
        }
    }
    Action::Continue
}

/// `cd [path]` — change current directory (no VFS round-trip; validated lazily).
fn cmd_cd(args: &Args<'_>, cwd: &[u8]) -> Action {
    let raw = args.get(1);
    let new = if raw.is_empty() {
        // cd with no args → go to root.
        let mut buf = [0u8; 64];
        buf[0] = b'/';
        (buf, 1usize)
    } else {
        resolve(cwd, raw)
    };
    Action::Cd(new.0, new.1)
}

/// `pwd` — print the current working directory.
fn cmd_pwd(cwd: &[u8]) {
    let len = cwd.iter().position(|&b| b == 0).unwrap_or(cwd.len());
    for &b in &cwd[..len] { serial::put_byte(b); }
    serial::put_byte(b'\n');
}

/// `mount` — show the filesystem mount table.
fn cmd_mount() {
    let mut req = Msg::zeroed();
    req.data[0] = OP_MOUNT;
    if !send_msg(VFS_PID, &req) {
        serial::print_str("mount: failed to reach VFS\n");
        return;
    }

    serial::print_str("MOUNT POINT              TYPE     SOURCE\n");
    serial::print_str("------------------------ -------- ------------------\n");

    loop {
        let mut resp = Msg::zeroed();
        if !recv_msg(VFS_TIMEOUT, &mut resp) {
            serial::print_str("mount: VFS timeout\n");
            return;
        }
        match resp.data[0] {
            RESP_DONE  => break,
            RESP_MOUNT => {
                // path in data[2..6] (32 bytes), fstype in data[6..8] (16 bytes)
                print_packed_name(&resp.data[2..6]);
                serial::print_str("\t ");
                print_packed_name(&resp.data[6..8]);
                serial::put_byte(b'\n');
            }
            _ => { serial::print_str("mount: unexpected VFS response\n"); return; }
        }
    }
}

/// `exec <path>` — load and run an ELF binary.
fn cmd_exec(args: &Args<'_>) {
    let path = args.get(1);
    if path.is_empty() {
        serial::print_str("usage: exec <path>\n");
        return;
    }
    serial::print_str("exec: ELF loader not yet implemented — cannot run ");
    for &b in path { serial::put_byte(b); }
    serial::put_byte(b'\n');
}

/// `kill <pid>` — send SIGTERM to a process.
fn cmd_kill(args: &Args<'_>) {
    let pid_bytes = args.get(1);
    if pid_bytes.is_empty() {
        serial::print_str("usage: kill <pid>\n");
        return;
    }
    let pid    = parse_u64(pid_bytes);
    let result = crate::syscall::notify(pid, 0x0000_0001);
    if result == 0 {
        serial::print_str("kill: signal sent to PID ");
        print_u64(pid);
        serial::put_byte(b'\n');
    } else {
        serial::print_str("kill: no such process\n");
    }
}

fn cmd_ps() {
    serial::print_str("ps: process list not yet available\n");
    serial::print_str("    own PID: ");
    print_u64(crate::syscall::getpid() as u64);
    serial::put_byte(b'\n');
}

fn cmd_mem() {
    serial::print_str("mem: memory info not yet available (needs kernel IPC)\n");
}

fn cmd_uptime() {
    serial::print_str("uptime: clock API not yet available (needs SYS_CLOCK_GETTIME)\n");
}

// ── Path helpers ──────────────────────────────────────────────────────────────

/// Resolve `input` relative to `cwd`.
/// Returns a (buf, len) pair where buf[..len] is the absolute path.
fn resolve(cwd: &[u8], input: &[u8]) -> ([u8; 64], usize) {
    let cwd_len = cwd.iter().position(|&b| b == 0).unwrap_or(cwd.len());
    let cwd     = &cwd[..cwd_len];

    // Absolute path: use as-is.
    if input.starts_with(b"/") {
        let mut buf = [0u8; 64];
        let len = input.len().min(63);
        buf[..len].copy_from_slice(&input[..len]);
        // Remove trailing slash unless it's the root itself.
        let len = if len > 1 && buf[len - 1] == b'/' { len - 1 } else { len };
        return (buf, len);
    }

    // Parent directory.
    if input == b".." {
        let mut buf = [0u8; 64];
        let new_len = parent_of(cwd);
        buf[..new_len].copy_from_slice(&cwd[..new_len]);
        return (buf, new_len);
    }

    // Current directory.
    if input == b"." {
        let mut buf = [0u8; 64];
        buf[..cwd_len].copy_from_slice(cwd);
        return (buf, cwd_len);
    }

    // Relative path: append to cwd.
    let mut buf = [0u8; 64];
    let mut len = cwd_len.min(63);
    buf[..len].copy_from_slice(&cwd[..len]);
    // Insert separator.
    if len > 0 && buf[len - 1] != b'/' && len < 63 {
        buf[len] = b'/';
        len += 1;
    }
    for &b in input.iter().take(63 - len) {
        buf[len] = b;
        len += 1;
    }
    (buf, len)
}

/// Return the length of the parent directory portion of `path`
/// (i.e., strip the last component).  Always returns at least 1 (the root `/`).
fn parent_of(path: &[u8]) -> usize {
    let stripped = if path.last() == Some(&b'/') && path.len() > 1 {
        &path[..path.len() - 1]
    } else {
        path
    };
    match stripped.iter().rposition(|&b| b == b'/') {
        Some(0) | None => 1,    // parent is root
        Some(pos)      => pos,
    }
}

/// Pack path bytes (up to 48 bytes = 6 words) as little-endian u64 words.
/// Returns (words[6], actual_len_capped).
fn path_as_words(path: &[u8]) -> ([u64; 6], usize) {
    let len = path.iter().position(|&b| b == 0).unwrap_or(path.len());
    let path = &path[..len];
    let mut words = [0u64; 6];
    for (i, &b) in path.iter().enumerate().take(48) {
        words[i / 8] |= (b as u64) << ((i % 8) * 8);
    }
    (words, len)
}

/// Print a null-terminated name from little-endian packed words.
fn print_packed_name(words: &[u64]) {
    'outer: for &w in words {
        for i in 0..8 {
            let b = (w >> (i * 8)) as u8;
            if b == 0 { break 'outer; }
            serial::put_byte(b);
        }
    }
}

/// Print `byte_count` bytes from little-endian packed words.
fn print_packed_bytes(words: &[u64], byte_count: usize) {
    let mut left = byte_count;
    'outer: for &w in words {
        for i in 0..8 {
            if left == 0 { break 'outer; }
            serial::put_byte((w >> (i * 8)) as u8);
            left -= 1;
        }
    }
}

// ── Number helpers ────────────────────────────────────────────────────────────

pub fn print_usize(mut n: usize) {
    if n == 0 { serial::put_byte(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut pos = 20usize;
    while n > 0 { pos -= 1; buf[pos] = b'0' + (n % 10) as u8; n /= 10; }
    for &b in &buf[pos..] { serial::put_byte(b); }
}

fn print_u64(mut n: u64) {
    if n == 0 { serial::put_byte(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut pos = 20usize;
    while n > 0 { pos -= 1; buf[pos] = b'0' + (n % 10) as u8; n /= 10; }
    for &b in &buf[pos..] { serial::put_byte(b); }
}

fn parse_u64(s: &[u8]) -> u64 {
    let mut n: u64 = 0;
    for &b in s {
        if b >= b'0' && b <= b'9' {
            n = n.saturating_mul(10).saturating_add((b - b'0') as u64);
        } else { break; }
    }
    n
}
