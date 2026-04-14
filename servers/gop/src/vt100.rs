//! VT100 / ANSI escape sequence parser for the GOP framebuffer terminal.
//!
//! Implements a subset of VT100 + ANSI X3.64 sufficient for the Rost shell:
//!   - CSI cursor movement: A/B/C/D (up/down/right/left), H/f (position), G (column)
//!   - CSI erase: J (display), K (line)
//!   - CSI SGR (m): attributes + 16-color foreground/background
//!   - ESC 7/8: save/restore cursor
//!   - Control chars: CR, LF, BS, HT, BEL
//!
//! # State machine
//!
//! ```text
//!   Normal  ──\033──►  Esc  ──[──►  Csi
//!     ▲                              │
//!     └──────────────────────────────┘  (on final byte or unknown)
//! ```

// ── 16-color ANSI palette (0xAA_RR_GG_BB, α ignored by renderer) ─────────────
//
// Standard VGA/xterm color set used by most terminal emulators.

pub const PALETTE: [u32; 16] = [
    0xFF_00_00_00, // 0  black
    0xFF_80_00_00, // 1  dark red
    0xFF_00_80_00, // 2  dark green
    0xFF_80_80_00, // 3  dark yellow / olive
    0xFF_00_00_80, // 4  dark blue
    0xFF_80_00_80, // 5  dark magenta
    0xFF_00_80_80, // 6  dark cyan
    0xFF_C0_C0_C0, // 7  light grey
    0xFF_80_80_80, // 8  dark grey
    0xFF_FF_55_55, // 9  bright red
    0xFF_55_FF_55, // 10 bright green
    0xFF_FF_FF_55, // 11 bright yellow
    0xFF_55_55_FF, // 12 bright blue
    0xFF_FF_55_FF, // 13 bright magenta
    0xFF_55_FF_FF, // 14 bright cyan
    0xFF_FF_FF_FF, // 15 white
];

pub const DEFAULT_FG: u8 = 7;  // light grey
pub const DEFAULT_BG: u8 = 0;  // black

// ── Parser state ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum State {
    Normal,
    Esc,
    Csi,
}

pub struct Vt100 {
    state:    State,
    params:   [u32; 8],
    nparam:   usize,
    flag:     u8,    // e.g. '?' for DEC private mode
    // Current text attributes
    pub fg:      u8,
    pub bg:      u8,
    bold:     bool,
    reverse:  bool,
    // Saved cursor position
    saved_col: u32,
    saved_row: u32,
}

impl Vt100 {
    pub const fn new() -> Self {
        Vt100 {
            state:     State::Normal,
            params:    [0u32; 8],
            nparam:    0,
            flag:      0,
            fg:        DEFAULT_FG,
            bg:        DEFAULT_BG,
            bold:      false,
            reverse:   false,
            saved_col: 0,
            saved_row: 0,
        }
    }

    /// Feed one byte to the parser.  Calls the provided callbacks.
    pub fn feed<F>(&mut self, b: u8, cb: &mut F)
    where
        F: FnMut(Vt100Event),
    {
        match self.state {
            State::Normal => self.normal(b, cb),
            State::Esc    => self.escape(b, cb),
            State::Csi    => self.csi_collect(b, cb),
        }
    }

    // ── Normal ────────────────────────────────────────────────────────────────

    fn normal<F: FnMut(Vt100Event)>(&mut self, b: u8, cb: &mut F) {
        match b {
            0x1B => { self.state = State::Esc; }
            b'\r' => cb(Vt100Event::CarriageReturn),
            b'\n' => cb(Vt100Event::LineFeed),
            b'\t' => cb(Vt100Event::Tab),
            0x08  => cb(Vt100Event::Backspace),
            0x07  => {} // BEL — ignore
            _     => cb(Vt100Event::Char(b)),
        }
    }

    // ── ESC byte ─────────────────────────────────────────────────────────────

