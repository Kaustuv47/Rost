//! Standard I/O — output formatting and buffered stream helpers.
//!
//! # Design
//!
//! Rost has no buffered stdio; all output goes directly through `SYS_UART_WRITE`
//! or the VFS write path.  This module provides:
//!
//! * `puts` / `putchar` — POSIX C-string output to stdout.
//! * `fwrite` / `fread` — block I/O through `unistd::write` / `read`.
//! * `write_fmt` / `WriteFmt` — `core::fmt::Write` adapter so that
//!   `write!(WriteFmt::stdout(), "hello {}", n)` works from Rust code.
//! * `print_dec` / `print_hex` — fast integer-to-serial helpers.
//!
//! There is no `printf` accepting a va_list: Rust's `core::ffi::VaList` is
//! unstable.  Rust callers should use `write_fmt`; C callers can implement
//! a sprintf wrapper on top.

use core::fmt;
use crate::syscall::uart_write;
use crate::types::{c_int, size_t};
use crate::unistd::{read, write};

// ── Standard stream pseudo-fd ─────────────────────────────────────────────────

/// Pseudo file-descriptor value used by fwrite/fread for stdin.
pub const STDIN:  c_int = 0;
/// Pseudo file-descriptor value used by fwrite/fread for stdout.
pub const STDOUT: c_int = 1;
/// Pseudo file-descriptor value used by fwrite/fread for stderr.
pub const STDERR: c_int = 2;

// ── puts / putchar ────────────────────────────────────────────────────────────

/// Write a null-terminated string to stdout followed by a newline (`\r\n`).
/// Returns a non-negative value on success or EOF (-1) on error.
#[no_mangle]
pub unsafe extern "C" fn puts(s: *const i8) -> c_int {
    if s.is_null() { return -1; }
    let mut i = 0usize;
    loop {
        let b = *s.add(i) as u8;
        if b == 0 { break; }
        uart_write(b);
        i += 1;
    }
    uart_write(b'\r');
    uart_write(b'\n');
    0
}

/// Write a single character to stdout.  Returns the character written (cast
/// to `c_int`) or EOF (-1) on error.
#[no_mangle]
pub extern "C" fn putchar(c: c_int) -> c_int {
    uart_write(c as u8);
    c
}

/// Write a null-terminated string to `stream` fd without appending a newline.
/// Returns 0 on success or EOF (-1) on error.
#[no_mangle]
pub unsafe extern "C" fn fputs(s: *const i8, stream: c_int) -> c_int {
    if s.is_null() { return -1; }
    let mut i = 0usize;
    loop {
        let b = *s.add(i) as u8;
        if b == 0 { break; }
        if stream == STDOUT || stream == STDERR {
            uart_write(b);
        } else {
            let b_arr = [b];
            if write(stream, b_arr.as_ptr(), 1) < 0 { return -1; }
        }
        i += 1;
    }
    0
}

// ── fwrite / fread ────────────────────────────────────────────────────────────

/// Write `nmemb` objects of `size` bytes each from `buf` to `stream`.
/// Returns the number of complete objects written.
#[no_mangle]
pub unsafe extern "C" fn fwrite(
    buf:   *const u8,
    size:  size_t,
    nmemb: size_t,
    stream: c_int,
) -> size_t {
    if size == 0 || nmemb == 0 { return 0; }
    let total = match size.checked_mul(nmemb) {
        Some(n) => n,
        None    => return 0,
    };
    let n = write(stream, buf, total);
    if n <= 0 { 0 } else { (n as usize) / size }
}

/// Read `nmemb` objects of `size` bytes each from `stream` into `buf`.
/// Returns the number of complete objects read.
#[no_mangle]
pub unsafe extern "C" fn fread(
    buf:   *mut u8,
    size:  size_t,
    nmemb: size_t,
    stream: c_int,
) -> size_t {
    if size == 0 || nmemb == 0 { return 0; }
    let total = match size.checked_mul(nmemb) {
        Some(n) => n,
        None    => return 0,
    };
    let n = read(stream, buf, total);
    if n <= 0 { 0 } else { (n as usize) / size }
}

// ── core::fmt::Write adapter ──────────────────────────────────────────────────

/// Wrapper that implements `core::fmt::Write` by writing to a file descriptor.
///
/// # Example
/// ```rust
/// use core::fmt::Write;
/// use rost_libc::stdio::WriteFmt;
///
/// let mut out = WriteFmt::stdout();
/// let _ = write!(out, "Hello, {}!\n", 42);
/// ```
pub struct WriteFmt {
    fd: c_int,
}

impl WriteFmt {
    /// Create a writer targeting stdout (fd 1).
    #[inline]
    pub fn stdout() -> Self { WriteFmt { fd: STDOUT } }
    /// Create a writer targeting stderr (fd 2).
    #[inline]
    pub fn stderr() -> Self { WriteFmt { fd: STDERR } }
    /// Create a writer targeting any file descriptor.
    #[inline]
    pub fn from_fd(fd: c_int) -> Self { WriteFmt { fd } }
}

impl fmt::Write for WriteFmt {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        unsafe { write(self.fd, bytes.as_ptr(), bytes.len()); }
        Ok(())
    }
}

// ── Integer print helpers ─────────────────────────────────────────────────────

/// Print an unsigned decimal integer to stdout.
pub fn print_dec(mut n: u64) {
    if n == 0 { uart_write(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut i = 20usize;
    while n > 0 { i -= 1; buf[i] = b'0' + (n % 10) as u8; n /= 10; }
    for &b in &buf[i..] { uart_write(b); }
}

/// Print a `u64` in hexadecimal (with `0x` prefix) to stdout.
pub fn print_hex(n: u64) {
    uart_write(b'0');
    uart_write(b'x');
    let hex = b"0123456789abcdef";
    for shift in (0..16).rev() {
        uart_write(hex[((n >> (shift * 4)) & 0xF) as usize]);
    }
}

/// Print a `&str` to stdout.
pub fn print_str(s: &str) {
    for b in s.bytes() { uart_write(b); }
}

/// Print a `&[u8]` byte slice to stdout.
pub fn print_bytes(s: &[u8]) {
    for &b in s { uart_write(b); }
}
