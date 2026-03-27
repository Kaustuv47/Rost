//! ELF-spawn and server-restart hooks — decouple the syscall layer
//! (`arch-x86_64`) from the kernel ELF loader (`kernel` crate) without
//! creating a circular dependency.
//!
//! The `kernel` crate calls [`set_elf_spawn_fn`] and [`set_restart_server_fn`]
//! once during Stage 6 (before any user process is spawned).
//! The syscall dispatcher (`arch-x86_64`) then calls:
//!   - [`call_elf_spawn`]      for `SYS_SPAWN_ELF`      (26)
//!   - [`call_restart_server`] for `SYS_RESTART_SERVER`  (27)
//!
//! # Safety invariant
//! `call_elf_spawn` is invoked with STAC active (SMAP disabled) so it may
//! safely read the user-space ELF buffer via the raw pointer.
//! `call_restart_server` is also invoked with STAC active; the name pointer
//! points to a 16-byte user buffer.

use core::sync::atomic::{AtomicU64, Ordering};

// ── SYS_SPAWN_ELF hook ────────────────────────────────────────────────────────

/// Stored as a `u64` to avoid keeping a `*const ()` in a static
/// (which would require `unsafe impl Sync`).
///
/// Format: `unsafe fn(*const u8, usize, u8) -> u32`
///  - arg0: pointer to first byte of ELF image
///  - arg1: byte length of ELF image
///  - arg2: priority (0 = default 128)
///  - return: new PID, or `u32::MAX` on error
static ELF_SPAWN_FN: AtomicU64 = AtomicU64::new(0);

/// Function-pointer type for the ELF spawn hook.
pub type SpawnElfFn = unsafe fn(*const u8, usize, u8) -> u32;

/// Register the ELF loader callable.
///
/// Called once from `kernel/src/main.rs` **before** any user process is
/// spawned so the hook is available when the first `SYS_SPAWN_ELF` arrives.
pub fn set_elf_spawn_fn(f: SpawnElfFn) {
    ELF_SPAWN_FN.store(f as usize as u64, Ordering::SeqCst);
}

/// Invoke the registered ELF spawn function.
///
/// # Returns
/// `Some(pid)` on success, `None` if the hook is unregistered or the load fails.
///
/// # Safety
/// The caller must guarantee that `[ptr, ptr + len)` is readable for the
/// duration of the call.  In the syscall context, STAC must be active.
pub unsafe fn call_elf_spawn(ptr: *const u8, len: usize, priority: u8) -> Option<u32> {
    let addr = ELF_SPAWN_FN.load(Ordering::SeqCst);
    if addr == 0 { return None; }
    // SAFETY: the address was stored by `set_elf_spawn_fn` from a valid
    // `SpawnElfFn` pointer.  Transmute recovers the original function type.
    let f: SpawnElfFn = unsafe { core::mem::transmute(addr as usize) };
    let pid = unsafe { f(ptr, len, priority) };
    if pid == u32::MAX { None } else { Some(pid) }
}

// ── SYS_RESTART_SERVER hook ───────────────────────────────────────────────────

/// Format: `unsafe fn(*const u8, usize) -> u32`
///  - arg0: pointer to 16-byte null-padded service name in user space
///  - arg1: length of the name (≤ 16)
///  - return: new PID, or `u32::MAX` on error (unknown name / table full / OOM)
static RESTART_SERVER_FN: AtomicU64 = AtomicU64::new(0);

/// Function-pointer type for the server restart hook.
pub type RestartServerFn = unsafe fn(*const u8, usize) -> u32;

/// Register the server restart callable.
///
/// Called once from `kernel/src/main.rs` alongside [`set_elf_spawn_fn`].
pub fn set_restart_server_fn(f: RestartServerFn) {
    RESTART_SERVER_FN.store(f as usize as u64, Ordering::SeqCst);
}

/// Invoke the registered server restart function.
///
/// `name` is a user-space pointer to a 16-byte null-padded ASCII service name.
///
/// # Returns
/// `Some(new_pid)` on success, `None` if the hook is unregistered, the name
/// is unknown, or the process table / memory is exhausted.
///
/// # Safety
/// The caller must guarantee that `[name_ptr, name_ptr + name_len)` is readable.
/// In the syscall context, STAC must be active.
pub unsafe fn call_restart_server(name_ptr: *const u8, name_len: usize) -> Option<u32> {
    let addr = RESTART_SERVER_FN.load(Ordering::SeqCst);
    if addr == 0 { return None; }
    let f: RestartServerFn = unsafe { core::mem::transmute(addr as usize) };
    let pid = unsafe { f(name_ptr, name_len) };
    if pid == u32::MAX { None } else { Some(pid) }
}
