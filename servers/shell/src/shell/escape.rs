//! ANSI/VT100 escape sequence parser.
//!
//! Covers all emacs-style (readline / zle) keybindings:
//!   Ctrl+A/B/C/D/E/F/K/L/N/P/R/U/W/Y
//!   Alt+B / Alt+F (word movement)
//!   Arrow keys, Home, End, Delete

/// Decoded key event — produced by `EscapeParser::feed()`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    // Printable
    Char(u8),

    // Basic editing
    Enter,
    Backspace,
    Delete,
    Tab,

    // Cursor movement
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,

    // Word movement (Alt+B / Alt+F  or  Ctrl+Left / Ctrl+Right)
    WordLeft,
    WordRight,

    // Emacs control keys
    CtrlA,  // beginning-of-line
    CtrlB,  // backward-char (= ArrowLeft)
    CtrlC,  // interrupt
    CtrlD,  // delete-char-or-eof
    CtrlE,  // end-of-line
    CtrlF,  // forward-char  (= ArrowRight)
    CtrlK,  // kill-to-end-of-line
    CtrlL,  // clear-screen
    CtrlN,  // next-history   (= ArrowDown)
    CtrlP,  // prev-history   (= ArrowUp)
    CtrlR,  // reverse-isearch
    CtrlU,  // kill-to-beginning-of-line
    CtrlW,  // kill-previous-word
    CtrlY,  // yank
}

#[derive(Clone, Copy)]
enum State {
    Normal,
    Esc,
    EscBracket,
    EscBracketParam(u8),
    EscO,              // SS3 sequences  (e.g. rxvt Home/End)
    EscBracket1Sc,     // ESC [ 1 ; — modifier sequences (Ctrl+Arrow)
    EscBracket1ScN(u8),// ESC [ 1 ; <modifier>
}

/// Stateful ANSI/VT220 escape sequence parser.
pub struct EscapeParser {
    state:   State,
    last_cr: bool,  // suppress the LF half of a CRLF pair
}

impl EscapeParser {
    pub const fn new() -> Self {
        EscapeParser { state: State::Normal, last_cr: false }
    }

    pub fn feed(&mut self, byte: u8) -> Option<Key> {
        match self.state {
            // ── Normal ────────────────────────────────────────────────────────
            State::Normal => match byte {
                0x1B        => { self.last_cr = false; self.state = State::Esc; None }
                b'\r'       => { self.last_cr = true;  Some(Key::Enter) }
                b'\n'       => {
                    // Absorb LF that is the second byte of a CRLF pair.
                    if self.last_cr { self.last_cr = false; return None; }
                    Some(Key::Enter)
                }
                b => {
                    self.last_cr = false;
                    match b {
                        0x08|0x7F   => Some(Key::Backspace),
                        b'\t'       => Some(Key::Tab),
                        0x01        => Some(Key::CtrlA),
                        0x02        => Some(Key::CtrlB),
                        0x03        => Some(Key::CtrlC),
                        0x04        => Some(Key::CtrlD),
                        0x05        => Some(Key::CtrlE),
                        0x06        => Some(Key::CtrlF),
                        0x0B        => Some(Key::CtrlK),
                        0x0C        => Some(Key::CtrlL),
                        0x0E        => Some(Key::CtrlN),
                        0x10        => Some(Key::CtrlP),
                        0x12        => Some(Key::CtrlR),
                        0x15        => Some(Key::CtrlU),
                        0x17        => Some(Key::CtrlW),
                        0x19        => Some(Key::CtrlY),
                        b if b >= 0x20 => Some(Key::Char(b)),
                        _ => None,
                    }
                }
            },

            // ── ESC ───────────────────────────────────────────────────────────
            State::Esc => match byte {
                b'['  => { self.state = State::EscBracket; None }
                b'O'  => { self.state = State::EscO; None }
                b'b'  => { self.state = State::Normal; Some(Key::WordLeft) }
                b'f'  => { self.state = State::Normal; Some(Key::WordRight) }
                b'B'  => { self.state = State::Normal; Some(Key::WordLeft) }
                b'F'  => { self.state = State::Normal; Some(Key::WordRight) }
                _ => { self.state = State::Normal; None }
            },

            // ── ESC [ ─────────────────────────────────────────────────────────
            State::EscBracket => {
                self.state = State::Normal;
                match byte {
                    b'A' => Some(Key::ArrowUp),
                    b'B' => Some(Key::ArrowDown),
                    b'C' => Some(Key::ArrowRight),
                    b'D' => Some(Key::ArrowLeft),
                    b'H' => Some(Key::Home),
                    b'F' => Some(Key::End),
                    b'1'..=b'9' => {
                        if byte == b'1' {
                            self.state = State::EscBracket1Sc;
                        } else {
                            self.state = State::EscBracketParam(byte - b'0');
                        }
                        None
                    }
                    _ => None,
                }
            },

            // ── ESC [ 1 ; ────────────────────────────────────────────────────
            State::EscBracket1Sc => {
                self.state = State::Normal;
                match byte {
                    b'~' => Some(Key::Home),       // ESC [ 1 ~  (VT220 home)
                    b';' => { self.state = State::EscBracket1ScN(0); None }
                    _ => None,
                }
            }

            // ── ESC [ 1 ; <modifier> ─────────────────────────────────────────
            State::EscBracket1ScN(_m) => {
                self.state = State::Normal;
                match byte {
                    b'C' => Some(Key::WordRight),   // Ctrl+Right
                    b'D' => Some(Key::WordLeft),    // Ctrl+Left
                    _ => None,
                }
            }

            // ── ESC [ <n> ~ ───────────────────────────────────────────────────
            State::EscBracketParam(n) => {
                self.state = State::Normal;
                if byte == b'~' {
                    match n {
                        1 | 7 => Some(Key::Home),
                        3     => Some(Key::Delete),
                        4 | 8 => Some(Key::End),
                        _     => None,
                    }
                } else {
                    None
                }
            },

            // ── ESC O (SS3 — used by some terminals for arrows) ───────────────
            State::EscO => {
                self.state = State::Normal;
                match byte {
                    b'A' => Some(Key::ArrowUp),
                    b'B' => Some(Key::ArrowDown),
                    b'C' => Some(Key::ArrowRight),
                    b'D' => Some(Key::ArrowLeft),
                    b'H' => Some(Key::Home),
                    b'F' => Some(Key::End),
                    _ => None,
                }
            },
        }
    }
}
