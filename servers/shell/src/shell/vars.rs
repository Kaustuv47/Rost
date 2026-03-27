//! Fixed-size environment variable store.
//!
//! Provides zsh-style variable assignment and lookup: `$VAR`, `${VAR}`,
//! export/unset.  Pre-populated with sensible defaults at shell startup.

pub const MAX_VARS: usize = 48;
const NAME_MAX: usize = 32;
pub const VAL_MAX: usize = 128;

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

pub struct VarStore {
    entries: [Entry; MAX_VARS],
    count:   usize,
}

impl VarStore {
    pub const fn new() -> Self {
        VarStore { entries: [Entry::empty(); MAX_VARS], count: 0 }
    }

    /// Populate default shell environment variables.
    pub fn init_defaults(&mut self) {
        self.set(b"HOME",     b"/home/user");
        self.set(b"USER",     b"rost");
        self.set(b"SHELL",    b"/bin/rost-shell");
        self.set(b"HOSTNAME", b"local");
        self.set(b"TERM",     b"xterm-256color");
        self.set(b"PATH",     b"/bin:/usr/bin");
        self.set(b"PWD",      b"/");
        self.set(b"OLDPWD",   b"/");
        self.set(b"IFS",      b" \t\n");
        self.set(b"LANG",     b"en_US.UTF-8");
    }

    /// Set or update a variable.  Returns `false` if the store is full.
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

        if self.count >= MAX_VARS { return false; }
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

    pub fn unset(&mut self, name: &[u8]) {
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
