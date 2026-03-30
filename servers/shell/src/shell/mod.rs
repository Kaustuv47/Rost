//! Shell main loop — zsh-compatible interactive shell.
//!
//! Features:
//!   - Full emacs-mode line editing (Ctrl+A/B/C/D/E/F/K/L/N/P/R/U/W/Y)
//!   - Kill ring (Ctrl+K/U/W to kill, Ctrl+Y to yank)
//!   - Ctrl+R incremental reverse history search
//!   - Word movement (Alt+B / Alt+F, Ctrl+Left / Ctrl+Right)
//!   - Command history (32 entries), history expansion (!!, !n, !prefix)
//!   - Tab completion: commands and filesystem paths
//!   - Variable store ($VAR, ${VAR}, export, unset)
//!   - Alias table (alias, unalias)
//!   - Compound commands (; && ||)
//!   - Script execution (source / .)

mod aliases;
mod commands;
mod escape;
mod expand;
mod history;
mod line_editor;
mod vars;

use crate::io as serial;

use aliases::AliasStore;
use commands::{Action, ExecCtx};
use escape::{EscapeParser, Key};
use history::History;
use line_editor::{LineEditor, LINE_MAX};
use vars::VarStore;

// ── Shell state ───────────────────────────────────────────────────────────────

pub struct Shell {
    // Line editor
    editor:    LineEditor,
    // Kill ring (single-entry, like standard zsh)
    kill_ring: [u8; LINE_MAX],
    kill_len:  usize,
    // History
    history:   History,
    // Escape sequence parser
    parser:    EscapeParser,
    // History browsing — None = editing fresh line; Some(age) = browsing
    hist_idx:  Option<usize>,
    // Saved fresh line while browsing history
    saved:     LineEditor,
    // Ctrl+R incremental reverse-search state
    search:    SearchState,
    // Current working directory
    cwd:       [u8; 64],
    cwd_len:   usize,
    // Environment variables
    vars:      VarStore,
    // Alias table
    aliases:   AliasStore,
    // Last command exit code ($?)
    last_exit: u64,
    // Our own PID (cached from SYS_GETPID)
    my_pid:    u32,
}

struct SearchState {
    active:    bool,
    query:     [u8; 64],
    qlen:      usize,
    /// The age of the currently-displayed history match (0 = most recent).
    match_age: usize,
    /// Editor contents before Ctrl+R was pressed (restored on Esc/Ctrl+G).
    saved:     LineEditor,
}

impl SearchState {
    const fn new() -> Self {
        SearchState {
            active:    false,
            query:     [0u8; 64],
            qlen:      0,
            match_age: 0,
            saved:     LineEditor::new(),
        }
    }

    fn reset(&mut self) {
        self.active    = false;
        self.qlen      = 0;
        self.match_age = 0;
    }
}

