//! Command dispatcher and all built-in commands.
//!
//! The full zsh-like pipeline executed for every input line:
//!   1. History expansion  (!!, !n, !prefix)
//!   2. Variable expansion ($VAR, ${VAR}, $$, $?, ~)
//!   3. Compound command split  (;  &&  ||)
//!   4. For each segment: alias resolution → tokenize → dispatch

use crate::io as serial;
use crate::syscall::{Msg, recv_msg, send_msg};
use super::line_editor::{LineEditor, LINE_MAX};
use super::history::History;
use super::vars::VarStore;
use super::aliases::AliasStore;
use super::expand::{expand_history, expand_vars};

// ── exec/load ELF buffer ──────────────────────────────────────────────────────
//
// Static BSS buffer for loading ELF images via `exec`.  BSS is not stored in
// the ELF file; it is zero-initialised by the kernel ELF loader at process
// creation, so this does not bloat the shell binary on disk.
//
// 512 KB covers typical release-mode server binaries.  Debug binaries are
// larger; exec will report an error if the image does not fit.
const EXEC_BUF_CAP: usize = 512 * 1024;
static mut EXEC_BUF: [u8; EXEC_BUF_CAP] = [0u8; EXEC_BUF_CAP];

// ── VFS IPC constants ─────────────────────────────────────────────────────────

// PID assignment (must match kernel/src/main.rs spawn order):
//   PID 1 — rost-init
//   PID 2 — kernel idle process
//   PID 3 — rost-uart-drv
//   PID 4 — rost-vfs        ← VFS is here, not 3
//   PID 5 — rost-shell
const VFS_PID:    u64 = 4;
const VFS_TIMEOUT: u64 = 100;

const OP_READDIR:    u64 = 0x20;
const OP_READ:       u64 = 0x22;
const OP_STAT:       u64 = 0x23;
const OP_MOUNT:      u64 = 0x24;
const OP_WRITE_OPEN: u64 = 0x25;
const OP_WRITE_DATA: u64 = 0x26;
const OP_WRITE_CLOSE:u64 = 0x27;
const OP_MKDIR:      u64 = 0x28;
const OP_UNLINK:     u64 = 0x29;

const RESP_ENTRY: u64 = 0x80;
const RESP_DONE:  u64 = 0x81;
const RESP_DATA:  u64 = 0x82;
const RESP_STAT:  u64 = 0x83;
const RESP_MOUNT: u64 = 0x84;
const RESP_OK:    u64 = 0x85;
const RESP_ERROR: u64 = 0x8F;

const CHUNK_SIZE: usize = 40;

// ── Sorted command registry (binary-search tab-completion) ────────────────────

const COMMANDS: &[&[u8]] = &[
    b"alias",
    b"cat",
    b"cd",
    b"clear",
    b"date",
    b"echo",
    b"env",
    b"exec",
    b"exit",
    b"export",
    b"false",
    b"halt",
    b"help",
    b"history",
    b"kill",
    b"log",
    b"ls",
    b"mem",
    b"mkdir",
    b"mount",
    b"ps",
    b"pwd",
    b"rm",
    b"set",
    b"sleep",
    b"source",
    b"touch",
    b"true",
    b"type",
    b"unalias",
    b"unset",
    b"uptime",
    b"which",
    b"write",
];

// ── Public action type ────────────────────────────────────────────────────────

pub enum Action {
    Continue,
    Exit(u64),
    /// Shell should update its cwd.
    Cd([u8; 64], usize),
}

// ── Execution context ─────────────────────────────────────────────────────────

/// Mutable shell state passed through the dispatch pipeline.
pub struct ExecCtx<'a> {
    pub history:   &'a History,
    pub vars:      &'a mut VarStore,
    pub aliases:   &'a mut AliasStore,
    pub cwd:       &'a [u8],
    pub pid:       u32,
    pub last_exit: u64,
}

// ── Top-level entry point ─────────────────────────────────────────────────────

/// Full pipeline: history expand → variable expand → compound split → dispatch.
///
/// Returns `(Action, last_exit_code)`.
pub fn execute_line(raw: &[u8], ctx: &mut ExecCtx<'_>) -> (Action, u64) {
    // 1. History expansion
    let mut hist_buf = [0u8; LINE_MAX];
    let line = {
        let n = expand_history(raw, ctx.history, &mut hist_buf);
        if n > 0 {
            // Echo expanded line (zsh behaviour)
            for &b in &hist_buf[..n] { serial::put_byte(b); }
            serial::put_byte(b'\n');
            &hist_buf[..n]
        } else {
            raw
        }
    };

    // 2. Variable expansion
    let mut var_buf = [0u8; LINE_MAX];
    let vlen = expand_vars(line, ctx.vars, ctx.pid, ctx.last_exit, &mut var_buf);
    let line = &var_buf[..vlen];

    // 3. Compound command split and execute
    let mut parts = [CompoundPart { cmd: &[], joiner: Joiner::Seq }; 8];
    let n = split_compound(line, &mut parts);

    let mut exit_code = ctx.last_exit;
    let mut final_action = Action::Continue;
    let mut skip = false;
    let mut skip_reason = Joiner::Seq;

    for i in 0..n {
        let part = &parts[i];

        if skip {
            match skip_reason {
                Joiner::And if exit_code != 0 => {}       // && with failure: keep skipping
                Joiner::Or  if exit_code == 0 => {}       // || with success: keep skipping
                _ => { skip = false; }
            }
        }

        if !skip {
            let (action, code) = dispatch_single(trim(part.cmd), ctx);
            exit_code = code;
            ctx.last_exit = exit_code;

            match action {
                Action::Continue => {}
                Action::Cd(p, l) => { final_action = Action::Cd(p, l); }
                Action::Exit(c)  => { return (Action::Exit(c), exit_code); }
            }

            // Set up skip for next part based on this joiner
            match part.joiner {
                Joiner::And if exit_code != 0 => { skip = true; skip_reason = Joiner::And; }
                Joiner::Or  if exit_code == 0 => { skip = true; skip_reason = Joiner::Or; }
                _ => {}
            }
        }
    }

    (final_action, exit_code)
}

// ── Compound command parser ───────────────────────────────────────────────────

#[derive(Copy, Clone)]
enum Joiner { Seq, And, Or }

#[derive(Copy, Clone)]
struct CompoundPart<'a> {
    cmd:    &'a [u8],
    joiner: Joiner,
}

