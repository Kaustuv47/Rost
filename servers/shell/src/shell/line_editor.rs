//! In-place line buffer with emacs-style editing operations.
//!
//! Supports all readline / zle keybindings:
//!   cursor movement, character/word deletion, kill ring, yank.

pub const LINE_MAX: usize = 256;

/// In-place line buffer with a movable cursor.
pub struct LineEditor {
    buf:    [u8; LINE_MAX],
    pub len:    usize,
    pub cursor: usize,
}

impl LineEditor {
    pub const fn new() -> Self {
        LineEditor { buf: [0; LINE_MAX], len: 0, cursor: 0 }
    }

    // ── Insert / delete ───────────────────────────────────────────────────────

    /// Insert `b` at the current cursor position.  Returns `false` if full.
    pub fn insert(&mut self, b: u8) -> bool {
        if self.len >= LINE_MAX - 1 { return false; }
        let mut i = self.len;
        while i > self.cursor { self.buf[i] = self.buf[i - 1]; i -= 1; }
        self.buf[self.cursor] = b;
        self.cursor += 1;
        self.len += 1;
        true
    }

    /// Delete the character before the cursor (Backspace).
    /// Returns `false` if already at the start.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 { return false; }
        self.cursor -= 1;
        let mut i = self.cursor;
        while i < self.len - 1 { self.buf[i] = self.buf[i + 1]; i += 1; }
        self.len -= 1;
        true
    }

    /// Delete the character at the cursor (Delete key).
    /// Returns `false` if already at the end.
    pub fn delete_forward(&mut self) -> bool {
        if self.cursor >= self.len { return false; }
        let mut i = self.cursor;
        while i < self.len - 1 { self.buf[i] = self.buf[i + 1]; i += 1; }
        self.len -= 1;
        true
    }

    // ── Kill / yank ───────────────────────────────────────────────────────────

    /// Kill from cursor to end of line (Ctrl+K).
    /// Writes killed text into `ring`, returns the number of bytes killed.
    pub fn kill_to_end(&mut self, ring: &mut [u8; LINE_MAX]) -> usize {
        let n = self.len - self.cursor;
        ring[..n].copy_from_slice(&self.buf[self.cursor..self.len]);
        self.len = self.cursor;
        n
    }

    /// Kill from beginning of line to cursor (Ctrl+U).
    /// Writes killed text into `ring`, returns bytes killed.
    pub fn kill_to_start(&mut self, ring: &mut [u8; LINE_MAX]) -> usize {
        let n = self.cursor;
        ring[..n].copy_from_slice(&self.buf[..self.cursor]);
        // Shift remaining content to the front.
        let rem = self.len - self.cursor;
        for i in 0..rem { self.buf[i] = self.buf[self.cursor + i]; }
        self.len = rem;
        self.cursor = 0;
        n
    }

    /// Kill the word immediately before the cursor (Ctrl+W).
    /// A "word" is a run of non-space characters.
    /// Writes killed text into `ring`, returns bytes killed.
    pub fn kill_prev_word(&mut self, ring: &mut [u8; LINE_MAX]) -> usize {
        // Skip trailing spaces.
        let mut end = self.cursor;
        while end > 0 && self.buf[end - 1] == b' ' { end -= 1; }
        // Skip the word characters.
        let mut start = end;
        while start > 0 && self.buf[start - 1] != b' ' { start -= 1; }

        let n = end - start;
        ring[..n].copy_from_slice(&self.buf[start..end]);

        // Remove [start..cursor] from the buffer.
        let removed = self.cursor - start;
        let rem = self.len - self.cursor;
        for i in 0..rem { self.buf[start + i] = self.buf[self.cursor + i]; }
        self.len -= removed;
        self.cursor = start;
        n
    }

    /// Yank `data[..len]` at the current cursor position (Ctrl+Y).
    pub fn yank(&mut self, data: &[u8], len: usize) -> bool {
        if len == 0 { return true; }
        if self.len + len >= LINE_MAX { return false; }
        // Make room.
        let mut i = self.len + len - 1;
        while i >= self.cursor + len {
            self.buf[i] = self.buf[i - len];
            if i == 0 { break; }
            i -= 1;
        }
        self.buf[self.cursor..self.cursor + len].copy_from_slice(&data[..len]);
        self.cursor += len;
        self.len += len;
        true
    }

    // ── Cursor movement ───────────────────────────────────────────────────────

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

    /// Move cursor left by one word (Alt+B).  Returns number of positions moved.
    pub fn word_left(&mut self) -> usize {
        let start = self.cursor;
        // Skip spaces.
        while self.cursor > 0 && self.buf[self.cursor - 1] == b' ' { self.cursor -= 1; }
        // Skip word characters.
        while self.cursor > 0 && self.buf[self.cursor - 1] != b' ' { self.cursor -= 1; }
        start - self.cursor
    }

    /// Move cursor right by one word (Alt+F).  Returns number of positions moved.
    pub fn word_right(&mut self) -> usize {
        let start = self.cursor;
        // Skip spaces.
        while self.cursor < self.len && self.buf[self.cursor] == b' ' { self.cursor += 1; }
        // Skip word characters.
        while self.cursor < self.len && self.buf[self.cursor] != b' ' { self.cursor += 1; }
        self.cursor - start
    }

    // ── Buffer management ─────────────────────────────────────────────────────

    pub fn clear(&mut self) {
        self.len = 0;
        self.cursor = 0;
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Overwrite buffer with `src` and place cursor at the end.
    pub fn load(&mut self, src: &[u8]) {
        let n = src.len().min(LINE_MAX - 1);
        self.buf[..n].copy_from_slice(&src[..n]);
        self.len = n;
        self.cursor = n;
    }

    pub fn is_empty(&self) -> bool { self.len == 0 }
}
