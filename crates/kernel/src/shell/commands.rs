use hal::uart as serial;

// ── Kernel Emergency Console — command set ────────────────────────────────────
//
// Intentionally minimal: only `help`, `halt`, and `info`.
// Full interactive shell lives in servers/shell (ring-3, rost-shell binary).

pub enum Action {
    Continue,
    Halt,
}

pub fn dispatch(line: &[u8]) -> Action {
    let cmd = trim(line);
    if cmd.is_empty() { return Action::Continue; }

    match cmd {
        b"halt" => return Action::Halt,
        b"help" => cmd_help(),
        b"info" => cmd_info(),
        _ => {
            serial::print_str("[kec] unknown command: ");
            for &b in cmd { serial::put_byte(b); }
            serial::print_str(" (try: help halt info)\n");
        }
    }

    Action::Continue
}

fn cmd_help() {
    serial::print_str("Kernel Emergency Console [ring-0]\n");
    serial::print_str("  help   show this message\n");
    serial::print_str("  halt   halt the system\n");
    serial::print_str("  info   show kernel build info\n");
    serial::print_str("NOTE: full shell is servers/rost-shell (ring-3)\n");
}

fn cmd_info() {
    serial::print_str("Rost microkernel — x86_64 / UEFI\n");
    serial::print_str("Ring-0 emergency console — no IPC, no scheduling\n");
}

/// Return the slice with leading/trailing spaces removed.
fn trim(line: &[u8]) -> &[u8] {
    let start = line.iter().position(|&b| b != b' ').unwrap_or(line.len());
    let end   = line.iter().rposition(|&b| b != b' ').map(|i| i + 1).unwrap_or(0);
    if start >= end { b"" } else { &line[start..end] }
}