/// Split `line` by unquoted `;`, `&&`, `||`.  Returns number of parts.
fn split_compound<'a>(line: &'a [u8], out: &mut [CompoundPart<'a>; 8]) -> usize {
    let mut count = 0usize;
    let mut start = 0usize;
    let mut i = 0usize;
    let mut in_q = false;
    let mut qch = 0u8;

    while i < line.len() {
        let b = line[i];
        if in_q {
            if b == qch { in_q = false; }
            i += 1;
            continue;
        }
        if b == b'"' || b == b'\'' { in_q = true; qch = b; i += 1; continue; }

        if b == b';' {
            if count < 7 {
                out[count] = CompoundPart { cmd: &line[start..i], joiner: Joiner::Seq };
                count += 1;
            }
            start = i + 1;
            i += 1;
        } else if b == b'&' && i + 1 < line.len() && line[i + 1] == b'&' {
            if count < 7 {
                out[count] = CompoundPart { cmd: &line[start..i], joiner: Joiner::And };
                count += 1;
            }
            start = i + 2;
            i += 2;
        } else if b == b'|' && i + 1 < line.len() && line[i + 1] == b'|' {
            if count < 7 {
                out[count] = CompoundPart { cmd: &line[start..i], joiner: Joiner::Or };
                count += 1;
            }
            start = i + 2;
            i += 2;
        } else {
            i += 1;
        }
    }

    if start <= line.len() {
        let last = trim(&line[start..]);
        if !last.is_empty() && count < 8 {
            out[count] = CompoundPart { cmd: last, joiner: Joiner::Seq };
            count += 1;
        }
    }
    count
}

// ── Single-segment dispatcher ─────────────────────────────────────────────────

/// Dispatch one command segment after alias resolution and tokenization.
fn dispatch_single(line: &[u8], ctx: &mut ExecCtx<'_>) -> (Action, u64) {
    if line.is_empty() { return (Action::Continue, 0); }

    // Check for VAR=value bare assignment (no spaces before =)
    if let Some(eq) = find_assignment(line) {
        let name = &line[..eq];
        let val  = &line[eq + 1..];
        ctx.vars.set(name, val);
        return (Action::Continue, 0);
    }

    // Alias expansion (single level — no recursive alias)
    let args = tokenize(line);
    let cmd = args.get(0);
    if cmd.is_empty() { return (Action::Continue, 0); }

    // Build alias-expanded line if an alias matches
    let mut alias_buf = [0u8; LINE_MAX];
    let effective_line = if let Some(repl) = ctx.aliases.get(cmd) {
        // Substitute first token with alias replacement, keep rest of args
        let rlen = repl.len().min(LINE_MAX - 1);
        alias_buf[..rlen].copy_from_slice(&repl[..rlen]);
        // Append remaining original args
        let orig_after_cmd = skip_first_token(line);
        if !orig_after_cmd.is_empty() {
            let sep_pos = rlen;
            if sep_pos + 1 + orig_after_cmd.len() < LINE_MAX {
                alias_buf[sep_pos] = b' ';
                let olen = orig_after_cmd.len();
                alias_buf[sep_pos + 1..sep_pos + 1 + olen]
                    .copy_from_slice(orig_after_cmd);
                &alias_buf[..sep_pos + 1 + olen]
            } else {
                &alias_buf[..rlen]
            }
        } else {
            &alias_buf[..rlen]
        }
    } else {
        line
    };

    dispatch(effective_line, ctx)
}

/// Skip the first whitespace-delimited token in `s`.
fn skip_first_token(s: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < s.len() && s[i] != b' ' { i += 1; }
    while i < s.len() && s[i] == b' ' { i += 1; }
    &s[i..]
}

/// Detect `NAME=value` bare assignment: returns index of `=` if this is
/// a valid identifier-only name with no leading space.
fn find_assignment(line: &[u8]) -> Option<usize> {
    for (i, &b) in line.iter().enumerate() {
        if b == b'=' && i > 0 { return Some(i); }
        if b == b' ' { return None; }
        if !matches!(b, b'a'..=b'z'|b'A'..=b'Z'|b'0'..=b'9'|b'_') { return None; }
    }
    None
}

// ── Tokenizer ─────────────────────────────────────────────────────────────────

const MAX_ARGS: usize = 16;

