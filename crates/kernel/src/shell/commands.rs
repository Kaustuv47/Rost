use hal::uart as serial;
use super::line_editor::LineEditor;
use super::history::History;

// ── Command registry ────────────────────────────────────────────────────────

/// All built-in command names, kept sorted for binary-search completion.
const COMMANDS: &[&[u8]] = &[
    b"clear",
    b"echo",
    b"halt",
    b"help",
    b"history",
];

pub enum Action {
    Continue,
    Halt,
}

// ── Tokenizer ───────────────────────────────────────────────────────────────

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
        // Skip whitespace
        while i < line.len() && line[i] == b' ' { i += 1; }
        if i >= line.len() { break; }

        if line[i] == b'"' {
            i += 1; // skip opening quote
            let start = i;
            while i < line.len() && line[i] != b'"' { i += 1; }
            args.items[args.count] = &line[start..i];
            args.count += 1;
            if i < line.len() { i += 1; } // skip closing quote
        } else {
            let start = i;
            while i < line.len() && line[i] != b' ' { i += 1; }
            args.items[args.count] = &line[start..i];
            args.count += 1;
        }
    }

    args
}

// ── Dispatch ────────────────────────────────────────────────────────────────

pub fn dispatch(line: &[u8], history: &History) -> Action {
    let args = tokenize(line);
    let cmd = args.get(0);
    if cmd.is_empty() { return Action::Continue; }

    match cmd {
        b"clear"   => cmd_clear(),
        b"echo"    => cmd_echo(&args),
        b"halt"    => return Action::Halt,
        b"help"    => cmd_help(),
        b"history" => cmd_history(history),
        _ => {
            serial::print_str("rost: command not found: ");
            for &b in cmd { serial::put_byte(b); }
            serial::put_byte(b'\n');
        }
    }

    Action::Continue
}

// ── Tab completion ───────────────────────────────────────────────────────────

/// Attempt to complete the command name currently being typed.
///
/// - 0 matches  → ring the terminal bell, return 0
/// - 1 match    → complete in-place, return 1
/// - N matches  → print candidates on a new line, return N (caller redraws)
pub fn try_complete(editor: &mut LineEditor) -> usize {
    // Only complete the first word (arg completion is future work)
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
        0 => {
            serial::put_byte(0x07); // bell
        }
        1 => {
            // Append the missing suffix + a space
            for &b in &matches[0][prefix.len()..] { editor.insert(b); }
            editor.insert(b' ');
        }
        _ => {
            // Show all candidates; caller is responsible for redrawing the prompt
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

// ── Individual commands ──────────────────────────────────────────────────────

fn cmd_echo(args: &Args<'_>) {
    for i in 1..args.count {
        if i > 1 { serial::put_byte(b' '); }
        for &b in args.items[i] { serial::put_byte(b); }
    }
    serial::put_byte(b'\n');
}

fn cmd_help() {
    serial::print_str("Built-in commands:\n");
    serial::print_str("  clear              clear the screen\n");
    serial::print_str("  echo <args...>     print arguments to the console\n");
    serial::print_str("  halt               halt the system\n");
    serial::print_str("  help               show this help message\n");
    serial::print_str("  history            list command history\n");
    serial::print_str("\nLine editing:\n");
    serial::print_str("  Left / Right       move cursor one character\n");
    serial::print_str("  Home / End         jump to start or end of line\n");
    serial::print_str("  Backspace          delete character before cursor\n");
    serial::print_str("  Delete             delete character at cursor\n");
    serial::print_str("  Up / Down          browse command history\n");
    serial::print_str("  Tab                complete command name\n");
    serial::print_str("  Ctrl+C             cancel current line\n");
    serial::print_str("  Ctrl+L             clear screen\n");
}

fn cmd_clear() {
    // ESC[2J  — erase entire display
    // ESC[H   — move cursor to top-left
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

// ── Helpers ─────────────────────────────────────────────────────────────────

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
