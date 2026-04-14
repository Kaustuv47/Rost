---
name: POSIX Compatibility Layer
description: rost-libc crate implementing POSIX interfaces on top of Rost syscalls
type: project
---

`servers/libc/` — crate `rost-libc`, rlib (added to servers workspace).

Implements: malloc/free/calloc/realloc (SYS_MAP heap at 0x0080_0000), string.h
(memcpy/memmove/memset/memcmp/strlen/strcpy/strcmp/strstr etc.), errno (AtomicI32 +
__errno_location), File I/O (open/close/read/write/lseek via VFS IPC OP_OPEN..OP_SEEK),
stdio (puts/putchar/fwrite/fread + WriteFmt core::fmt::Write adapter), unistd
(getpid/exit/sleep/usleep/exec_elf), signal (mapped to SYS_NOTIFY bits; signal_dispatch()
polls cooperatively), pthread (mutex as AtomicUsize CAS spinlock; pthread_create=ENOSYS;
pthread_once OK).

**Why:** §22 of ROADMAP — enable C/POSIX programs to link against Rost without std.
**How to apply:** Add `rost-libc = { path = "../libc" }` to a server's Cargo.toml.
Not required by any existing server yet — available as a dependency.
