mod commands;
mod escape;
mod history;
mod line_editor;

use crate::io as serial;

use commands::Action;
use escape::{EscapeParser, Key};
use history::History;
use line_editor::{LineEditor, LINE_MAX};

// ── Prompt ────────────────────────────────────────────────────────────────────
// Visible text: "rost@local:~$ "
// ANSI codes:   bold-green "rost", reset, "@", bold-blue "local", reset, ":"
//               bold-white "~", reset, "$ "
const PROMPT: &[u8] =
    b"\x1b[1;32mrost\x1b[0m@\x1b[1;34mlocal\x1b[0m:\x1b[1;37m~\x1b[0m$ ";

// ── Shell state ───────────────────────────────────────────────────────────────

pub struct Shell {
    editor:   LineEditor,
    history:  History,
    parser:   EscapeParser,
    /// Line saved when the user first presses Up while typing a fresh command.
    /// Restored when they press Down back past the most-recent history entry.
    saved:    LineEditor,
    /// `None`      — user is editing a fresh line
    /// `Some(age)` — user is browsing history at this age (0 = most recent)
    hist_idx: Option<usize>,
}

impl Shell {
    pub fn new() -> Self {
        Shell {
            editor:   LineEditor::new(),
            history:  History::new(),
            parser:   EscapeParser::new(),
            saved:    LineEditor::new(),
            hist_idx: None,
        }
    }

    pub fn run(&mut self) -> ! {
        // Print banner once at startup.
        serial::print_str("\x1b[1;32mRost Shell\x1b[0m — type \x1b[1mhelp\x1b[0m for commands\n");
        print_prompt(&self.editor);

        loop {
            if let Some(byte) = serial::read_byte() {
                if let Some(key) = self.parser.feed(byte) {
                    self.handle_key(key);
                }
            } else {
                // No input — yield the CPU slice back to the kernel so other
                // processes can run.  We will be scheduled again when the uart-
                // drv delivers a keystroke to our mailbox.
                crate::syscall::yield_cpu();
            }
        }
    }

    fn handle_key(&mut self, key: Key) {
        match key {

            // ── Submit ────────────────────────────────────────────────────────
            Key::Enter => {
                serial::put_byte(b'\n');
                if !self.editor.is_empty() {
                    let mut buf = [0u8; LINE_MAX];
                    let len = self.editor.len;
                    buf[..len].copy_from_slice(self.editor.as_bytes());

                    self.history.push(&buf[..len]);
                    self.editor.clear();
                    self.saved.clear();
                    self.hist_idx = None;

                    match commands::dispatch(&buf[..len], &self.history) {
                        Action::Exit(code) => {
                            serial::print_str("Goodbye.\n");
                            crate::syscall::exit(code);
                        }
                        Action::Continue => {}
                    }
                }
                print_prompt(&self.editor);
            }

            // ── Cancel current line (Ctrl+C) ──────────────────────────────────
            Key::CtrlC => {
                serial::print_str("^C\n");
                self.editor.clear();
                self.saved.clear();
                self.hist_idx = None;
                print_prompt(&self.editor);
            }

            // ── Clear screen (Ctrl+L) ─────────────────────────────────────────
            Key::CtrlL => {
                serial::print_str("\x1b[2J\x1b[H");
                print_prompt(&self.editor);
            }

            // ── Regular character ─────────────────────────────────────────────
            Key::Char(b) => {
                if self.hist_idx.is_some() {
                    self.hist_idx = None;
                    self.saved.clear();
                }
                self.editor.insert(b);
                redraw(&self.editor);
            }

            // ── Backspace ─────────────────────────────────────────────────────
            Key::Backspace => {
                if self.editor.backspace() {
                    redraw(&self.editor);
                }
            }

            // ── Forward delete ────────────────────────────────────────────────
            Key::Delete => {
                if self.editor.delete_forward() {
                    redraw(&self.editor);
                }
            }

            // ── Cursor: left / right ──────────────────────────────────────────
            Key::ArrowLeft  => { if self.editor.move_left()  { cursor_left(1); } }
            Key::ArrowRight => { if self.editor.move_right() { cursor_right(1); } }

            // ── Cursor: home / end ────────────────────────────────────────────
            Key::Home => {
                let n = self.editor.cursor;
                self.editor.home();
                if n > 0 { cursor_left(n); }
            }
            Key::End => {
                let n = self.editor.len - self.editor.cursor;
                self.editor.end();
                if n > 0 { cursor_right(n); }
            }

            // ── History: older (Up) ───────────────────────────────────────────
            Key::ArrowUp => {
                let next_age = self.hist_idx.map(|a| a + 1).unwrap_or(0);
                if let Some(entry) = self.history.get(next_age) {
                    if self.hist_idx.is_none() {
                        self.saved.clear();
                        self.saved.load(self.editor.as_bytes());
                    }
                    let mut tmp = [0u8; LINE_MAX];
                    let n = entry.len();
                    tmp[..n].copy_from_slice(entry);
                    self.hist_idx = Some(next_age);
                    self.editor.load(&tmp[..n]);
                    redraw(&self.editor);
                }
            }

            // ── History: newer (Down) ─────────────────────────────────────────
            Key::ArrowDown => {
                match self.hist_idx {
                    None => {}
                    Some(0) => {
                        self.hist_idx = None;
                        let mut tmp = [0u8; LINE_MAX];
                        let n = self.saved.len;
                        tmp[..n].copy_from_slice(self.saved.as_bytes());
                        self.saved.clear();
                        self.editor.load(&tmp[..n]);
                        redraw(&self.editor);
                    }
                    Some(age) => {
                        let prev_age = age - 1;
                        if let Some(entry) = self.history.get(prev_age) {
                            let mut tmp = [0u8; LINE_MAX];
                            let n = entry.len();
                            tmp[..n].copy_from_slice(entry);
                            self.hist_idx = Some(prev_age);
                            self.editor.load(&tmp[..n]);
                            redraw(&self.editor);
                        }
                    }
                }
            }

            // ── Tab completion ────────────────────────────────────────────────
            Key::Tab => {
                let n = commands::try_complete(&mut self.editor);
                if n > 1 {
                    print_prompt(&self.editor);
                } else if n == 1 {
                    redraw(&self.editor);
                }
            }
        }
    }
}

// ── Display helpers ───────────────────────────────────────────────────────────

fn print_prompt(editor: &LineEditor) {
    for &b in PROMPT { serial::put_byte(b); }
    for &b in editor.as_bytes() { serial::put_byte(b); }
    let back = editor.len - editor.cursor;
    if back > 0 { cursor_left(back); }
}

fn redraw(editor: &LineEditor) {
    serial::put_byte(b'\r');
    for &b in PROMPT { serial::put_byte(b); }
    for &b in editor.as_bytes() { serial::put_byte(b); }
    serial::print_str("\x1b[K");
    let back = editor.len - editor.cursor;
    if back > 0 { cursor_left(back); }
}

fn cursor_left(n: usize) {
    if n == 0 { return; }
    serial::print_str("\x1b[");
    commands::print_usize(n);
    serial::put_byte(b'D');
}

fn cursor_right(n: usize) {
    if n == 0 { return; }
    serial::print_str("\x1b[");
    commands::print_usize(n);
    serial::put_byte(b'C');
}