impl Shell {
    pub fn new() -> Self {
        let mut vars = VarStore::new();
        vars.init_defaults();
        let mut aliases = AliasStore::new();
        aliases.init_defaults();

        let my_pid = crate::syscall::getpid();

        let mut cwd = [0u8; 64];
        cwd[0] = b'/';

        Shell {
            editor:    LineEditor::new(),
            kill_ring: [0u8; LINE_MAX],
            kill_len:  0,
            history:   History::new(),
            parser:    EscapeParser::new(),
            hist_idx:  None,
            saved:     LineEditor::new(),
            search:    SearchState::new(),
            cwd,
            cwd_len:   1,
            vars,
            aliases,
            last_exit: 0,
            my_pid,
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
                // No input — yield the CPU slice back to the kernel so other
                // processes (uart-drv) can run and push keystrokes to our
                // mailbox.  Using yield keeps us Ready so we are rescheduled
                // at the very next tick rather than blocking for a full timeout.
                crate::syscall::yield_cpu();
            }
        }
    }

    fn handle_key(&mut self, key: Key) {
        // ── Ctrl+R: incremental reverse search ───────────────────────────────
        if self.search.active {
            self.handle_search_key(key);
            return;
        }

        match key {

            // ── Submit (Enter) ────────────────────────────────────────────────
            Key::Enter => {
                serial::put_newline();
                if !self.editor.is_empty() {
                    let mut buf = [0u8; LINE_MAX];
                    let len = self.editor.len;
                    buf[..len].copy_from_slice(self.editor.as_bytes());

                    self.history.push(&buf[..len]);
                    self.editor.clear();
                    self.saved.clear();
                    self.hist_idx = None;

                    let mut ctx = ExecCtx {
                        history:   &self.history,
                        vars:      &mut self.vars,
                        aliases:   &mut self.aliases,
                        cwd:       &self.cwd[..self.cwd_len],
                        pid:       self.my_pid,
                        last_exit: self.last_exit,
                    };

                    let (action, exit) = commands::execute_line(&buf[..len], &mut ctx);
                    self.last_exit = exit;

                    match action {
                        Action::Exit(code) => {
                            serial::print_str("Goodbye.\n");
                            crate::syscall::exit(code);
                        }
                        Action::Cd(path, len) => {
                            // Update OLDPWD, then PWD.
                            self.vars.set(b"OLDPWD", &self.cwd[..self.cwd_len]);
                            self.cwd[..len].copy_from_slice(&path[..len]);
                            for b in &mut self.cwd[len..] { *b = 0; }
                            self.cwd_len = len;
                            self.vars.set(b"PWD", &self.cwd[..len]);
                        }
                        Action::Continue => {}
                    }
                }
                print_prompt(&self.editor, &self.cwd[..self.cwd_len]);
            }

            // ── Ctrl+C: cancel line ───────────────────────────────────────────
            Key::CtrlC => {
                serial::print_str("^C\n");
                self.editor.clear();
                self.saved.clear();
                self.hist_idx = None;
                print_prompt(&self.editor, &self.cwd[..self.cwd_len]);
            }

            // ── Ctrl+D: delete forward or EOF ─────────────────────────────────
            Key::CtrlD => {
                if self.editor.is_empty() {
                    serial::print_str("exit\nGoodbye.\n");
                    crate::syscall::exit(0);
                }
                if self.editor.delete_forward() {
                    redraw(&self.editor, &self.cwd[..self.cwd_len]);
                }
            }

            // ── Ctrl+L: clear screen ──────────────────────────────────────────
            Key::CtrlL => {
                serial::print_str("\x1b[2J\x1b[H");
                print_prompt(&self.editor, &self.cwd[..self.cwd_len]);
            }

            // ── Ctrl+R: enter reverse search ──────────────────────────────────
            Key::CtrlR => {
                self.search.active    = true;
                self.search.qlen      = 0;
                self.search.match_age = 0;
                self.search.saved.load(self.editor.as_bytes());
                self.editor.clear();
                print_search_prompt(b"", None);
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

            // ── Delete ────────────────────────────────────────────────────────
            Key::Delete => {
                if self.editor.delete_forward() {
                    redraw(&self.editor, &self.cwd[..self.cwd_len]);
                }
            }

            // ── Beginning / end of line ───────────────────────────────────────
            Key::CtrlA | Key::Home => {
                let n = self.editor.cursor;
                self.editor.home();
                if n > 0 { cursor_left(n); }
            }
            Key::CtrlE | Key::End => {
                let n = self.editor.len - self.editor.cursor;
                self.editor.end();
                if n > 0 { cursor_right(n); }
            }

            // ── Character movement ────────────────────────────────────────────
            Key::CtrlB | Key::ArrowLeft  => { if self.editor.move_left()  { cursor_left(1); } }
            Key::CtrlF | Key::ArrowRight => { if self.editor.move_right() { cursor_right(1); } }

            // ── Word movement ─────────────────────────────────────────────────
            Key::WordLeft => {
                let n = self.editor.word_left();
                if n > 0 { cursor_left(n); }
            }
            Key::WordRight => {
                let n = self.editor.word_right();
                if n > 0 { cursor_right(n); }
            }

            // ── Kill to end of line (Ctrl+K) ──────────────────────────────────
            Key::CtrlK => {
                self.kill_len = self.editor.kill_to_end(&mut self.kill_ring);
                redraw(&self.editor, &self.cwd[..self.cwd_len]);
            }

            // ── Kill to beginning of line (Ctrl+U) ────────────────────────────
            Key::CtrlU => {
                self.kill_len = self.editor.kill_to_start(&mut self.kill_ring);
                redraw(&self.editor, &self.cwd[..self.cwd_len]);
            }

            // ── Kill previous word (Ctrl+W) ───────────────────────────────────
            Key::CtrlW => {
                self.kill_len = self.editor.kill_prev_word(&mut self.kill_ring);
                redraw(&self.editor, &self.cwd[..self.cwd_len]);
            }

            // ── Yank (Ctrl+Y) ─────────────────────────────────────────────────
            Key::CtrlY => {
                if self.kill_len > 0 {
                    let ring = self.kill_ring;
                    let len  = self.kill_len;
                    if self.editor.yank(&ring, len) {
                        redraw(&self.editor, &self.cwd[..self.cwd_len]);
                    }
                }
            }

            // ── History: Up / Ctrl+P ──────────────────────────────────────────
            Key::ArrowUp | Key::CtrlP => {
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

            // ── History: Down / Ctrl+N ────────────────────────────────────────
            Key::ArrowDown | Key::CtrlN => {
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
                let n = commands::try_complete(&mut self.editor, &self.cwd[..self.cwd_len]);
                if n > 1 {
                    print_prompt(&self.editor, &self.cwd[..self.cwd_len]);
                } else if n == 1 {
                    redraw(&self.editor, &self.cwd[..self.cwd_len]);
                }
            }
        }
    }

    // ── Ctrl+R incremental reverse search ─────────────────────────────────────

    fn handle_search_key(&mut self, key: Key) {
        match key {
            // Accept the current match
            Key::Enter => {
                serial::put_newline();
                self.search.reset();
                // editor already loaded with the match; just re-prompt
                if !self.editor.is_empty() {
                    let mut buf = [0u8; LINE_MAX];
                    let len = self.editor.len;
                    buf[..len].copy_from_slice(self.editor.as_bytes());

                    self.history.push(&buf[..len]);
                    self.editor.clear();

                    let mut ctx = ExecCtx {
                        history:   &self.history,
                        vars:      &mut self.vars,
                        aliases:   &mut self.aliases,
                        cwd:       &self.cwd[..self.cwd_len],
                        pid:       self.my_pid,
                        last_exit: self.last_exit,
                    };
                    let (action, exit) = commands::execute_line(&buf[..len], &mut ctx);
                    self.last_exit = exit;

                    match action {
                        Action::Exit(code) => {
                            serial::print_str("Goodbye.\n");
                            crate::syscall::exit(code);
                        }
                        Action::Cd(path, plen) => {
                            self.vars.set(b"OLDPWD", &self.cwd[..self.cwd_len]);
                            self.cwd[..plen].copy_from_slice(&path[..plen]);
                            for b in &mut self.cwd[plen..] { *b = 0; }
                            self.cwd_len = plen;
                            self.vars.set(b"PWD", &self.cwd[..plen]);
                        }
                        Action::Continue => {}
                    }
                }
                print_prompt(&self.editor, &self.cwd[..self.cwd_len]);
            }

            // Cancel search — restore original line
            Key::CtrlC => {
                self.search.reset();
                self.editor.load(self.search.saved.as_bytes());
                serial::print_str("\n");
                print_prompt(&self.editor, &self.cwd[..self.cwd_len]);
            }

            // Next search result (older)
            Key::CtrlR => {
                let start = self.search.match_age + 1;
                self.search_find(start);
                self.print_search_display();
            }

            // Backspace: shorten query
            Key::Backspace => {
                if self.search.qlen > 0 {
                    self.search.qlen -= 1;
                    self.search.match_age = 0;
                    self.search_find(0);
                    self.print_search_display();
                }
            }

            // Regular character: extend query
            Key::Char(b) => {
                if self.search.qlen < 64 {
                    self.search.query[self.search.qlen] = b;
                    self.search.qlen += 1;
                    self.search.match_age = 0;
                    self.search_find(0);
                    self.print_search_display();
                }
            }

            // Any other key: exit search without accepting
            _ => {
                self.search.reset();
                serial::print_str("\n");
                print_prompt(&self.editor, &self.cwd[..self.cwd_len]);
            }
        }
    }

    fn search_find(&mut self, start_age: usize) {
        let query = &self.search.query[..self.search.qlen];
        if query.is_empty() { return; }
        for age in start_age..self.history.len() {
            if let Some(entry) = self.history.get(age) {
                if contains(entry, query) {
                    self.editor.load(entry);
                    self.search.match_age = age;
                    return;
                }
            }
        }
        // No match — keep current display
    }

    fn print_search_display(&self) {
        let query = &self.search.query[..self.search.qlen];
        // Overwrite current line.
        serial::put_byte(b'\r');
        print_search_prompt(query, Some(self.editor.as_bytes()));
        serial::print_str("\x1b[K");
    }
}

// ── Display helpers ───────────────────────────────────────────────────────────

fn emit_prompt(cwd: &[u8]) {
    serial::print_str("\x1b[1;32mrost\x1b[0m@\x1b[1;34mlocal\x1b[0m:");
    serial::print_str("\x1b[1;37m");
    for &b in cwd { serial::put_byte(b); }
    serial::print_str("\x1b[0m$ ");
}

fn print_prompt(editor: &LineEditor, cwd: &[u8]) {
    serial::put_byte(b'\r');   // ensure cursor is at column 0 before printing prompt
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

fn print_search_prompt(query: &[u8], matched: Option<&[u8]>) {
    serial::print_str("(reverse-i-search)`");
    for &b in query { serial::put_byte(b); }
    serial::print_str("': ");
    if let Some(line) = matched {
        for &b in line { serial::put_byte(b); }
    }
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

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() { return true; }
    if needle.len() > haystack.len() { return false; }
    for i in 0..=(haystack.len() - needle.len()) {
        if &haystack[i..i + needle.len()] == needle { return true; }
    }
    false
}
