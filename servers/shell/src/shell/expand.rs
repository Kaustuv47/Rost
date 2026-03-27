//! Word expansion pipeline.
//!
//! Applied to every input line before dispatching, in this order:
//!   1. History expansion  (`!!`, `!n`, `!-n`, `!prefix`)
//!   2. Variable expansion (`$VAR`, `${VAR}`, `$$`, `$?`, `$0`, `~`)
//!
//! All operations work on fixed-size byte slices — no heap required.

use super::history::History;
use super::line_editor::LINE_MAX;
use super::vars::VarStore;

// ── History expansion ─────────────────────────────────────────────────────────

/// Expand history references in `input` and write the result to `out`.
///
/// Returns the new length.  If nothing was expanded the returned length is 0
/// and the caller should use the original `input`.
pub fn expand_history(
    input:   &[u8],
    history: &History,
    out:     &mut [u8; LINE_MAX],
) -> usize {
    let mut i = 0;
    let mut o = 0;
    let mut did_expand = false;

    while i < input.len() {
        if input[i] != b'!' {
            if o < LINE_MAX - 1 { out[o] = input[i]; o += 1; }
            i += 1;
            continue;
        }

        i += 1; // skip !
        if i >= input.len() {
            if o < LINE_MAX - 1 { out[o] = b'!'; o += 1; }
            break;
        }

        match input[i] {
            // !! — repeat last command
            b'!' => {
                i += 1;
                if let Some(e) = history.get(0) {
                    did_expand = true;
                    for &b in e { if o < LINE_MAX - 1 { out[o] = b; o += 1; } }
                } else {
                    if o < LINE_MAX - 1 { out[o] = b'!'; o += 1; }
                    if o < LINE_MAX - 1 { out[o] = b'!'; o += 1; }
                }
            }

            // !-n — nth from end (1-based)
            b'-' => {
                i += 1;
                let start = i;
                while i < input.len() && input[i].is_ascii_digit() { i += 1; }
                let n = parse_uint(&input[start..i]) as usize;
                if n > 0 {
                    if let Some(e) = history.get(n - 1) {
                        did_expand = true;
                        for &b in e { if o < LINE_MAX - 1 { out[o] = b; o += 1; } }
                    }
                }
            }

            // !n — absolute index (1-based from oldest)
            b if b.is_ascii_digit() => {
                let start = i;
                while i < input.len() && input[i].is_ascii_digit() { i += 1; }
                let n = parse_uint(&input[start..i]) as usize;
                let len = history.len();
                if n > 0 && n <= len {
                    if let Some(e) = history.get(len - n) {
                        did_expand = true;
                        for &b in e { if o < LINE_MAX - 1 { out[o] = b; o += 1; } }
                    }
                }
            }

            // !prefix — last command starting with prefix
            _ => {
                let start = i;
                while i < input.len() && input[i] != b' ' && input[i] != b';' { i += 1; }
                let prefix = &input[start..i];
                let mut found = false;
                for age in 0..history.len() {
                    if let Some(e) = history.get(age) {
                        if e.starts_with(prefix) {
                            did_expand = true;
                            found = true;
                            for &b in e { if o < LINE_MAX - 1 { out[o] = b; o += 1; } }
                            break;
                        }
                    }
                }
                if !found {
                    if o < LINE_MAX - 1 { out[o] = b'!'; o += 1; }
                    for &b in prefix { if o < LINE_MAX - 1 { out[o] = b; o += 1; } }
                }
            }
        }
    }

    if o < LINE_MAX { out[o] = 0; }
    if did_expand { o } else { 0 }
}

// ── Variable expansion ────────────────────────────────────────────────────────

/// Expand `$VAR`, `${VAR}`, `$$`, `$?`, `$0`, and leading `~` in `input`.
///
/// Always returns the new length written to `out`.
pub fn expand_vars(
    input:      &[u8],
    vars:       &VarStore,
    pid:        u32,
    last_exit:  u64,
    out:        &mut [u8; LINE_MAX],
) -> usize {
    let mut i = 0;
    let mut o = 0;

    while i < input.len() && o < LINE_MAX - 1 {
        let b = input[i];

        // Tilde expansion: ~ at start of word
        if b == b'~' && (i == 0 || input[i - 1] == b' ') {
            let home = vars.get(b"HOME").unwrap_or(b"/home/user");
            for &h in home { if o < LINE_MAX - 1 { out[o] = h; o += 1; } }
            i += 1;
            continue;
        }

        if b != b'$' {
            out[o] = b;
            o += 1;
            i += 1;
            continue;
        }

        i += 1; // skip $
        if i >= input.len() { break; }

        match input[i] {
            b'$' => {
                // $$ — shell PID
                let n = emit_u64(out, o, pid as u64);
                o += n; i += 1;
            }
            b'?' => {
                // $? — last exit code
                let n = emit_u64(out, o, last_exit);
                o += n; i += 1;
            }
            b'0' => {
                // $0 — shell name
                for &c in b"rost-shell" { if o < LINE_MAX - 1 { out[o] = c; o += 1; } }
                i += 1;
            }
            b'{' => {
                // ${VAR} — explicit-braces form
                i += 1;
                let start = i;
                while i < input.len() && input[i] != b'}' { i += 1; }
                let name = &input[start..i];
                if i < input.len() { i += 1; } // skip }
                if let Some(v) = vars.get(name) {
                    for &c in v { if o < LINE_MAX - 1 { out[o] = c; o += 1; } }
                }
            }
            c if is_id_char(c) => {
                // $VAR — bare name: read until non-identifier byte
                let start = i;
                while i < input.len() && is_id_char(input[i]) { i += 1; }
                let name = &input[start..i];
                if let Some(v) = vars.get(name) {
                    for &c in v { if o < LINE_MAX - 1 { out[o] = c; o += 1; } }
                }
            }
            _ => {
                // $ not followed by a valid sigil — emit literally
                if o < LINE_MAX - 1 { out[o] = b'$'; o += 1; }
            }
        }
    }

    if o < LINE_MAX { out[o] = 0; }
    o
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn is_id_char(b: u8) -> bool {
    matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_')
}

/// Write decimal representation of `n` starting at `out[start]`.
/// Returns number of bytes written.
fn emit_u64(out: &mut [u8; LINE_MAX], start: usize, mut n: u64) -> usize {
    if n == 0 {
        if start < LINE_MAX - 1 { out[start] = b'0'; return 1; }
        return 0;
    }
    let mut buf = [0u8; 20];
    let mut pos = 20usize;
    while n > 0 { pos -= 1; buf[pos] = b'0' + (n % 10) as u8; n /= 10; }
    let len = 20 - pos;
    let avail = (LINE_MAX - 1).saturating_sub(start);
    let n = len.min(avail);
    out[start..start + n].copy_from_slice(&buf[pos..pos + n]);
    n
}

fn parse_uint(s: &[u8]) -> u64 {
    let mut n = 0u64;
    for &b in s {
        if b.is_ascii_digit() { n = n.saturating_mul(10).saturating_add((b - b'0') as u64); }
        else { break; }
    }
    n
}
