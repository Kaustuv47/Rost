# Chapter 17 — The Shell Server

## 17.1 Design Philosophy

The Rost shell (`servers/shell`, binary `rost-shell`, PID 5) is a full
interactive shell running entirely in ring-3.  It has no kernel privileges and
accesses hardware exclusively through IPC:

```
rost-shell ──SYS_UART_WRITE──► kernel ──port I/O──► COM1
rost-shell ──send_msg──────────► gop    ──MMIO──────► framebuffer
           ◄──recv_msg (uart-drv pushes keystroke)──── uart-drv
           ──send_msg──────────► vfs    (file I/O)
           ──SYS_SPAWN_ELF────► kernel (exec)
```

The shell is structured as a Rust library spread across six modules:

| Module | Purpose |
|--------|---------|
| `shell/mod.rs` | Shell state, main loop |
| `shell/commands.rs` | Built-in command dispatcher, `exec` |
| `shell/line_editor.rs` | Emacs-mode line editing |
| `shell/history.rs` | 32-entry command history |
| `shell/expand.rs` | History and variable expansion |
| `shell/escape.rs` | ANSI/VT100 escape sequence parser |
| `shell/vars.rs` | Environment variable store |
| `shell/aliases.rs` | Alias table |
| `io.rs` | UART I/O primitives |
| `gop.rs` | GOP framebuffer forwarding |

## 17.2 Shell State

```rust
pub struct Shell {
    editor:    LineEditor,        // current input line buffer
    kill_ring: [u8; LINE_MAX],   // single-entry kill ring (Ctrl+K/U/W then Ctrl+Y)
    kill_len:  usize,
    history:   History,           // 32-entry ring buffer of past commands
    parser:    EscapeParser,      // ANSI escape sequence state machine
    hist_idx:  Option<usize>,    // None = fresh line; Some(n) = browsing history
    saved:     LineEditor,        // saved fresh line while browsing
    search:    SearchState,       // Ctrl+R incremental history search
    cwd:       [u8; 64],         // current working directory
    cwd_len:   usize,
    vars:      VarStore,          // environment variables ($VAR)
    aliases:   AliasStore,        // alias table
    last_exit: u64,               // $? — exit code of last command
    my_pid:    u32,               // our own PID (SYS_GETPID cache)
}
```

All fields are stack-allocated (no heap).  The `Shell` struct is approximately
20 KB in size (dominated by `LineEditor`, `History`, `VarStore`, and `AliasStore`
arrays).  The 128 KB user-mode stack handles this comfortably.

## 17.3 Terminal I/O

### 17.3.1 Output

```rust
// io.rs
pub fn put_byte(b: u8) {
    crate::syscall::uart_write(b);  // SYS_UART_WRITE: direct COM1 bypass
    crate::gop::write_byte(b);      // forward to GOP framebuffer
}

pub fn put_newline() {
    crate::syscall::uart_write(b'\r');
    crate::syscall::uart_write(b'\n');
    crate::gop::write_byte(b'\r');
    crate::gop::write_byte(b'\n');
    crate::gop::flush();
}
```

Output goes directly through `SYS_UART_WRITE` (syscall 12), which calls the
kernel's `hal::uart::put_byte` without going through uart-drv's IPC queue.
This bypasses the 16-message queue depth and prevents byte loss during long
outputs like the help screen or `ls` on a large directory.

Each output byte is also forwarded to the GOP server so the graphical display
mirrors the serial console.

### 17.3.2 Input

```rust
pub fn read_byte() -> Option<u8> {
    let v = crate::syscall::recv(0);  // timeout=0 → non-blocking poll
    if v == u64::MAX { None } else { Some(v as u8) }
}

pub fn read_byte_blocking() -> u8 {
    crate::gop::flush();  // flush prompt to framebuffer before blocking
    loop {
        let v = crate::syscall::recv(u64::MAX);  // block until keystroke
        if v != u64::MAX { return v as u8; }
        crate::syscall::yield_cpu();
    }
}
```

Keystrokes arrive as IPC messages sent by uart-drv (each byte as `SYS_SEND(shell_pid, byte, 0)`).
`recv` with `timeout=0` polls non-blocking; `timeout=u64::MAX` blocks until a
byte arrives.  While blocked, the scheduler marks the shell as `Blocked` and does
not schedule it — it burns no CPU waiting for input.

**GOP flush before blocking**: the prompt (e.g. `~/bin $ `) does not end with
`\n`, so `put_newline` is never called after it.  `read_byte_blocking` calls
`gop::flush()` before blocking to force the partial line into the GOP IPC message
so the framebuffer shows the prompt immediately.

## 17.4 GOP Framebuffer Forwarding

