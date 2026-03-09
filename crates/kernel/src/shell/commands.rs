use hal::uart as serial;

fn trim(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|&b| b != b' ' && b != b'\t').unwrap_or(s.len());
    let end = s.iter().rposition(|&b| b != b' ' && b != b'\t').map(|i| i + 1).unwrap_or(0);
    if start >= end { b"" } else { &s[start..end] }
}

pub fn dispatch(line: &[u8]) {
    let line = trim(line);

    if line.starts_with(b"echo") {
        let rest = trim(&line[4..]);
        let text = if rest.len() >= 2 && rest[0] == b'"' && rest[rest.len() - 1] == b'"' {
            &rest[1..rest.len() - 1]
        } else {
            rest
        };
        for &b in text { serial::put_byte(b); }
        serial::put_byte(b'\n');
    } else if line == b"help" {
        serial::print_str("Commands:\n");
        serial::print_str("  echo <text>   print text to console\n");
        serial::print_str("  help          show this message\n");
    } else if !line.is_empty() {
        serial::print_str("Unknown command: '");
        for &b in line { serial::put_byte(b); }
        serial::print_str("'\n");
    }
}