#[derive(Copy, Clone)]
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
        } else if line[i] == b'\'' {
            i += 1;
            let start = i;
            while i < line.len() && line[i] != b'\'' { i += 1; }
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

// ── Core dispatch ─────────────────────────────────────────────────────────────

fn dispatch(line: &[u8], ctx: &mut ExecCtx<'_>) -> (Action, u64) {
    let args = tokenize(line);
    let cmd  = args.get(0);
    if cmd.is_empty() { return (Action::Continue, 0); }

    let ret = match cmd {
        b"alias"   => { cmd_alias(&args, ctx.aliases); 0 }
        b"cat"     => { cmd_cat(&args, ctx.cwd); 0 }
        b"cd"      => {
            let (a, c) = cmd_cd(&args, ctx.cwd);
            return (a, c);
        }
        b"clear"   => { cmd_clear(); 0 }
        b"date"    => { cmd_date(); 0 }
        b"echo"    => { cmd_echo(&args); 0 }
        b"env"     => { cmd_env(ctx.vars); 0 }
        b"exec"    => { cmd_exec(&args, ctx.cwd); 0 }
        b"exit"    => {
            return (Action::Exit(parse_u64(args.get(1))), 0);
        }
        b"export"  => { cmd_export(&args, ctx.vars); 0 }
        b"false"   => 1,
        b"halt"    => {
            serial::print_str("System halting...\n");
            return (Action::Exit(0), 0);
        }
        b"help"    => { cmd_help(); 0 }
        b"history" => { cmd_history(ctx.history); 0 }
        b"kill"    => { cmd_kill(&args) }
        b"log"     => { cmd_log(); 0 }
        b"ls"      => { cmd_ls(&args, ctx.cwd); 0 }
        b"mem"     => { cmd_mem(); 0 }
        b"mkdir"   => { cmd_mkdir(&args, ctx.cwd) }
        b"mount"   => { cmd_mount(); 0 }
        b"ps"      => { cmd_ps(ctx.pid); 0 }
        b"pwd"     => { cmd_pwd(ctx.cwd); 0 }
        b"rm"      => { cmd_rm(&args, ctx.cwd) }
        b"set"     => { cmd_set(ctx.vars); 0 }
        b"sleep"   => { cmd_sleep(&args); 0 }
        b"source" | b"." => {
            let (a, c) = cmd_source(&args, ctx);
            return (a, c);
        }
        b"touch"   => { cmd_touch(&args, ctx.cwd) }
        b"true"    => 0,
        b"type"    => { cmd_type(&args, ctx.aliases); 0 }
        b"unalias" => { cmd_unalias(&args, ctx.aliases); 0 }
        b"unset"   => { cmd_unset(&args, ctx.vars); 0 }
        b"uptime"  => { cmd_uptime(); 0 }
        b"which"   => { cmd_which(&args, ctx.aliases); 0 }
        b"write"   => { cmd_write(&args, ctx.cwd) }
        b"["       => { cmd_test_bracket(&args, ctx.cwd) }
        b"test"    => { cmd_test(&args, ctx.cwd) }
        _ => {
            serial::print_str("\x1b[1;31mrost\x1b[0m: command not found: ");
            for &b in cmd { serial::put_byte(b); }
            serial::put_byte(b'\n');
            127
        }
    };

    (Action::Continue, ret)
}

// ── Tab completion ────────────────────────────────────────────────────────────

/// Tab-complete the current editor contents.
///
/// Returns the number of completions found (0 = no match, 1 = unique, >1 = list).
pub fn try_complete(editor: &mut LineEditor, cwd: &[u8]) -> usize {
    // Copy bytes-before-cursor to avoid holding an immutable borrow when we
    // later mutably borrow editor for insertion.
    let cursor = editor.cursor;
    let mut tmp = [0u8; LINE_MAX];
    tmp[..cursor].copy_from_slice(&editor.as_bytes()[..cursor]);
    let bytes = &tmp[..cursor];

    if !bytes.contains(&b' ') {
        // Command completion (also handles path-like commands starting with / or .)
        if bytes.starts_with(b"/") || bytes.starts_with(b"./") {
            let mut pb = [0u8; LINE_MAX];
            pb[..cursor].copy_from_slice(bytes);
            return path_complete(editor, &pb[..cursor], cwd);
        }
        let mut pb = [0u8; LINE_MAX];
        pb[..cursor].copy_from_slice(bytes);
        complete_command(editor, &pb[..cursor])
    } else {
        // Argument completion — find the word under / before the cursor
        let last_sp = bytes.iter().rposition(|&b| b == b' ').unwrap_or(0);
        let partial_start = last_sp + 1;
        let partial_len = cursor - partial_start;
        let mut pb = [0u8; LINE_MAX];
        pb[..partial_len].copy_from_slice(&bytes[partial_start..]);
        path_complete(editor, &pb[..partial_len], cwd)
    }
}

fn complete_command(editor: &mut LineEditor, prefix: &[u8]) -> usize {
    let mut matches: [&[u8]; 32] = [b""; 32];
    let mut count = 0usize;

    for &cmd in COMMANDS {
        if cmd.starts_with(prefix) && count < 32 {
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

/// Complete `partial` as a filesystem path.  Returns number of matches.
fn path_complete(editor: &mut LineEditor, partial: &[u8], cwd: &[u8]) -> usize {
    // Split partial into (dir, name_prefix).
    let (dir_buf, dir_len, name_prefix) = if let Some(slash) = partial.iter().rposition(|&b| b == b'/') {
        let (dp, nl) = if slash == 0 {
            let mut b = [0u8; 64];
            b[0] = b'/';
            (b, 1usize)
        } else {
            let (b, l) = make_path(&partial[..slash]);
            (b, l)
        };
        (dp, nl, &partial[slash + 1..])
    } else {
        let cwd_len = cwd.iter().position(|&b| b == 0).unwrap_or(cwd.len());
        let mut dp = [0u8; 64];
        dp[..cwd_len].copy_from_slice(&cwd[..cwd_len]);
        (dp, cwd_len, partial)
    };

    let (pw, _) = path_as_words(&dir_buf[..dir_len]);
    let mut req = Msg::zeroed();
    req.data[0] = OP_READDIR;
    req.data[2..8].copy_from_slice(&pw);

    if !send_msg(VFS_PID, &req) { return 0; }

    let mut names: [[u8; 48]; 16] = [[0u8; 48]; 16];
    let mut count = 0usize;

    loop {
        let mut resp = Msg::zeroed();
        if !recv_msg(VFS_TIMEOUT, &mut resp) { break; }
        match resp.data[0] {
            RESP_DONE => break,
            RESP_ENTRY => {
                if count < 16 {
                    let mut name = [0u8; 48];
                    let nlen = unpack_name(&resp.data[3..8], &mut name);
                    if name[..name_prefix.len().min(nlen)]
                        == name_prefix[..name_prefix.len().min(nlen)]
                        && nlen >= name_prefix.len()
                    {
                        names[count] = name;
                        count += 1;
                    }
                }
            }
            _ => break,
        }
    }

    match count {
        0 => { serial::put_byte(0x07); }
        1 => {
            let nlen = names[0].iter().position(|&b| b == 0).unwrap_or(48);
            for &b in &names[0][name_prefix.len()..nlen] { editor.insert(b); }
        }
        _ => {
            serial::put_byte(b'\n');
            for i in 0..count {
                let nlen = names[i].iter().position(|&b| b == 0).unwrap_or(48);
                for &b in &names[i][..nlen] { serial::put_byte(b); }
                serial::print_str("  ");
            }
            serial::put_byte(b'\n');
        }
    }
    count
}

// ── Built-in commands ─────────────────────────────────────────────────────────

fn cmd_echo(args: &Args<'_>) {
    let mut newline = true;
    let start = if args.get(1) == b"-n" { newline = false; 2 } else { 1 };
    for i in start..args.count {
        if i > start { serial::put_byte(b' '); }
        for &b in args.items[i] { serial::put_byte(b); }
    }
    if newline { serial::put_newline(); }
}

fn cmd_help() {
    serial::print_str("\x1b[1;33mRost Shell\x1b[0m — zsh-compatible interactive shell\n\n");
    serial::print_str("\x1b[1mBuilt-in commands:\x1b[0m\n");
    serial::print_str("  alias [name=val]   define or list aliases\n");
    serial::print_str("  cat <path>         print file contents\n");
    serial::print_str("  cd [path]          change directory  (default: $HOME)\n");
    serial::print_str("  clear              clear the screen\n");
    serial::print_str("  date               show current time\n");
    serial::print_str("  echo [-n] <args>   print arguments\n");
    serial::print_str("  env                list all environment variables\n");
    serial::print_str("  exec <path> [pri]  load ELF from VFS and spawn (priority 0=default)\n");
    serial::print_str("  exit [code]        exit the shell\n");
    serial::print_str("  export VAR=val     set environment variable\n");
    serial::print_str("  false              return exit code 1\n");
    serial::print_str("  halt               halt the system\n");
    serial::print_str("  help               show this message\n");
    serial::print_str("  history            list command history\n");
    serial::print_str("  kill <pid>         send SIGTERM to a process\n");
    serial::print_str("  log                show crash log info\n");
    serial::print_str("  ls [path]          list directory\n");
    serial::print_str("  mem                show memory info\n");
    serial::print_str("  mkdir <path>       create a directory\n");
    serial::print_str("  mount              show mount table\n");
    serial::print_str("  ps                 list processes\n");
    serial::print_str("  pwd                print working directory\n");
    serial::print_str("  rm <path>          remove a mutable file or directory\n");
    serial::print_str("  set                list all shell variables\n");
    serial::print_str("  sleep <n>          sleep n seconds\n");
    serial::print_str("  source <file>      execute script from VFS\n");
    serial::print_str("  touch <path>       create empty file\n");
    serial::print_str("  test EXPR          evaluate expression (see below)\n");
    serial::print_str("  true               return exit code 0\n");
    serial::print_str("  type <cmd>         show type of command\n");
    serial::print_str("  unalias <name>     remove alias\n");
    serial::print_str("  unset <name>       remove variable\n");
    serial::print_str("  uptime             show uptime\n");
    serial::print_str("  which <cmd>        show command location\n");
    serial::print_str("  write <path> <text>  write text to a file\n");
    serial::print_str("\n\x1b[1mLine editing (emacs mode):\x1b[0m\n");
    serial::print_str("  Ctrl+A/E           beginning / end of line\n");
    serial::print_str("  Ctrl+B/F           backward / forward char\n");
    serial::print_str("  Alt+B / Alt+F      backward / forward word\n");
    serial::print_str("  Ctrl+K / Ctrl+U    kill to end / start of line\n");
    serial::print_str("  Ctrl+W             kill previous word\n");
    serial::print_str("  Ctrl+Y             yank (paste) killed text\n");
    serial::print_str("  Ctrl+R             reverse incremental search\n");
    serial::print_str("  Ctrl+P / Ctrl+N    previous / next history\n");
    serial::print_str("  Tab                complete command or path\n");
    serial::print_str("  Ctrl+C / Ctrl+L    cancel line / clear screen\n");
    serial::print_str("\n\x1b[1mHistory expansion:\x1b[0m\n");
    serial::print_str("  !!        last command\n");
    serial::print_str("  !n        nth history entry\n");
    serial::print_str("  !-n       nth from end\n");
    serial::print_str("  !prefix   last command starting with prefix\n");
    serial::print_str("\n\x1b[1mCompound commands:\x1b[0m\n");
    serial::print_str("  cmd1 ; cmd2        run sequentially\n");
    serial::print_str("  cmd1 && cmd2       run cmd2 only if cmd1 succeeds\n");
    serial::print_str("  cmd1 || cmd2       run cmd2 only if cmd1 fails\n");
    serial::print_str("\n\x1b[1mtest / [ expressions:\x1b[0m\n");
    serial::print_str("  -f FILE   file exists      -d FILE  is directory\n");
    serial::print_str("  -z STR    string empty     -n STR   string non-empty\n");
    serial::print_str("  A = B     string equal     A != B   string not equal\n");
    serial::print_str("  A -eq B   numeric equal    -ne -lt -le -gt -ge\n");
}

fn cmd_clear() {
    serial::print_str("\x1b[2J\x1b[H");
}

fn cmd_date() {
    let ns    = crate::syscall::clock();
    let secs  = ns / 1_000_000_000;
    let ms    = (ns % 1_000_000_000) / 1_000_000;
    let hours = (secs % 86400) / 3600;
    let mins  = (secs % 3600) / 60;
    let s     = secs % 60;
    serial::print_str("uptime ");
    print_u64(secs / 86400); serial::print_str("d ");
    print_u64(hours);        serial::print_str("h ");
    print_u64(mins);         serial::print_str("m ");
    print_u64(s);            serial::print_str("s ");
    print_u64(ms);           serial::print_str("ms\n");
}

fn cmd_uptime() {
    let ns    = crate::syscall::clock();
    let secs  = ns / 1_000_000_000;
    let ms    = (ns % 1_000_000_000) / 1_000_000;
    let hours = secs / 3600;
    let mins  = (secs % 3600) / 60;
    let s     = secs % 60;
    serial::print_str("up ");
    print_u64(hours); serial::print_str("h ");
    print_u64(mins);  serial::print_str("m ");
    print_u64(s);     serial::print_str("s (");
    print_u64(ms);    serial::print_str(" ms)\n");
}

fn cmd_history(history: &History) {
    if history.is_empty() {
        serial::print_str("No history.\n");
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

fn cmd_env(vars: &VarStore) {
    for i in 0..vars.count() {
        if let Some((name, val)) = vars.entry(i) {
            for &b in name { serial::put_byte(b); }
            serial::put_byte(b'=');
            for &b in val  { serial::put_byte(b); }
            serial::put_byte(b'\n');
        }
    }
}

fn cmd_set(vars: &VarStore) {
    cmd_env(vars);
}

fn cmd_export(args: &Args<'_>, vars: &mut VarStore) {
    let arg = args.get(1);
    if arg.is_empty() {
        cmd_env(vars);
        return;
    }
    // Support both `export VAR=val` and `export VAR` (mark existing as exported)
    if let Some(eq) = arg.iter().position(|&b| b == b'=') {
        vars.set(&arg[..eq], &arg[eq + 1..]);
    } else {
        // export without value — no-op (already set or just acknowledge)
        if vars.get(arg).is_none() {
            vars.set(arg, b"");
        }
    }
}

fn cmd_unset(args: &Args<'_>, vars: &mut VarStore) {
    let name = args.get(1);
    if name.is_empty() {
        serial::print_str("usage: unset <name>\n");
        return;
    }
    vars.unset(name);
}

fn cmd_alias(args: &Args<'_>, aliases: &mut AliasStore) {
    if args.count <= 1 {
        // List all aliases
        for i in 0..aliases.count() {
            if let Some((name, val)) = aliases.entry(i) {
                serial::print_str("alias ");
                for &b in name { serial::put_byte(b); }
                serial::put_byte(b'=');
                serial::put_byte(b'\'');
                for &b in val  { serial::put_byte(b); }
                serial::put_byte(b'\'');
                serial::put_byte(b'\n');
            }
        }
        return;
    }
    let arg = args.get(1);
    if let Some(eq) = arg.iter().position(|&b| b == b'=') {
        let name = &arg[..eq];
        // Strip surrounding quotes from value
        let raw_val = &arg[eq + 1..];
        let val = if raw_val.starts_with(b"'") && raw_val.ends_with(b"'") && raw_val.len() > 1 {
            &raw_val[1..raw_val.len() - 1]
        } else if raw_val.starts_with(b"\"") && raw_val.ends_with(b"\"") && raw_val.len() > 1 {
            &raw_val[1..raw_val.len() - 1]
        } else {
            raw_val
        };
        if !aliases.set(name, val) {
            serial::print_str("alias: table full\n");
        }
    } else {
        // Print single alias
        if let Some(val) = aliases.get(arg) {
            serial::print_str("alias ");
            for &b in arg { serial::put_byte(b); }
            serial::print_str("='");
            for &b in val { serial::put_byte(b); }
            serial::print_str("'\n");
        } else {
            serial::print_str("alias: ");
            for &b in arg { serial::put_byte(b); }
            serial::print_str(": not found\n");
        }
    }
}

fn cmd_unalias(args: &Args<'_>, aliases: &mut AliasStore) {
    let name = args.get(1);
    if name.is_empty() {
        serial::print_str("usage: unalias <name>\n");
        return;
    }
    aliases.remove(name);
}

fn cmd_which(args: &Args<'_>, aliases: &AliasStore) {
    let name = args.get(1);
    if name.is_empty() {
        serial::print_str("usage: which <command>\n");
        return;
    }
    if aliases.get(name).is_some() {
        serial::print_str("alias ");
        for &b in name { serial::put_byte(b); }
        serial::put_byte(b'\n');
        return;
    }
    for &cmd in COMMANDS {
        if cmd == name {
            serial::print_str("builtin ");
            for &b in name { serial::put_byte(b); }
            serial::put_byte(b'\n');
            return;
        }
    }
    // Try VFS /bin/<name>
    let mut path = [0u8; 64];
    path[0] = b'/'; path[1] = b'b'; path[2] = b'i'; path[3] = b'n'; path[4] = b'/';
    let nlen = name.len().min(58);
    path[5..5 + nlen].copy_from_slice(&name[..nlen]);
    let (pw, _) = path_as_words(&path[..5 + nlen]);
    let mut req = Msg::zeroed();
    req.data[0] = OP_STAT;
    req.data[2..8].copy_from_slice(&pw);
    if send_msg(VFS_PID, &req) {
        let mut resp = Msg::zeroed();
        if recv_msg(VFS_TIMEOUT, &mut resp) && resp.data[0] == RESP_STAT {
            let flags = resp.data[1];
            if flags & 2 != 0 {
                for &b in &path[..5 + nlen] { serial::put_byte(b); }
                serial::put_byte(b'\n');
                return;
            }
        }
    }
    for &b in name { serial::put_byte(b); }
    serial::print_str(": not found\n");
}

fn cmd_type(args: &Args<'_>, aliases: &AliasStore) {
    let name = args.get(1);
    if name.is_empty() {
        serial::print_str("usage: type <command>\n");
        return;
    }
    if aliases.get(name).is_some() {
        for &b in name { serial::put_byte(b); }
        serial::print_str(" is an alias\n");
        return;
    }
    for &cmd in COMMANDS {
        if cmd == name {
            for &b in name { serial::put_byte(b); }
            serial::print_str(" is a shell builtin\n");
            return;
        }
    }
    for &b in name { serial::put_byte(b); }
    serial::print_str(": not found\n");
}

fn cmd_sleep(args: &Args<'_>) {
    let secs = parse_u64(args.get(1));
    if secs == 0 { return; }
    // 100 Hz timer: 100 ticks per second
    let ticks = secs.saturating_mul(100);
    // SYS_RECV with timeout = ticks (will expire without receiving anything)
    crate::syscall::recv(ticks);
}

fn cmd_log() {
    serial::print_str("Crash log is stored at physical address 0x4000.\n");
    serial::print_str("Ring buffer: 16 × 64-byte ErrorRecord entries.\n");
    serial::print_str("Fields: magic, vector, tick, pid, rip, rflags, cr2.\n");
    serial::print_str("Access requires a privileged IPC call to the kernel\n");
    serial::print_str("(SYS_LOG not yet exposed to ring-3).\n");
}

fn cmd_ps(my_pid: u32) {
    // Each entry: pid(4) + state(1) + priority(1) + pad(2) + name(16) = 24 bytes.
    let mut buf = [0u8; 24 * 32];
    let count = crate::syscall::list_procs(&mut buf);

    if count == 0 {
        serial::print_str("ps: no process info available\n");
        return;
    }

    serial::print_str("  PID  ST  PRI  NAME\n");
    serial::print_str("-----  --  ---  ----------------\n");

    for i in 0..count {
        let base = i * 24;
        let pid = u32::from_le_bytes([buf[base], buf[base+1], buf[base+2], buf[base+3]]);
        let state    = buf[base + 4];
        let priority = buf[base + 5];
        let name     = &buf[base + 8..base + 24];

        // PID column (right-aligned, 5 chars)
        let pid_u64 = pid as u64;
        let digits = if pid_u64 >= 10000 { 5 }
                     else if pid_u64 >= 1000 { 4 }
                     else if pid_u64 >= 100  { 3 }
                     else if pid_u64 >= 10   { 2 }
                     else                    { 1 };
        for _ in 0..(5 - digits) { serial::put_byte(b' '); }
        print_u64(pid_u64);
        serial::print_str("  ");

        // State column
        match state {
            0 => serial::print_str("R "),
            1 => serial::print_str("S "),
            _ => serial::print_str("Z "),
        }
        serial::print_str(" ");

        // Priority column (3 chars, right-aligned)
        let pri = priority as u64;
        let pd = if pri >= 100 { 3 } else if pri >= 10 { 2 } else { 1 };
        for _ in 0..(3 - pd) { serial::put_byte(b' '); }
        print_u64(pri);
        serial::print_str("  ");

        // Name (stop at first NUL)
        let nlen = name.iter().position(|&b| b == 0).unwrap_or(16);
        if nlen == 0 {
            serial::print_str("(no name)");
        } else {
            for &b in &name[..nlen] { serial::put_byte(b); }
        }

        if pid == my_pid { serial::print_str(" \x1b[1;32m*\x1b[0m"); }
        serial::put_byte(b'\n');
    }
}

fn cmd_mem() {
    serial::print_str("Physical memory: managed by kernel bitmap allocator\n");
    serial::print_str("  Frame size:   4 KB\n");
    serial::print_str("  Max frames:   65 536  (256 MB)\n");
    serial::print_str("  Bump start:   0x1780000 (typical QEMU)\n");
    serial::print_str("  Kernel heap:  1 MB static BSS array\n");
    serial::print_str("  Pool:         512 × 4 KB page-table frames\n");
    serial::print_str("(detailed stats require kernel IPC extension)\n");
}

// ── VFS write helpers ─────────────────────────────────────────────────────────

/// Pack up to CHUNK_SIZE bytes from `src` into 6 little-endian-packed u64 words.
fn pack_data_words(src: &[u8], words: &mut [u64; 6]) {
    for (i, &b) in src.iter().enumerate().take(CHUNK_SIZE) {
        words[i / 8] |= (b as u64) << ((i % 8) * 8);
    }
}

/// Stream `data` into VFS at `path` using WRITE_OPEN / WRITE_DATA / WRITE_CLOSE.
///
/// Returns `true` on success.
fn vfs_write_file(path: &[u8], flags: u8, data: &[u8]) -> bool {
    let (pw, _) = path_as_words(path);

    // WRITE_OPEN
    let mut req = Msg::zeroed();
    req.data[0] = OP_WRITE_OPEN;
    req.data[1] = flags as u64;
    req.data[2..8].copy_from_slice(&pw);
    if !send_msg(VFS_PID, &req) { return false; }
    let mut resp = Msg::zeroed();
    if !recv_msg(VFS_TIMEOUT, &mut resp) || resp.data[0] != RESP_OK { return false; }

    // WRITE_DATA — send CHUNK_SIZE bytes per message
    let mut offset = 0usize;
    while offset < data.len() {
        let end = (offset + CHUNK_SIZE).min(data.len());
        let chunk = &data[offset..end];
        let mut words = [0u64; 6];
        pack_data_words(chunk, &mut words);
        let mut req = Msg::zeroed();
        req.data[0] = OP_WRITE_DATA;
        req.data[1] = chunk.len() as u64;
        req.data[2..8].copy_from_slice(&words);
        if !send_msg(VFS_PID, &req) { return false; }
        let mut resp = Msg::zeroed();
        if !recv_msg(VFS_TIMEOUT, &mut resp) || resp.data[0] != RESP_OK { return false; }
        offset = end;
    }

    // WRITE_CLOSE
    let mut req = Msg::zeroed();
    req.data[0] = OP_WRITE_CLOSE;
    if !send_msg(VFS_PID, &req) { return false; }
    let mut resp = Msg::zeroed();
    recv_msg(VFS_TIMEOUT, &mut resp) && resp.data[0] == RESP_OK
}

fn cmd_touch(args: &Args<'_>, cwd: &[u8]) -> u64 {
    let raw = args.get(1);
    if raw.is_empty() {
        serial::print_str("usage: touch <path>\n");
        return 1;
    }
    let (res_buf, res_len) = resolve(cwd, raw);
    if vfs_write_file(&res_buf[..res_len], 0, b"") {
        0
    } else {
        serial::print_str("touch: failed (table full or path error)\n");
        1
    }
}

fn cmd_mkdir(args: &Args<'_>, cwd: &[u8]) -> u64 {
    let raw = args.get(1);
    if raw.is_empty() {
        serial::print_str("usage: mkdir <path>\n");
        return 1;
    }
    let (res_buf, res_len) = resolve(cwd, raw);
    let (pw, _) = path_as_words(&res_buf[..res_len]);

    let mut req = Msg::zeroed();
    req.data[0] = OP_MKDIR;
    req.data[1] = 0;
    req.data[2..8].copy_from_slice(&pw);
    if !send_msg(VFS_PID, &req) {
        serial::print_str("mkdir: failed to reach VFS\n");
        return 1;
    }
    let mut resp = Msg::zeroed();
    if !recv_msg(VFS_TIMEOUT, &mut resp) {
        serial::print_str("mkdir: VFS timeout\n");
        return 1;
    }
    match resp.data[0] {
        RESP_OK    => 0,
        RESP_ERROR => {
            serial::print_str("mkdir: ");
            match resp.data[1] {
                5 => serial::print_str("no space left\n"),
                _ => serial::print_str("error\n"),
            }
            1
        }
        _ => { serial::print_str("mkdir: unexpected response\n"); 1 }
    }
}

fn cmd_rm(args: &Args<'_>, cwd: &[u8]) -> u64 {
    let raw = args.get(1);
    if raw.is_empty() {
        serial::print_str("usage: rm <path>\n");
        return 1;
    }
    let (res_buf, res_len) = resolve(cwd, raw);
    let (pw, _) = path_as_words(&res_buf[..res_len]);

    let mut req = Msg::zeroed();
    req.data[0] = OP_UNLINK;
    req.data[2..8].copy_from_slice(&pw);
    if !send_msg(VFS_PID, &req) {
        serial::print_str("rm: failed to reach VFS\n");
        return 1;
    }
    let mut resp = Msg::zeroed();
    if !recv_msg(VFS_TIMEOUT, &mut resp) {
        serial::print_str("rm: VFS timeout\n");
        return 1;
    }
    match resp.data[0] {
        RESP_OK    => 0,
        RESP_ERROR => {
            serial::print_str("rm: ");
            for &b in raw { serial::put_byte(b); }
            match resp.data[1] {
                1 => serial::print_str(": no such file or directory\n"),
                _ => serial::print_str(": error\n"),
            }
            1
        }
        _ => { serial::print_str("rm: unexpected response\n"); 1 }
    }
}

/// write <path> <word1> [word2 ...] — write space-joined args as text to a file.
fn cmd_write(args: &Args<'_>, cwd: &[u8]) -> u64 {
    if args.count < 2 {
        serial::print_str("usage: write <path> <text...>\n");
        return 1;
    }
    let raw = args.get(1);
    let (res_buf, res_len) = resolve(cwd, raw);

    // Assemble text from remaining args into a 4096-byte buffer.
    let mut text = [0u8; 4096];
    let mut tlen = 0usize;
    for i in 2..args.count {
        if i > 2 && tlen < text.len() { text[tlen] = b' '; tlen += 1; }
        for &b in args.items[i] {
            if tlen < text.len() { text[tlen] = b; tlen += 1; }
        }
    }
    if tlen < text.len() { text[tlen] = b'\n'; tlen += 1; }

    if vfs_write_file(&res_buf[..res_len], 0, &text[..tlen]) {
        0
    } else {
        serial::print_str("write: failed (table full or path error)\n");
        1
    }
}

fn cmd_exec(args: &Args<'_>, cwd: &[u8]) {
    let path_raw = args.get(1);
    if path_raw.is_empty() {
        serial::print_str("usage: exec <path> [priority]\n");
        return;
    }
    let priority = if args.count > 2 { parse_u64(args.get(2)).min(255) as u8 } else { 0 };

    // ── Read ELF from VFS ─────────────────────────────────────────────────────
    // SAFETY: EXEC_BUF is only accessed from the single-threaded shell loop.
    // Use addr_of_mut! to avoid the Rust 2024 `static_mut_refs` lint.
    let buf: &mut [u8; EXEC_BUF_CAP] =
        unsafe { &mut *core::ptr::addr_of_mut!(EXEC_BUF) };
    let (res_buf, res_len) = resolve(cwd, path_raw);
    let (pw, _) = path_as_words(&res_buf[..res_len]);

    let mut offset: u64 = 0;
    let mut total:  u64 = u64::MAX;
    let mut file_len: usize = 0;

    'read: loop {
        let mut req = Msg::zeroed();
        req.data[0] = OP_READ;
        req.data[1] = offset;
        req.data[2..8].copy_from_slice(&pw);
        if !send_msg(VFS_PID, &req) {
            serial::print_str("exec: failed to reach VFS\n");
            return;
        }
        let mut resp = Msg::zeroed();
        if !recv_msg(VFS_TIMEOUT, &mut resp) {
            serial::print_str("exec: VFS timeout\n");
            return;
        }
        match resp.data[0] {
            RESP_DATA => {
                if total == u64::MAX { total = resp.data[1]; }
                let chunk = resp.data[2] as usize;
                if chunk == 0 { break; }
                if file_len + chunk > EXEC_BUF_CAP {
                    serial::print_str("exec: ELF too large (> 512 KB)\n");
                    return;
                }
                let mut tmp = [0u8; 40];
                unpack_bytes(&resp.data[3..8], &mut tmp, chunk);
                buf[file_len..file_len + chunk].copy_from_slice(&tmp[..chunk]);
                file_len += chunk;
                offset += chunk as u64;
                if offset >= total { break 'read; }
            }
            RESP_DONE => break,
            RESP_ERROR => {
                let errno = resp.data[1];
                serial::print_str("exec: ");
                for &b in path_raw { serial::put_byte(b); }
                match errno {
                    1 => serial::print_str(": no such file or directory\n"),
                    3 => serial::print_str(": is a directory\n"),
                    _ => serial::print_str(": error\n"),
                }
                return;
            }
            _ => { serial::print_str("exec: unexpected VFS response\n"); return; }
        }
    }

    if file_len == 0 {
        serial::print_str("exec: empty file\n");
        return;
    }

    // Quick ELF magic check before handing to kernel.
    if file_len < 4 || &buf[..4] != b"\x7fELF" {
        serial::print_str("exec: not an ELF binary\n");
        return;
    }

    // ── Spawn via SYS_SPAWN_ELF ───────────────────────────────────────────────
    match crate::syscall::spawn_elf(&buf[..file_len], priority) {
        Some(pid) => {
            serial::print_str("exec: spawned PID ");
            print_u64(pid as u64);
            serial::put_byte(b'\n');
        }
        None => {
            serial::print_str("exec: failed to spawn process\n");
        }
    }
}

fn cmd_kill(args: &Args<'_>) -> u64 {
    let pid_bytes = args.get(1);
    if pid_bytes.is_empty() {
        serial::print_str("usage: kill <pid>\n");
        return 1;
    }
    let pid    = parse_u64(pid_bytes);
    let result = crate::syscall::notify(pid, 0x0000_0001);
    if result == 0 {
        serial::print_str("kill: signal sent to PID ");
        print_u64(pid);
        serial::put_byte(b'\n');
        0
    } else {
        serial::print_str("kill: no such process\n");
        1
    }
}

// ── VFS commands ──────────────────────────────────────────────────────────────

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
                let flags  = resp.data[1];
                let size   = resp.data[2];
                let is_dir = flags & 1 != 0;
                let is_exe = flags & 2 != 0;
                if is_dir      { serial::print_str("\x1b[1;34m"); }
                else if is_exe { serial::print_str("\x1b[1;32m"); }
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
            _ => { serial::print_str("ls: unexpected response\n"); return; }
        }
    }
    if !found { serial::print_str("(empty)\n"); }
}

fn cmd_cat(args: &Args<'_>, cwd: &[u8]) {
    let raw = args.get(1);
    if raw.is_empty() {
        serial::print_str("usage: cat <path>\n");
        return;
    }
    let (res_buf, res_len) = resolve(cwd, raw);
    let (pw, _) = path_as_words(&res_buf[..res_len]);

    let mut offset: u64 = 0;
    let mut total:  u64 = u64::MAX;

    loop {
        let mut req = Msg::zeroed();
        req.data[0] = OP_READ;
        req.data[1] = offset;
        req.data[2..8].copy_from_slice(&pw);

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
                return;
            }
            _ => { serial::print_str("cat: unexpected response\n"); return; }
        }
    }
}

fn cmd_cd(args: &Args<'_>, cwd: &[u8]) -> (Action, u64) {
    let raw = args.get(1);
    let new = if raw.is_empty() || raw == b"~" {
        let mut buf = [0u8; 64];
        let home = b"/home/user";
        let l = home.len().min(63);
        buf[..l].copy_from_slice(&home[..l]);
        (buf, l)
    } else {
        resolve(cwd, raw)
    };
    (Action::Cd(new.0, new.1), 0)
}

fn cmd_pwd(cwd: &[u8]) {
    let len = cwd.iter().position(|&b| b == 0).unwrap_or(cwd.len());
    for &b in &cwd[..len] { serial::put_byte(b); }
    serial::put_byte(b'\n');
}

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
                print_packed_name(&resp.data[2..6]);
                serial::print_str("\t ");
                print_packed_name(&resp.data[6..8]);
                serial::put_byte(b'\n');
            }
            _ => { serial::print_str("mount: unexpected response\n"); return; }
        }
    }
}