The GOP module (`gop.rs`) batches output bytes into 40-byte chunks:

```rust
const OP_GOP_PUTS: u64 = 0x71;

static mut BUF: [u8; 40] = [0u8; 40];
static mut BUF_LEN: usize = 0;

pub fn write_byte(b: u8) {
    let pid = gop_pid();
    if pid == 0 { return; }   // GOP not available yet

    unsafe {
        BUF[BUF_LEN] = b;
        BUF_LEN += 1;
        if BUF_LEN == 40 { flush_inner(pid); }  // auto-flush when full
    }
}

pub fn flush() {
    let pid = gop_pid();
    if pid == 0 { return; }
    unsafe { flush_inner(pid); }
}
```

Bytes are packed into a single `OP_GOP_PUTS` IPC message (5 × u64 = 40 bytes
of payload).  Sending one message per 40 bytes is far more efficient than sending
one message per byte.

**Fire-and-forget**: `send_msg` is non-blocking.  If the GOP server's IPC queue
is full, the message is dropped rather than stalling the shell.  A slow GOP
server cannot block user input.

**Lazy PID lookup**: `gop_pid()` caches the result after the first successful
`SYS_LOOKUP("gop")`.  If the GOP server hasn't registered yet (it starts after
the shell), the call returns 0 and output is silently dropped until the GOP
server is available.

## 17.5 Line Editing

The line editor (`shell/line_editor.rs`) implements GNU Readline-compatible
Emacs bindings:

| Key | Action |
|-----|--------|
| Ctrl+A | Move to start of line |
| Ctrl+E | Move to end of line |
| Ctrl+B / ← | Move backward one character |
| Ctrl+F / → | Move forward one character |
| Alt+B / Ctrl+← | Move backward one word |
| Alt+F / Ctrl+→ | Move forward one word |
| Ctrl+D | Delete character at cursor (or EOF if empty line) |
| Backspace | Delete character before cursor |
| Ctrl+K | Kill to end of line (saved to kill ring) |
| Ctrl+U | Kill to start of line (saved to kill ring) |
| Ctrl+W | Kill previous word (saved to kill ring) |
| Ctrl+Y | Yank kill ring contents at cursor |
| Ctrl+L | Clear screen, redraw prompt |
| Ctrl+C | Cancel current line |
| Tab | Tab-complete command or path |

The kill ring holds one entry (same as standard Emacs).  Ctrl+K, Ctrl+U, and
Ctrl+W all write to the same slot.  Ctrl+Y pastes it.

## 17.6 History

```rust
const HISTORY_MAX: usize = 32;

struct History {
    lines: [[u8; LINE_MAX]; HISTORY_MAX],
    lens:  [usize; HISTORY_MAX],
    count: usize,
    head:  usize,   // next write position (ring buffer)
}
```

The history ring holds up to 32 commands.  Arrow keys navigate:

- Ctrl+P / ↑ — older command (higher age)
- Ctrl+N / ↓ — newer command (lower age)

When browsing history, the current fresh line is saved to `Shell::saved`.
Returning to the bottom of history (pressing ↓ past the newest entry) restores
the saved fresh line.

### 17.6.1 History Expansion

```
!!        — repeat last command
!n        — repeat command number n (1-based from oldest in history)
!-n       — repeat n-th command from the end
!prefix   — repeat most recent command starting with prefix
```

Expansion runs before variable expansion so that `!!` can expand to a line that
itself contains `$VAR` references.

## 17.7 Variable Expansion

```
$VAR    — expand variable VAR
${VAR}  — same, for adjacent non-space characters
$$      — process PID
$?      — exit code of last command
$0      — shell name ("rost-shell")
~       — expand to home directory ($HOME or "/")
```

The variable store (`shell/vars.rs`) holds up to 32 key-value pairs as fixed-size
`[u8; 16]` key and `[u8; 64]` value arrays.  Default variables:

```
HOME=/
PATH=/bin
SHELL=rost-shell
TERM=vt100
```

## 17.8 Alias Handling

```rust
alias ls='ls --color=auto'
alias ll='ls -la'
```

Aliases are expanded before tokenization.  The alias store holds up to 16
entries.  Default aliases:

```
la  → 'ls -a'
ll  → 'ls -l'
l   → 'ls -CF'
```

## 17.9 Command Pipeline

Every input line goes through a five-stage pipeline:

```
1. History expansion    (!!, !n, !prefix)
2. Variable expansion   ($VAR, ${VAR}, $$, $?, ~)
3. Compound command split  (;  &&  ||)
4. Alias resolution
5. Built-in dispatch / exec
```

Compound commands:
- `;` — run sequentially, ignore exit code
- `&&` — run second only if first exits 0
- `||` — run second only if first exits non-zero

