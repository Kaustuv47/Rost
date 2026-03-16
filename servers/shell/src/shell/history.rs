use super::line_editor::LINE_MAX;

pub const HISTORY_CAP: usize = 32;

/// Fixed-capacity circular command history.
///
/// Entries are indexed by *age*: `get(0)` is the most recent entry,
/// `get(1)` is second most recent, and so on.
pub struct History {
    entries: [[u8; LINE_MAX]; HISTORY_CAP],
    lens:    [usize; HISTORY_CAP],
    count:   usize,   // number of valid entries (saturates at HISTORY_CAP)
    next:    usize,   // slot where the *next* push will write
}

impl History {
    pub const fn new() -> Self {
        History {
            entries: [[0u8; LINE_MAX]; HISTORY_CAP],
            lens:    [0usize; HISTORY_CAP],
            count:   0,
            next:    0,
        }
    }

    /// Push a new entry. Empty lines and consecutive duplicates are ignored.
    pub fn push(&mut self, line: &[u8]) {
        if line.is_empty() { return; }
        if self.count > 0 && self.get(0) == Some(line) { return; }

        let n = line.len().min(LINE_MAX);
        self.entries[self.next][..n].copy_from_slice(&line[..n]);
        self.lens[self.next] = n;
        self.next = (self.next + 1) % HISTORY_CAP;
        if self.count < HISTORY_CAP { self.count += 1; }
    }

    /// Retrieve an entry by age. Returns `None` if `age >= self.len()`.
    pub fn get(&self, age: usize) -> Option<&[u8]> {
        if age >= self.count { return None; }
        // `next - 1` is the most recent slot; subtract age from there
        let idx = (self.next + HISTORY_CAP - 1 - age) % HISTORY_CAP;
        Some(&self.entries[idx][..self.lens[idx]])
    }

    pub fn len(&self) -> usize { self.count }
    pub fn is_empty(&self) -> bool { self.count == 0 }
}