fn cmd_source(args: &Args<'_>, ctx: &mut ExecCtx<'_>) -> (Action, u64) {
    let path_raw = args.get(1);
    if path_raw.is_empty() {
        serial::print_str("usage: source <file>\n");
        return (Action::Continue, 1);
    }

    let (res_buf, res_len) = resolve(ctx.cwd, path_raw);
    let (pw, _) = path_as_words(&res_buf[..res_len]);

    // Read entire file in chunks into a fixed 2 KB buffer.
    const BUF_CAP: usize = 2048;
    let mut file_buf = [0u8; BUF_CAP];
    let mut file_len = 0usize;
    let mut offset: u64 = 0;
    let mut total: u64 = u64::MAX;

    'read: loop {
        let mut req = Msg::zeroed();
        req.data[0] = OP_READ;
        req.data[1] = offset;
        req.data[2..8].copy_from_slice(&pw);
        if !send_msg(VFS_PID, &req) { break; }
        let mut resp = Msg::zeroed();
        if !recv_msg(VFS_TIMEOUT, &mut resp) { break; }
        match resp.data[0] {
            RESP_DATA => {
                if total == u64::MAX { total = resp.data[1]; }
                let chunk = resp.data[2] as usize;
                if chunk == 0 { break; }
                let copy = chunk.min(BUF_CAP - file_len);
                let mut tmp = [0u8; 40];
                unpack_bytes(&resp.data[3..8], &mut tmp, chunk);
                file_buf[file_len..file_len + copy].copy_from_slice(&tmp[..copy]);
                file_len += copy;
                offset += chunk as u64;
                if file_len >= BUF_CAP || offset >= total { break 'read; }
            }
            RESP_DONE => break,
            RESP_ERROR => {
                let errno = resp.data[1];
                serial::print_str("source: ");
                for &b in path_raw { serial::put_byte(b); }
                match errno {
                    1 => serial::print_str(": no such file or directory\n"),
                    3 => serial::print_str(": is a directory\n"),
                    _ => serial::print_str(": error\n"),
                }
                return (Action::Continue, 1);
            }
            _ => break,
        }
    }

    // Execute each line.
    let mut last_exit = 0u64;
    let mut line_start = 0usize;

    for i in 0..=file_len {
        let at_end = i == file_len;
        let is_newline = !at_end && (file_buf[i] == b'\n' || file_buf[i] == b'\r');
        if is_newline || at_end {
            let line = trim(&file_buf[line_start..i]);
            // Skip empty lines and comments.
            if !line.is_empty() && line[0] != b'#' {
                let mut sub_ctx = ExecCtx {
                    history:   ctx.history,
                    vars:      ctx.vars,
                    aliases:   ctx.aliases,
                    cwd:       ctx.cwd,
                    pid:       ctx.pid,
                    last_exit,
                };
                let (action, code) = execute_line(line, &mut sub_ctx);
                last_exit = code;
                match action {
                    Action::Exit(c)  => return (Action::Exit(c), last_exit),
                    Action::Cd(p, l) => return (Action::Cd(p, l), last_exit),
                    Action::Continue => {}
                }
            }
            line_start = i + 1;
        }
    }

    (Action::Continue, last_exit)
}

