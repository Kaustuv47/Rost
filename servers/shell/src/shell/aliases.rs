//! Fixed-size alias table.
//!
//! Supports `alias name=value`, `unalias name`, and alias expansion at dispatch
//! time (first token lookup).  Pre-populated with common zsh defaults.

pub const MAX_ALIASES: usize = 16;
const NAME_MAX: usize = 32;
const VAL_MAX:  usize = 128;

#[derive(Copy, Clone)]
struct Entry {
    name:     [u8; NAME_MAX],
    val:      [u8; VAL_MAX],
    name_len: u8,
    val_len:  u8,
}

impl Entry {
    const fn empty() -> Self {
        Entry { name: [0; NAME_MAX], val: [0; VAL_MAX], name_len: 0, val_len: 0 }
    }
}

pub struct AliasStore {
    entries: [Entry; MAX_ALIASES],
    count:   usize,
}

impl AliasStore {
    pub const fn new() -> Self {
        AliasStore { entries: [Entry::empty(); MAX_ALIASES], count: 0 }
    }

    /// Pre-populate with sensible zsh-style aliases.
    pub fn init_defaults(&mut self) {
        self.set(b"ll",   b"ls");
        self.set(b"la",   b"ls");
        self.set(b"h",    b"history");
        self.set(b"quit", b"exit");
        self.set(b".",    b"source");
    }

    /// Set or update an alias.  Returns `false` if the store is full.
    pub fn set(&mut self, name: &[u8], val: &[u8]) -> bool {
        let nl = name.len().min(NAME_MAX);
        let vl = val.len().min(VAL_MAX);

        for i in 0..self.count {
            if self.entries[i].name_len as usize == nl
                && self.entries[i].name[..nl] == name[..nl]
            {
                let e = &mut self.entries[i];
                e.val[..vl].copy_from_slice(&val[..vl]);
                for b in &mut e.val[vl..] { *b = 0; }
                e.val_len = vl as u8;
                return true;
            }
        }

        if self.count >= MAX_ALIASES { return false; }
        let e = &mut self.entries[self.count];
        e.name[..nl].copy_from_slice(&name[..nl]);
        e.name_len = nl as u8;
        e.val[..vl].copy_from_slice(&val[..vl]);
        e.val_len = vl as u8;
        self.count += 1;
        true
    }

    pub fn get(&self, name: &[u8]) -> Option<&[u8]> {
        let nl = name.len().min(NAME_MAX);
        for i in 0..self.count {
            if self.entries[i].name_len as usize == nl
                && self.entries[i].name[..nl] == name[..nl]
            {
                let l = self.entries[i].val_len as usize;
                return Some(&self.entries[i].val[..l]);
            }
        }
        None
    }

    pub fn remove(&mut self, name: &[u8]) {
        let nl = name.len().min(NAME_MAX);
        for i in 0..self.count {
            if self.entries[i].name_len as usize == nl
                && self.entries[i].name[..nl] == name[..nl]
            {
                self.count -= 1;
                if i < self.count {
                    self.entries[i] = self.entries[self.count];
                }
                return;
            }
        }
    }

    pub fn count(&self) -> usize { self.count }

    pub fn entry(&self, i: usize) -> Option<(&[u8], &[u8])> {
        if i >= self.count { return None; }
        let e = &self.entries[i];
        Some((&e.name[..e.name_len as usize], &e.val[..e.val_len as usize]))
    }
}