    fn escape<F: FnMut(Vt100Event)>(&mut self, b: u8, cb: &mut F) {
        match b {
            b'[' => {
                // Begin CSI sequence
                self.state  = State::Csi;
                self.params = [0u32; 8];
                self.nparam = 0;
                self.flag   = 0;
            }
            b'7' => { // DECSC — save cursor
                self.saved_col = 0; // caller must have current cursor
                self.saved_row = 0;
                cb(Vt100Event::SaveCursor);
                self.state = State::Normal;
            }
            b'8' => { // DECRC — restore cursor
                cb(Vt100Event::RestoreCursor);
                self.state = State::Normal;
            }
            b'c' => { // RIS — full reset
                cb(Vt100Event::Reset);
                self.state = State::Normal;
            }
            _ => { self.state = State::Normal; } // unknown ESC sequence
        }
    }

    // ── CSI parameter collection ──────────────────────────────────────────────

    fn csi_collect<F: FnMut(Vt100Event)>(&mut self, b: u8, cb: &mut F) {
        match b {
            b'0'..=b'9' => {
                // Accumulate digit into current parameter
                if self.nparam == 0 { self.nparam = 1; }
                let idx = self.nparam - 1;
                if idx < 8 {
                    self.params[idx] = self.params[idx].saturating_mul(10)
                        .saturating_add((b - b'0') as u32);
                }
            }
            b';' => {
                // Parameter separator
                if self.nparam < 8 { self.nparam += 1; }
                if self.nparam < 8 { self.params[self.nparam] = 0; }
            }
            b'?' | b'>' | b'!' => {
                self.flag = b;
            }
            b'A'..=b'Z' | b'a'..=b'z' | b'@' | b'`' => {
                // Final byte — dispatch
                self.dispatch(b, cb);
                self.state = State::Normal;
            }
            _ => {
                // Other intermediate bytes — ignore and stay in CSI
            }
        }
    }

    // ── CSI dispatch ─────────────────────────────────────────────────────────

    fn p(&self, i: usize, default: u32) -> u32 {
        if i < self.nparam && self.params[i] != 0 { self.params[i] } else { default }
    }

    fn dispatch<F: FnMut(Vt100Event)>(&mut self, final_byte: u8, cb: &mut F) {
        match final_byte {
            // ── Cursor movement ───────────────────────────────────────────────
            b'A' => cb(Vt100Event::CursorUp(self.p(0, 1))),
            b'B' => cb(Vt100Event::CursorDown(self.p(0, 1))),
            b'C' => cb(Vt100Event::CursorRight(self.p(0, 1))),
            b'D' => cb(Vt100Event::CursorLeft(self.p(0, 1))),
            b'E' => cb(Vt100Event::CursorNextLine(self.p(0, 1))),
            b'F' => cb(Vt100Event::CursorPrevLine(self.p(0, 1))),
            b'G' => cb(Vt100Event::CursorColumn(self.p(0, 1).saturating_sub(1))),
            b'H' | b'f' => {
                let row = self.p(0, 1).saturating_sub(1);
                let col = self.p(1, 1).saturating_sub(1);
                cb(Vt100Event::CursorPosition(row, col));
            }
            // ── Save/restore cursor ───────────────────────────────────────────
            b's' => cb(Vt100Event::SaveCursor),
            b'u' => cb(Vt100Event::RestoreCursor),
            // ── Erase ─────────────────────────────────────────────────────────
            b'J' => cb(Vt100Event::EraseDisplay(self.p(0, 0))),
            b'K' => cb(Vt100Event::EraseLine(self.p(0, 0))),
            b'P' => cb(Vt100Event::DeleteChars(self.p(0, 1))),
            // ── Scroll ────────────────────────────────────────────────────────
            b'S' => cb(Vt100Event::ScrollUp(self.p(0, 1))),
            b'T' => cb(Vt100Event::ScrollDown(self.p(0, 1))),
            // ── SGR (Select Graphic Rendition) ────────────────────────────────
            b'm' => {
                let n = if self.nparam == 0 { 1 } else { self.nparam };
                let mut i = 0;
                while i < n {
                    self.apply_sgr(self.params[i], cb);
                    i += 1;
                }
            }
            // ── DEC private mode (ignore) ─────────────────────────────────────
            b'h' | b'l' => { /* show/hide cursor, alternate screen etc — ignore */ }
            // ── Ignored sequences ─────────────────────────────────────────────
            _ => {}
        }
    }