// ── test / [ built-in ─────────────────────────────────────────────────────────

fn cmd_test_bracket(args: &Args<'_>, cwd: &[u8]) -> u64 {
    // Strip trailing `]`
    let mut stripped_args = *args;
    if stripped_args.count > 0 && stripped_args.items[stripped_args.count - 1] == b"]" {
        stripped_args.count -= 1;
        stripped_args.items[stripped_args.count] = b"";
    }
    eval_test(&stripped_args, cwd)
}

fn cmd_test(args: &Args<'_>, cwd: &[u8]) -> u64 {
    eval_test(args, cwd)
}

fn eval_test(args: &Args<'_>, cwd: &[u8]) -> u64 {
    match args.count {
        // test -flag arg
        3 if args.get(1).starts_with(b"-") => {
            let flag = args.get(1);
            let arg  = args.get(2);
            match flag {
                b"-f" => {
                    // File exists and is regular
                    let (buf, len) = resolve(cwd, arg);
                    if vfs_stat_exists(&buf[..len]) { 0 } else { 1 }
                }
                b"-d" => {
                    // Is directory
                    let (buf, len) = resolve(cwd, arg);
                    if vfs_stat_isdir(&buf[..len]) { 0 } else { 1 }
                }
                b"-z" => if arg.is_empty() { 0 } else { 1 },
                b"-n" => if !arg.is_empty() { 0 } else { 1 },
                _     => 1,
            }
        }
        // test A op B
        4 => {
            let lhs = args.get(1);
            let op  = args.get(2);
            let rhs = args.get(3);
            match op {
                b"="  | b"==" => if lhs == rhs { 0 } else { 1 },
                b"!=" => if lhs != rhs { 0 } else { 1 },
                b"-eq" => cmp_num(lhs, rhs, |a,b| a == b),
                b"-ne" => cmp_num(lhs, rhs, |a,b| a != b),
                b"-lt" => cmp_num(lhs, rhs, |a,b| a <  b),
                b"-le" => cmp_num(lhs, rhs, |a,b| a <= b),
                b"-gt" => cmp_num(lhs, rhs, |a,b| a >  b),
                b"-ge" => cmp_num(lhs, rhs, |a,b| a >= b),
                _ => 1,
            }
        }
        _ => 1,
    }
}

