mod commands;
mod escape;
mod history;
mod line_editor;

use crate::io as serial;

use commands::Action;
use escape::{EscapeParser, Key};
use history::History;
use line_editor::{LineEditor, LINE_MAX};

// ── Shell state ───────────────────────────────────────────────────────────────

pub struct Shell {
    editor:   LineEditor,
    history:  History,
    parser:   EscapeParser,
    /// Saved partial input when the user browses history (restored on Down).
    saved:    LineEditor,
    /// `None` = editing fresh line; `Some(age)` = browsing history.
    hist_idx: Option<usize>,
    /// Current working directory (absolute, null-terminated in buf[..cwd_len]).
    cwd:     [u8; 64],
    cwd_len: usize,
}

impl Shell {
    pub fn new() -> Self {
        let mut cwd = [0u8; 64];
        cwd[0] = b'/';
        Shell {
            editor:   LineEditor::new(),
            history:  History::new(),
            parser:   EscapeParser::new(),
            saved:    LineEditor::new(),
            hist_idx: None,
            cwd,
            cwd_len: 1,
        }
    }

    pub fn run(&mut self) -> ! {
        serial::print_str("\x1b[1;32mRost Shell\x1b[0m — type \x1b[1mhelp\x1b[0m for commands\n");
        print_prompt(&self.editor, &self.cwd[..self.cwd_len]);

        loop {
            if let Some(byte) = serial::read_byte() {
                if let Some(key) = self.parser.feed(byte) {
                    self.handle_key(key);
                }
            } else {
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

                    match commands::dispatch(
                        &buf[..len],
                        &self.history,
                        &self.cwd[..self.cwd_len],
                    ) {
                        Action::Exit(code) => {
                            serial::print_str("Goodbye.\n");
                            crate::syscall::exit(code);
                        }
                        Action::Cd(path, len) => {
                            self.cwd[..len].copy_from_slice(&path[..len]);
                            // Null-terminate the rest for safety.
                            for b in &mut self.cwd[len..] { *b = 0; }
                            self.cwd_len = len;
                        }
                        Action::Continue => {}
                    }
                }
                print_prompt(&self.editor, &self.cwd[..self.cwd_len]);
            }

            // ── Cancel (Ctrl+C) ───────────────────────────────────────────────
            Key::CtrlC => {
                serial::print_str("^C\n");
                self.editor.clear();
                self.saved.clear();
                self.hist_idx = None;
                print_prompt(&self.editor, &self.cwd[..self.cwd_len]);
            }

            // ── Clear screen (Ctrl+L) ─────────────────────────────────────────
            Key::CtrlL => {
                serial::print_str("\x1b[2J\x1b[H");
                print_prompt(&self.editor, &self.cwd[..self.cwd_len]);
            }

            // ── Regular character ─────────────────────────────────────────────
            Key::Char(b) => {
                if self.hist_idx.is_some() {
                    self.hist_idx = None;
                    self.saved.clear();
                }
                self.editor.insert(b);
                redraw(&self.editor, &self.cwd[..self.cwd_len]);
            }

            // ── Backspace ─────────────────────────────────────────────────────
            Key::Backspace => {
                if self.editor.backspace() {
                    redraw(&self.editor, &self.cwd[..self.cwd_len]);
                }
            }

            // ── Forward delete ────────────────────────────────────────────────
            Key::Delete => {
                if self.editor.delete_forward() {
                    redraw(&self.editor, &self.cwd[..self.cwd_len]);
                }
            }

            // ── Cursor movement ───────────────────────────────────────────────
            Key::ArrowLeft  => { if self.editor.move_left()  { cursor_left(1); } }
            Key::ArrowRight => { if self.editor.move_right() { cursor_right(1); } }
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

            // ── History: Up ───────────────────────────────────────────────────
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
                    redraw(&self.editor, &self.cwd[..self.cwd_len]);
                }
            }

            // ── History: Down ─────────────────────────────────────────────────
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
                        redraw(&self.editor, &self.cwd[..self.cwd_len]);
                    }
                    Some(age) => {
                        let prev = age - 1;
                        if let Some(entry) = self.history.get(prev) {
                            let mut tmp = [0u8; LINE_MAX];
                            let n = entry.len();
                            tmp[..n].copy_from_slice(entry);
                            self.hist_idx = Some(prev);
                            self.editor.load(&tmp[..n]);
                            redraw(&self.editor, &self.cwd[..self.cwd_len]);
                        }
                    }
                }
            }

            // ── Tab completion ────────────────────────────────────────────────
            Key::Tab => {
                let n = commands::try_complete(&mut self.editor);
                if n > 1 {
                    print_prompt(&self.editor, &self.cwd[..self.cwd_len]);
                } else if n == 1 {
                    redraw(&self.editor, &self.cwd[..self.cwd_len]);
                }
            }
        }
    }
}

// ── Display helpers ───────────────────────────────────────────────────────────
//
// Prompt format:  \e[1;32mrost\e[0m@\e[1;34mlocal\e[0m:<cwd>\e[0m$
// Example:        rost@local:/etc$

fn emit_prompt(cwd: &[u8]) {
    serial::print_str("\x1b[1;32mrost\x1b[0m@\x1b[1;34mlocal\x1b[0m:");
    serial::print_str("\x1b[1;37m");
    for &b in cwd { serial::put_byte(b); }
    serial::print_str("\x1b[0m$ ");
}

/// Calculate the visible character width of the prompt string.
/// Skips ANSI escape sequences (ESC [ ... m).
fn prompt_visible_len(cwd: &[u8]) -> usize {
    // "rost@local:" = 11 chars, cwd, "$ " = 2 chars
    11 + cwd.len() + 2
}

fn print_prompt(editor: &LineEditor, cwd: &[u8]) {
    emit_prompt(cwd);
    for &b in editor.as_bytes() { serial::put_byte(b); }
    let back = editor.len - editor.cursor;
    if back > 0 { cursor_left(back); }
}

fn redraw(editor: &LineEditor, cwd: &[u8]) {
    serial::put_byte(b'\r');
    emit_prompt(cwd);
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
