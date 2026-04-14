//! rost-libc — POSIX compatibility layer for Rost ring-3 servers.
//!
//! Maps standard C/POSIX interfaces onto Rost syscalls.  Runs entirely in
//! ring 3; nothing touches the kernel directly except through the syscall ABI
//! defined in `arch-x86_64/src/cpu/syscall.rs`.
//!
//! # Module layout
//!
//! | Module      | Contents |
//! |-------------|-------------------------------------------------------------------------|
//! | `types`     | C primitive type aliases (`c_int`, `size_t`, `pid_t`, …) |
//! | `errno`     | Error constants + per-process `errno` accessor |
//! | `malloc`    | `malloc` / `free` / `calloc` / `realloc` — heap via `SYS_MAP` |
//! | `string`    | `memcpy`, `memset`, `memcmp`, `strlen`, `strcmp`, `strcpy`, … |
//! | `stdio`     | `puts`, `putchar`, `fwrite`, `fread`, `write_fmt` |
//! | `unistd`    | `getpid`, `exit`, `read`, `write`, `close`, `lseek`, `sleep` |
//! | `fcntl`     | `open` + O_* flag constants |
//! | `signal`    | `signal`, `raise`, `kill` — mapped to Rost IPC notifications |
//! | `pthread`   | `pthread_mutex_t` (atomic spinlock); `pthread_create` → ENOSYS |
//!
//! # What is intentionally not implemented
//!
//! * `fork` — no copy-on-write page tables in the kernel; `exec` is available
//!   via `unistd::exec_elf`.
//! * Full `printf` with `va_list` — requires nightly `core::ffi::VaList`; use
//!   `stdio::write_fmt` with `core::fmt::Write` or `stdio::puts` / `fwrite`.
//! * Multi-threaded `pthread_create` — Rost processes do not share address
//!   space; returns `ENOSYS`.  `pthread_mutex_t` works (atomic spinlock).
#![no_std]
#![allow(non_camel_case_types, clippy::missing_safety_doc)]

pub mod errno;
pub mod fcntl;
pub mod malloc;
pub mod pthread;
pub mod signal;
pub mod stdio;
pub mod string;
pub mod types;
pub mod unistd;

pub(crate) mod syscall;
pub(crate) mod vfs;