fn cmp_num(a: &[u8], b: &[u8], f: fn(u64,u64)->bool) -> u64 {
    let na = parse_u64(a);
    let nb = parse_u64(b);
    if f(na, nb) { 0 } else { 1 }
}

fn vfs_stat_exists(path: &[u8]) -> bool {
    let (pw, _) = path_as_words(path);
    let mut req = Msg::zeroed();
    req.data[0] = OP_STAT;
    req.data[2..8].copy_from_slice(&pw);
    if !send_msg(VFS_PID, &req) { return false; }
    let mut resp = Msg::zeroed();
    recv_msg(VFS_TIMEOUT, &mut resp) && resp.data[0] == RESP_STAT
}

fn vfs_stat_isdir(path: &[u8]) -> bool {
    let (pw, _) = path_as_words(path);
    let mut req = Msg::zeroed();
    req.data[0] = OP_STAT;
    req.data[2..8].copy_from_slice(&pw);
    if !send_msg(VFS_PID, &req) { return false; }
    let mut resp = Msg::zeroed();
    if !recv_msg(VFS_TIMEOUT, &mut resp) { return false; }
    resp.data[0] == RESP_STAT && (resp.data[1] & 1 != 0)
}

// ── Path helpers ──────────────────────────────────────────────────────────────

