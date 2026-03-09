pub const LINE_MAX: usize = 256;

/// In-place line buffer with a movable cursor.
///
/// All operations are O(n) in the worst case (insert/delete shift bytes),
/// which is fine for interactive input of typical line lengths.
pub struct LineEditor {
    buf:    [u8; LINE_MAX],
    pub len:    usize,
    pub cursor: usize,
}

impl LineEditor {
    pub const fn new() -> Self {
        LineEditor { buf: [0; LINE_MAX], len: 0, cursor: 0 }
    }

    /// Insert `b` at the current cursor position, shifting everything right.
    /// Returns `false` if the buffer is full.
    pub fn insert(&mut self, b: u8) -> bool {
        if self.len >= LINE_MAX - 1 { return false; }
        let mut i = self.len;
        while i > self.cursor {
            self.buf[i] = self.buf[i - 1];
            i -= 1;
        }
        self.buf[self.cursor] = b;
        self.cursor += 1;
        self.len += 1;
        true
    }

    /// Delete the character before the cursor (backspace).
    /// Returns `false` if already at the start.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 { return false; }
        self.cursor -= 1;
        let mut i = self.cursor;
        while i < self.len - 1 {
            self.buf[i] = self.buf[i + 1];
            i += 1;
        }
        self.len -= 1;
        true
    }

    /// Delete the character at the cursor (forward delete).
    /// Returns `false` if already at the end.
    pub fn delete_forward(&mut self) -> bool {
        if self.cursor >= self.len { return false; }
        let mut i = self.cursor;
        while i < self.len - 1 {
            self.buf[i] = self.buf[i + 1];
            i += 1;
        }
        self.len -= 1;
        true
    }

    pub fn move_left(&mut self) -> bool {
        if self.cursor == 0 { return false; }
        self.cursor -= 1;
        true
    }

    pub fn move_right(&mut self) -> bool {
        if self.cursor >= self.len { return false; }
        self.cursor += 1;
        true
    }

    pub fn home(&mut self) { self.cursor = 0; }
    pub fn end(&mut self)  { self.cursor = self.len; }

    pub fn clear(&mut self) {
        self.len = 0;
        self.cursor = 0;
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Overwrite buffer contents with `src` and place cursor at the end.
    pub fn load(&mut self, src: &[u8]) {
        let n = src.len().min(LINE_MAX - 1);
        self.buf[..n].copy_from_slice(&src[..n]);
        self.len = n;
        self.cursor = n;
    }

    pub fn is_empty(&self) -> bool { self.len == 0 }
}
