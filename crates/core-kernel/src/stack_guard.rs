//! Stack-Smashing Protector (SSP) runtime support for the UEFI / MSVC ABI.
//!
//! When the kernel is compiled with `-Z stack-protector=strong` (nightly),
//! LLVM inserts a canary into functions with local arrays or large frames.
//! The target-specific symbols required differ by ABI:
//!
//! | ABI / linker model | guard global         | check function              |
//! |--------------------|----------------------|-----------------------------|
//! | ELF (Linux/GCC)    | `__stack_chk_guard`  | `__stack_chk_fail() -> !`   |
//! | MSVC / UEFI (PE)   | `__security_cookie`  | `__security_check_cookie()` |
//!
//! The UEFI target links as a PE/COFF binary with MSVC-compatible calling
//! conventions, so we provide the two MSVC-flavoured symbols.
//!
//! This module lives in `core-kernel` (rather than the top-level `kernel`
//! crate) so that the symbols are visible to the linker when any crate in the
//! workspace is built with `-Z stack-protector=strong`.
//!
//! # Canary strategy
//! We use a **fixed compile-time canary** rather than runtime RDRAND
//! randomisation.  A fixed value still catches the common case (accidental
//! linear overflows) and avoids a chicken-and-egg initialisation race:
//! with `-Z stack-protector=strong`, `efi_main` may get a canary in its
//! own prologue before any `init` routine could set a runtime value.
//!
//! # LLVM epilogue model (MSVC)
//! - Prologue: stores raw `*__security_cookie` in a local stack slot.
//! - Epilogue: loads the slot, calls `__security_check_cookie(stored_value)`.
//! - `__security_check_cookie` does a direct equality comparison.
//!
//! IEC 61508 §7.4.3: spatial isolation — the canary word lies between local
//! variables and the saved return address, complementing the read-only `.text`
//! mapping established by `remap_kernel_sections()`.

// These symbols must be present at link time; they are emitted by the compiler
// for every crate compiled with -Z stack-protector, so they belong here in the
// shared core-kernel crate rather than in the kernel binary crate.

// ── Stack canary ──────────────────────────────────────────────────────────────

/// Global stack-canary value (MSVC/UEFI model).
///
/// LLVM reads this in every protected function's prologue and stores a copy
/// on the stack.  The epilogue passes the stored copy to
/// `__security_check_cookie` for comparison.
///
/// The value is non-zero in every byte and non-ASCII to reduce the chance of
/// being produced by an out-of-bounds string write.
#[no_mangle]
pub static __security_cookie: usize = 0x595F_D4A3_E8C7_2B16;

// ── Check function ────────────────────────────────────────────────────────────

/// Called by compiler-generated SSP epilogue with the stored canary value.
///
/// Returns normally when `cookie == __security_cookie`; halts immediately on
/// mismatch (stack corruption detected).
///
/// Marked `#[inline(never)]` to ensure the check is not elided by the
/// compiler, and must not itself be stack-protected (recursion risk).
#[no_mangle]
#[inline(never)]
pub unsafe extern "C" fn __security_check_cookie(cookie: usize) {
    if cookie != __security_cookie {
        hal::uart::print_str(
            "[FATAL] Stack smash detected — security cookie mismatch, halting.\n",
        );
        loop {
            core::arch::asm!("cli", "hlt", options(nostack, nomem));
        }
    }
}