fn resolve(cwd: &[u8], input: &[u8]) -> ([u8; 64], usize) {
    let cwd_len = cwd.iter().position(|&b| b == 0).unwrap_or(cwd.len());
    let cwd     = &cwd[..cwd_len];

    if input.starts_with(b"/") {
        let mut buf = [0u8; 64];
        let len = input.len().min(63);
        buf[..len].copy_from_slice(&input[..len]);
        let len = if len > 1 && buf[len - 1] == b'/' { len - 1 } else { len };
        return (buf, len);
    }

    if input == b".." {
        let mut buf = [0u8; 64];
        let nl = parent_of(cwd);
        buf[..nl].copy_from_slice(&cwd[..nl]);
        return (buf, nl);
    }

    if input == b"." {
        let mut buf = [0u8; 64];
        buf[..cwd_len].copy_from_slice(cwd);
        return (buf, cwd_len);
    }

    // Relative: append to cwd
    let mut buf = [0u8; 64];
    let mut len = cwd_len.min(63);
    buf[..len].copy_from_slice(&cwd[..len]);
    if len > 0 && buf[len - 1] != b'/' && len < 63 { buf[len] = b'/'; len += 1; }
    for &b in input.iter().take(63 - len) { buf[len] = b; len += 1; }
    (buf, len)
}

