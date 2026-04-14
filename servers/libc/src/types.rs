//! C primitive type aliases.
//!
//! These mirror the types a C compiler would define for `x86_64-unknown-none`.

/// Signed char
pub type c_char  = i8;
/// Unsigned char
pub type c_uchar = u8;
/// Signed int (32-bit on x86-64)
pub type c_int   = i32;
/// Unsigned int (32-bit on x86-64)
pub type c_uint  = u32;
/// Signed long (64-bit on x86-64 LP64)
pub type c_long  = i64;
/// Unsigned long (64-bit on x86-64 LP64)
pub type c_ulong = u64;

/// Memory size type (unsigned, matches pointer width)
pub type size_t  = usize;
/// Signed memory / I/O size type
pub type ssize_t = isize;
/// Process identifier
pub type pid_t   = u32;
/// File offset (signed 64-bit)
pub type off_t   = i64;
/// File permission/mode bits
pub type mode_t  = u32;
/// File descriptor number
pub type c_fd    = i32;

/// Opaque void pointer equivalent — use `*mut u8` / `*const u8` in practice.
pub use core::ffi::c_void;

// SEEK_* constants for lseek.
pub const SEEK_SET: c_int = 0;
pub const SEEK_CUR: c_int = 1;
pub const SEEK_END: c_int = 2;
