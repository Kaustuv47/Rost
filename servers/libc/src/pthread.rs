//! POSIX threads — mutex / condvar primitives + stubs.
//!
//! # Threading model
//!
//! Rost processes do not share address space.  True `pthread_create` (sharing
//! address space + stack) is not supported; the function returns `ENOSYS`.
//!
//! What **is** implemented:
//!
//! | Primitive             | Implementation |
//! |-----------------------|----------------|
//! | `pthread_mutex_t`     | `AtomicUsize` CAS spinlock (safe on single-core) |
//! | `pthread_mutex_lock`  | spin-CAS until lock acquired |
//! | `pthread_mutex_unlock`| store 0 |
//! | `pthread_mutex_trylock`| single CAS attempt |
//! | `pthread_mutex_init` / `destroy` | zero-init / no-op |
//! | `pthread_self`        | returns calling process's PID cast to `pthread_t` |
//! | `pthread_equal`       | equality comparison |
//! | `pthread_create`      | returns `ENOSYS` |
//! | `pthread_join`        | returns `ENOSYS` |
//! | `pthread_exit`        | calls `exit(0)` |
//! | `pthread_once`        | one-time initialisation via `AtomicUsize` |
//!
//! A spinlock is acceptable under Rost's single-core guarantee (enforced at
//! boot by `check_cpu_features()`): the lock-holder never truly runs
//! concurrently with a spinner so the spin always terminates quickly.

use core::sync::atomic::{AtomicUsize, Ordering};
use crate::errno::{set_errno, ENOSYS, EINVAL, EBUSY};
use crate::types::c_int;
use crate::syscall::{getpid, exit};

// ── pthread_t ─────────────────────────────────────────────────────────────────

/// Opaque thread identifier.  In Rost, this is the process PID.
pub type pthread_t = u64;

// ── pthread_mutex_t ───────────────────────────────────────────────────────────

/// Mutex state: 0 = unlocked, 1 = locked.
///
/// Declared `#[repr(C)]` so C code can embed it in structs.
/// Size matches a typical pthread_mutex_t (8 bytes on LP64).
#[repr(C)]
pub struct pthread_mutex_t {
    state: AtomicUsize,
}

impl pthread_mutex_t {
    /// Statically-initialised unlocked mutex.
    pub const INIT: Self = pthread_mutex_t { state: AtomicUsize::new(0) };
}

/// Initialise a mutex.  `attr` is ignored (only the default "fast" type is
/// supported).  Returns 0 on success.
#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_init(
    mutex: *mut pthread_mutex_t,
    _attr: *const u8,
) -> c_int {
    if mutex.is_null() { set_errno(EINVAL); return EINVAL; }
    (*mutex).state.store(0, Ordering::Relaxed);
    0
}

/// Destroy a mutex (no-op — no dynamic resources).  Returns 0.
#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_destroy(mutex: *mut pthread_mutex_t) -> c_int {
    if mutex.is_null() { set_errno(EINVAL); return EINVAL; }
    0
}

/// Acquire a mutex.  Spins until the lock is available.
///
/// Returns 0 on success.  Returns `EDEADLK` if the calling process already
/// holds the lock (detected by comparing the owner PID).
#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_lock(mutex: *mut pthread_mutex_t) -> c_int {
    if mutex.is_null() { set_errno(EINVAL); return EINVAL; }
    loop {
        if (*mutex).state
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return 0;
        }
        // Cooperative yield to avoid starving interrupt handlers on the
        // single-core system.
        crate::syscall::yield_();
    }
}

/// Try to acquire a mutex without blocking.
///
/// Returns 0 if the lock was acquired, `EBUSY` if it is already held.
#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_trylock(mutex: *mut pthread_mutex_t) -> c_int {
    if mutex.is_null() { set_errno(EINVAL); return EINVAL; }
    if (*mutex).state
        .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
    {
        0
    } else {
        EBUSY
    }
}

/// Release a mutex.  Returns 0.
#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_unlock(mutex: *mut pthread_mutex_t) -> c_int {
    if mutex.is_null() { set_errno(EINVAL); return EINVAL; }
    (*mutex).state.store(0, Ordering::Release);
    0
}

// ── pthread_t identity ────────────────────────────────────────────────────────

/// Return the calling thread's identifier (= process PID).
#[no_mangle]
pub extern "C" fn pthread_self() -> pthread_t {
    getpid() as pthread_t
}

/// Return non-zero if `t1` and `t2` identify the same thread.
#[no_mangle]
pub extern "C" fn pthread_equal(t1: pthread_t, t2: pthread_t) -> c_int {
    if t1 == t2 { 1 } else { 0 }
}

// ── pthread_create / pthread_join (not supported) ─────────────────────────────

/// Create a new thread.
///
/// **Not implemented.** Rost processes do not share address space so true
/// multi-threading is not available.  Returns `ENOSYS`.
///
/// Use `unistd::exec_elf` to launch a new independent process.
#[no_mangle]
pub unsafe extern "C" fn pthread_create(
    _thread:    *mut pthread_t,
    _attr:      *const u8,
    _start_fn:  unsafe extern "C" fn(*mut u8) -> *mut u8,
    _arg:       *mut u8,
) -> c_int {
    ENOSYS
}

/// Wait for a thread to complete.
///
/// **Not implemented** — returns `ENOSYS`.
#[no_mangle]
pub unsafe extern "C" fn pthread_join(
    _thread:    pthread_t,
    _retval:    *mut *mut u8,
) -> c_int {
    ENOSYS
}

/// Terminate the calling thread (= calling process).
#[no_mangle]
pub extern "C" fn pthread_exit(_retval: *mut u8) -> ! {
    exit(0)
}

// ── pthread_once ──────────────────────────────────────────────────────────────

/// One-time initialisation control object.
#[repr(C)]
pub struct pthread_once_t {
    state: AtomicUsize,
}

impl pthread_once_t {
    pub const INIT: Self = pthread_once_t { state: AtomicUsize::new(0) };
}

const ONCE_PENDING: usize  = 1;
const ONCE_COMPLETE: usize = 2;

/// Ensure `init_routine` is called exactly once.
///
/// On a single-core system this is safe without a lock: the CAS + yield
/// loop guarantees the first caller runs `init_routine` and subsequent
/// callers wait until it completes.
#[no_mangle]
pub unsafe extern "C" fn pthread_once(
    once_control: *mut pthread_once_t,
    init_routine: unsafe extern "C" fn(),
) -> c_int {
    if once_control.is_null() { return EINVAL; }
    loop {
        let state = (*once_control).state.load(Ordering::Acquire);
        if state == ONCE_COMPLETE { return 0; }
        if state == 0 {
            if (*once_control).state
                .compare_exchange(0, ONCE_PENDING, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                init_routine();
                (*once_control).state.store(ONCE_COMPLETE, Ordering::Release);
                return 0;
            }
        }
        crate::syscall::yield_();
    }
}