## 17.10 Built-In Commands

The shell implements 30 built-in commands:

| Command | Description |
|---------|-------------|
| `alias` / `unalias` | Manage alias table |
| `cat` | Concatenate files via VFS OP_READ |
| `cd` | Change working directory |
| `clear` | Clear screen (ANSI escape `\e[2J\e[H`) |
| `date` | Display SYS_CLOCK value as uptime |
| `echo` | Print arguments with variable expansion |
| `env` | List environment variables |
| `exec` | Load and spawn an ELF via SYS_SPAWN_ELF |
| `exit` | Call SYS_EXIT |
| `export` | Set environment variable |
| `false` | Exit with code 1 |
| `halt` | Send OP_SHUTDOWN to init |
| `help` | List all commands |
| `history` | Display command history |
| `kill` | Send SYS_NOTIFY to a PID |
| `log` | Read init's boot log via OP_LOG_READ |
| `ls` | List directory via VFS OP_READDIR |
| `mem` | Display physical memory statistics |
| `mkdir` | Create directory via VFS OP_MKDIR |
| `mount` | Mount a filesystem via VFS OP_MOUNT |
| `ps` | List processes via SYS_LIST_PROCS |
| `pwd` | Print current working directory |
| `rm` / `unlink` | Delete file via VFS OP_UNLINK |
| `set` | Display or set shell variables |
| `sleep` | Busy-wait N seconds |
| `source` / `.` | Execute a shell script file |
| `test` / `[` | POSIX test expressions |
| `true` | Exit with code 0 |
| `type` | Show whether a name is a builtin or file |
| `unset` | Unset a variable |
| `uptime` | Display SYS_CLOCK as hours:minutes:seconds |
| `which` | Find command in PATH |

## 17.11 The `exec` Command

`exec path [priority]` loads and runs an ELF binary from the VFS:

```
1. Resolve path (relative to cwd, or absolute)
2. VFS OP_STAT — verify file exists and get size
3. VFS OP_READ — read ELF bytes into EXEC_BUF (512 KB static BSS)
4. Validate ELF magic (\x7fELF)
5. SYS_SPAWN_ELF(EXEC_BUF, size, priority)
6. New process runs; shell continues (parent does not wait)
```

The 512 KB `EXEC_BUF` is a BSS static — it doesn't inflate the shell ELF on
disk.  Debug binaries often exceed 512 KB; if the file is too large, `exec`
reports an error without attempting the spawn.

## 17.12 Ctrl+R Reverse History Search

```
Ctrl+R       — enter incremental search mode
              prompt changes to: (reverse-i-search)`query': matched-line
typing       — refines the query; jumps to the most recent match
Ctrl+R again — cycle to the next older match
Enter        — accept the match, execute the line
Esc/Ctrl+G   — cancel; restore the line that was being edited before Ctrl+R
```

The search state is:

```rust
struct SearchState {
    active:    bool,
    query:     [u8; 64],
    qlen:      usize,
    match_age: usize,   // age of current match in history
    saved:     LineEditor,  // line saved before entering search
}
```

## 17.13 Tab Completion

Pressing Tab triggers completion:

1. If the cursor is at the first token position: complete against the built-in
   command list (binary search on the sorted `COMMANDS` array)
2. If the cursor is mid-path: send VFS `OP_READDIR` for the parent directory
   and find entries that match the current prefix
3. If exactly one match: replace the token in the line
4. If multiple matches: print them as a column list and redraw the prompt

## 17.14 Script Execution via `source`

`source file` (or `. file`):

1. Reads up to 2048 bytes from the VFS via `OP_READ`
2. Splits on newlines (max 64 lines)
3. Executes each non-comment, non-empty line through the same pipeline as
   interactive input

This enables initialization scripts like `/etc/profile` and user-defined
automation scripts stored in the VFS overlay.

## 17.15 Summary

The Rost shell provides:

- **Full Emacs line editing** — Ctrl+A/B/C/D/E/F/K/L/N/P/R/U/W/Y, kill ring, word motion
- **32-entry command history** — arrow key navigation, `!!`/`!n`/`!prefix` expansion
- **Variable expansion** — `$VAR`, `${VAR}`, `$$`, `$?`, `~`
- **Alias table** — 16 entries, default `la`/`ll`/`l` aliases
- **Compound commands** — `;`, `&&`, `||`
- **Tab completion** — builtins + VFS directory listings
- **Ctrl+R search** — incremental reverse history search
- **30 built-in commands** — full POSIX shell utility set
- **ELF exec** — loads user binaries from VFS via SYS_SPAWN_ELF
- **Script execution** — `source` / `.` for shell scripts
- **Dual output** — UART serial + GOP framebuffer, mirrored in real time
