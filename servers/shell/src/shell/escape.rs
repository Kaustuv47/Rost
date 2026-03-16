/// Decoded key event — produced by `EscapeParser::feed()`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(u8),
    Enter,
    Backspace,
    Delete,
    Tab,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    CtrlC,
    CtrlL,
}

#[derive(Clone, Copy)]
enum State {
    Normal,
    Esc,
    EscBracket,
    EscParam(u8),
}

/// Stateful ANSI escape sequence parser.
///
/// Feed it one raw byte at a time from the serial port.
/// Returns `Some(Key)` when a complete key event is recognised.
pub struct EscapeParser {
    state: State,
}

impl EscapeParser {
    pub const fn new() -> Self {
        EscapeParser { state: State::Normal }
    }

    pub fn feed(&mut self, byte: u8) -> Option<Key> {
        match self.state {
            State::Normal => match byte {
                0x1B        => { self.state = State::Esc; None }
                b'\r'|b'\n' => Some(Key::Enter),
                0x08|0x7F   => Some(Key::Backspace),
                b'\t'       => Some(Key::Tab),
                0x03        => Some(Key::CtrlC),
                0x0C        => Some(Key::CtrlL),
                b if b >= 0x20 => Some(Key::Char(b)),
                _ => None,
            },

            State::Esc => match byte {
                b'[' => { self.state = State::EscBracket; None }
                _    => { self.state = State::Normal; None }
            },

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
                        self.state = State::EscParam(byte - b'0');
                        None
                    }
                    _ => None,
                }
            },

            // ESC [ <n> ~
            State::EscParam(n) => {
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
        }
    }
}
