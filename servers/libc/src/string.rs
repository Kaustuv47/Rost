//! Memory and string utility functions (C string.h subset).
//!
//! All functions follow the POSIX / C standard interface exactly.  Rust
//! callers are encouraged to use the safe wrappers where available.

// ── Memory functions ──────────────────────────────────────────────────────────

/// Copy `n` bytes from `src` to `dst`.  Regions must not overlap; use
/// `memmove` when they might.  Returns `dst`.
#[no_mangle]
pub unsafe extern "C" fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    core::ptr::copy_nonoverlapping(src, dst, n);
    dst
}

/// Copy `n` bytes from `src` to `dst`, handling overlapping regions correctly.
/// Returns `dst`.
#[no_mangle]
pub unsafe extern "C" fn memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    core::ptr::copy(src, dst, n);
    dst
}

/// Fill `n` bytes of `dst` with the byte value `c`.  Returns `dst`.
#[no_mangle]
pub unsafe extern "C" fn memset(dst: *mut u8, c: i32, n: usize) -> *mut u8 {
    core::ptr::write_bytes(dst, c as u8, n);
    dst
}

/// Compare `n` bytes of `a` and `b`.
/// Returns 0 if equal, negative if `a < b`, positive if `a > b`.
#[no_mangle]
pub unsafe extern "C" fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    for i in 0..n {
        let diff = *a.add(i) as i32 - *b.add(i) as i32;
        if diff != 0 { return diff; }
    }
    0
}

/// Find the first occurrence of byte `c` in the first `n` bytes of `s`.
/// Returns a pointer to the match or null if not found.
#[no_mangle]
pub unsafe extern "C" fn memchr(s: *const u8, c: i32, n: usize) -> *const u8 {
    let byte = c as u8;
    for i in 0..n {
        if *s.add(i) == byte { return s.add(i); }
    }
    core::ptr::null()
}

// ── String functions ──────────────────────────────────────────────────────────

/// Return the length of the null-terminated string `s` (not counting `\0`).
#[no_mangle]
pub unsafe extern "C" fn strlen(s: *const i8) -> usize {
    let mut n = 0usize;
    while *s.add(n) != 0 { n += 1; }
    n
}

/// Copy null-terminated string `src` to `dst`.  Returns `dst`.
/// SAFETY: `dst` must have room for `strlen(src) + 1` bytes.
#[no_mangle]
pub unsafe extern "C" fn strcpy(dst: *mut i8, src: *const i8) -> *mut i8 {
    let mut i = 0usize;
    loop {
        let b = *src.add(i);
        *dst.add(i) = b;
        if b == 0 { break; }
        i += 1;
    }
    dst
}

/// Copy at most `n` bytes from `src` to `dst`, zero-padding to `n` if
/// `src` is shorter.  Returns `dst`.
#[no_mangle]
pub unsafe extern "C" fn strncpy(dst: *mut i8, src: *const i8, n: usize) -> *mut i8 {
    for i in 0..n {
        let b = if i < n { *src.add(i) } else { 0 };
        *dst.add(i) = b;
        if b == 0 {
            // Zero-fill the rest.
            for j in (i + 1)..n { *dst.add(j) = 0; }
            break;
        }
    }
    dst
}

/// Append null-terminated `src` to the end of `dst`.  Returns `dst`.
#[no_mangle]
pub unsafe extern "C" fn strcat(dst: *mut i8, src: *const i8) -> *mut i8 {
    let offset = strlen(dst);
    strcpy(dst.add(offset), src);
    dst
}

/// Append at most `n` bytes of `src` to `dst`, always null-terminating.
/// Returns `dst`.
#[no_mangle]
pub unsafe extern "C" fn strncat(dst: *mut i8, src: *const i8, n: usize) -> *mut i8 {
    let dst_len = strlen(dst);
    let src_len = strlen(src).min(n);
    core::ptr::copy_nonoverlapping(src as *const u8, dst.add(dst_len) as *mut u8, src_len);
    *dst.add(dst_len + src_len) = 0;
    dst
}

/// Lexicographic comparison of null-terminated strings.
/// Returns 0 if equal, negative if `a < b`, positive if `a > b`.
#[no_mangle]
pub unsafe extern "C" fn strcmp(a: *const i8, b: *const i8) -> i32 {
    let mut i = 0usize;
    loop {
        let ca = *a.add(i) as u8;
        let cb = *b.add(i) as u8;
        if ca != cb { return ca as i32 - cb as i32; }
        if ca == 0  { return 0; }
        i += 1;
    }
}

/// Compare at most `n` characters of `a` and `b`.
#[no_mangle]
pub unsafe extern "C" fn strncmp(a: *const i8, b: *const i8, n: usize) -> i32 {
    for i in 0..n {
        let ca = *a.add(i) as u8;
        let cb = *b.add(i) as u8;
        if ca != cb { return ca as i32 - cb as i32; }
        if ca == 0  { return 0; }
    }
    0
}

/// Find the first occurrence of `c` in the null-terminated string `s`.
/// Returns a pointer to the match (including a match on `\0`) or null.
#[no_mangle]
pub unsafe extern "C" fn strchr(s: *const i8, c: i32) -> *const i8 {
    let target = c as i8;
    let mut i = 0usize;
    loop {
        let ch = *s.add(i);
        if ch == target { return s.add(i); }
        if ch == 0      { return core::ptr::null(); }
        i += 1;
    }
}

/// Find the last occurrence of `c` in the null-terminated string `s`.
#[no_mangle]
pub unsafe extern "C" fn strrchr(s: *const i8, c: i32) -> *const i8 {
    let target = c as i8;
    let mut last: *const i8 = core::ptr::null();
    let mut i = 0usize;
    loop {
        let ch = *s.add(i);
        if ch == target { last = s.add(i); }
        if ch == 0      { return last; }
        i += 1;
    }
}

/// Find the first occurrence of the null-terminated substring `needle` in
/// `haystack`.  Returns a pointer into `haystack` or null if not found.
#[no_mangle]
pub unsafe extern "C" fn strstr(haystack: *const i8, needle: *const i8) -> *const i8 {
    if *needle == 0 { return haystack; }
    let nlen = strlen(needle);
    let hlen = strlen(haystack);
    if nlen > hlen { return core::ptr::null(); }
    for i in 0..=(hlen - nlen) {
        if strncmp(haystack.add(i), needle, nlen) == 0 {
            return haystack.add(i);
        }
    }
    core::ptr::null()
}

// ── Safe Rust wrappers ────────────────────────────────────────────────────────

/// Return the length of a null-terminated byte string.  Panics if the string
/// is not null-terminated within `usize::MAX` bytes — in practice this cannot
/// happen on a correctly linked binary.
#[inline]
pub fn str_len(s: &[u8]) -> usize {
    s.iter().position(|&b| b == 0).unwrap_or(s.len())
}

/// Byte-wise comparison of two null-terminated byte slices.
#[inline]
pub fn str_eq(a: &[u8], b: &[u8]) -> bool {
    let la = str_len(a);
    let lb = str_len(b);
    la == lb && &a[..la] == &b[..lb]
}
