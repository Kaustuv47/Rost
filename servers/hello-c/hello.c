/*
 * hello-c — freestanding C demo for Rost.
 *
 * This program runs in ring-3 with no libc, no CRT, no OS dependencies.
 * It uses inline asm to invoke Rost syscalls directly via the x86_64
 * SYSCALL instruction.
 *
 * Build:  make -C servers/hello-c
 * Run:    exec /bin/hello-c   (from the Rost shell)
 *
 * Syscalls used:
 *   SYS_UART_WRITE (12): write one byte to COM1 (bypasses uart-drv queue)
 *   SYS_EXIT       ( 1): terminate process cleanly
 */

/* SYS_UART_WRITE (12): write one byte directly to COM1 */
static inline void uart_write(char c)
{
    __asm__ volatile (
        "syscall"
        : /* no outputs */
        : "a"(12ULL), "D"((unsigned long long)(unsigned char)c)
        : "rcx", "r11", "memory"
    );
}

static void print(const char *s)
{
    while (*s)
        uart_write(*s++);
}

/*
 * _start — process entry point.
 *
 * There is no C runtime (no crt0), so execution jumps here directly from
 * the kernel's ring3_entry_trampoline IRETQ.  argc/argv/envp are not set up.
 */
void _start(void)
{
    print("[hello-c] Hello from C in ring-3!\n");
    print("[hello-c] Freestanding C works on Rost.\n");
    print("[hello-c] No libc. No CRT. Just syscalls.\n");

    /* SYS_EXIT(0) */
    __asm__ volatile (
        "syscall"
        : /* no outputs */
        : "a"(1ULL), "D"(0ULL)
        : "rcx", "r11", "memory"
    );
    __builtin_unreachable();
}
