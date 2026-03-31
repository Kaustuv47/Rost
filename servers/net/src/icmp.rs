//! ICMP — Internet Control Message Protocol (RFC 792).
//!
//! Echo Request/Reply layout (after IP header):
//!   byte  0     : type (8=request, 0=reply)
//!   byte  1     : code (0)
//!   bytes 2–3   : checksum
//!   bytes 4–5   : identifier
//!   bytes 6–7   : sequence number
//!   bytes 8+    : data payload

use crate::ipv4::ip_checksum;

pub const ICMP_ECHO_REQUEST: u8 = 8;
pub const ICMP_ECHO_REPLY:   u8 = 0;

/// Build an ICMP echo request into `out`.
/// Returns the number of bytes written.
pub fn build_icmp_echo_request(
    id:      u16,
    seq:     u16,
    payload: &[u8],
    out:     &mut [u8],
) -> usize {
    build_icmp_echo(ICMP_ECHO_REQUEST, id, seq, payload, out)
}

/// Build an ICMP echo reply into `out`.
/// Returns the number of bytes written.
pub fn build_icmp_echo_reply(
    id:      u16,
    seq:     u16,
    payload: &[u8],
    out:     &mut [u8],
) -> usize {
    build_icmp_echo(ICMP_ECHO_REPLY, id, seq, payload, out)
}

fn build_icmp_echo(
    icmp_type: u8,
    id:        u16,
    seq:       u16,
    payload:   &[u8],
    out:       &mut [u8],
) -> usize {
    let total = 8 + payload.len();
    if out.len() < total { return 0; }

    out[0] = icmp_type;
    out[1] = 0; // code
    out[2] = 0; // checksum placeholder
    out[3] = 0;
    out[4..6].copy_from_slice(&id.to_be_bytes());
    out[6..8].copy_from_slice(&seq.to_be_bytes());
    out[8..total].copy_from_slice(payload);

    // Compute checksum over entire ICMP message
    let csum = icmp_checksum(&out[0..total]);
    out[2..4].copy_from_slice(&csum.to_be_bytes());

    total
}

/// Parse an ICMP echo reply and verify it matches `expected_id` and `expected_seq`.
/// Returns `true` if the packet is a valid echo reply for us.
pub fn parse_icmp_echo_reply(buf: &[u8], expected_id: u16, expected_seq: u16) -> bool {
    if buf.len() < 8 { return false; }

    let icmp_type = buf[0];
    let _code     = buf[1];
    let id  = u16::from_be_bytes([buf[4], buf[5]]);
    let seq = u16::from_be_bytes([buf[6], buf[7]]);

    icmp_type == ICMP_ECHO_REPLY && id == expected_id && seq == expected_seq
}

/// Standard one's complement checksum (same algorithm as IPv4 header checksum).
pub fn icmp_checksum(data: &[u8]) -> u16 {
    ip_checksum(data)
}