fn parent_of(path: &[u8]) -> usize {
    let stripped = if path.last() == Some(&b'/') && path.len() > 1 {
        &path[..path.len() - 1]
    } else { path };
    match stripped.iter().rposition(|&b| b == b'/') {
        Some(0) | None => 1,
        Some(pos)      => pos,
    }
}

fn make_path(raw: &[u8]) -> ([u8; 64], usize) {
    let mut buf = [0u8; 64];
    let len = raw.len().min(63);
    buf[..len].copy_from_slice(&raw[..len]);
    (buf, len)
}

fn path_as_words(path: &[u8]) -> ([u64; 6], usize) {
    let len = path.iter().position(|&b| b == 0).unwrap_or(path.len());
    let path = &path[..len];
    let mut words = [0u64; 6];
    for (i, &b) in path.iter().enumerate().take(48) {
        words[i / 8] |= (b as u64) << ((i % 8) * 8);
    }
    (words, len)
}

fn print_packed_name(words: &[u64]) {
    'outer: for &w in words {
        for i in 0..8 {
            let b = (w >> (i * 8)) as u8;
            if b == 0 { break 'outer; }
            serial::put_byte(b);
        }
    }
}

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

fn unpack_name(words: &[u64], out: &mut [u8; 48]) -> usize {
    let mut len = 0usize;
    'outer: for &w in words {
        for i in 0..8 {
            let b = (w >> (i * 8)) as u8;
            if b == 0 { break 'outer; }
            if len < 48 { out[len] = b; len += 1; }
        }
    }
    len
}

fn unpack_bytes(words: &[u64], out: &mut [u8; 40], count: usize) {
    let mut left = count.min(40);
    let mut pos  = 0usize;
    'outer: for &w in words {
        for i in 0..8 {
            if left == 0 { break 'outer; }
            out[pos] = (w >> (i * 8)) as u8;
            pos += 1;
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

fn trim(s: &[u8]) -> &[u8] {
    let s = if let Some(i) = s.iter().position(|&b| b != b' ' && b != b'\t') { &s[i..] } else { return b""; };
    let e = s.iter().rposition(|&b| b != b' ' && b != b'\t').map(|i| i + 1).unwrap_or(0);
    &s[..e]
}