    fn apply_sgr<F: FnMut(Vt100Event)>(&mut self, code: u32, cb: &mut F) {
        match code {
            0 => {
                self.fg      = DEFAULT_FG;
                self.bg      = DEFAULT_BG;
                self.bold    = false;
                self.reverse = false;
                cb(Vt100Event::SetFg(PALETTE[DEFAULT_FG as usize]));
                cb(Vt100Event::SetBg(PALETTE[DEFAULT_BG as usize]));
            }
            1 => { // bold / bright
                self.bold = true;
                let fg = if self.fg < 8 { self.fg + 8 } else { self.fg };
                cb(Vt100Event::SetFg(PALETTE[fg as usize]));
            }
            2 | 22 => { // dim / normal intensity — ignore
                self.bold = false;
            }
            7 => { // reverse video
                self.reverse = true;
                cb(Vt100Event::SetFg(PALETTE[self.bg as usize]));
                cb(Vt100Event::SetBg(PALETTE[self.fg as usize]));
            }
            27 => { // un-reverse
                self.reverse = false;
                cb(Vt100Event::SetFg(PALETTE[self.fg as usize]));
                cb(Vt100Event::SetBg(PALETTE[self.bg as usize]));
            }
            30..=37 => { // standard foreground
                let idx = (code - 30) as u8 + if self.bold { 8 } else { 0 };
                self.fg = idx.min(15);
                cb(Vt100Event::SetFg(PALETTE[self.fg as usize]));
            }
            38 => { /* 256-color / truecolor — ignored */ }
            39 => { // default foreground
                self.fg   = DEFAULT_FG;
                self.bold = false;
                cb(Vt100Event::SetFg(PALETTE[DEFAULT_FG as usize]));
            }
            40..=47 => { // standard background
                let idx = (code - 40) as u8;
                self.bg = idx;
                cb(Vt100Event::SetBg(PALETTE[self.bg as usize]));
            }
            48 => { /* 256-color / truecolor bg — ignored */ }
            49 => { // default background
                self.bg = DEFAULT_BG;
                cb(Vt100Event::SetBg(PALETTE[DEFAULT_BG as usize]));
            }
            90..=97 => { // bright foreground
                let idx = (code - 90 + 8) as u8;
                self.fg = idx.min(15);
                cb(Vt100Event::SetFg(PALETTE[self.fg as usize]));
            }
            100..=107 => { // bright background
                let idx = (code - 100 + 8) as u8;
                self.bg = idx.min(15);
                cb(Vt100Event::SetBg(PALETTE[self.bg as usize]));
            }
            _ => {} // unrecognised SGR — ignore
        }
    }
}

// ── Events emitted by the VT100 parser ───────────────────────────────────────

pub enum Vt100Event {
    Char(u8),
    CarriageReturn,
    LineFeed,
    Tab,
    Backspace,
    CursorUp(u32),
    CursorDown(u32),
    CursorRight(u32),
    CursorLeft(u32),
    CursorNextLine(u32),
    CursorPrevLine(u32),
    CursorColumn(u32),
    CursorPosition(u32, u32), // (row, col) 0-based
    SaveCursor,
    RestoreCursor,
    EraseDisplay(u32), // 0=below, 1=above, 2=whole, 3=scrollback
    EraseLine(u32),    // 0=right, 1=left, 2=whole
    DeleteChars(u32),
    ScrollUp(u32),
    ScrollDown(u32),
    SetFg(u32),        // ARGB color
    SetBg(u32),        // ARGB color
    Reset,
}
