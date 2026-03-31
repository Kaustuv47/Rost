//! IPv4 packet parsing and building.
//!
//! Header layout (20 bytes minimum, no options):
//!   byte  0     : version (4) | IHL (5 for 20-byte header)
//!   byte  1     : DSCP/ECN (TOS)
//!   bytes 2–3   : total length (big-endian)
//!   bytes 4–5   : identification
//!   bytes 6–7   : flags | fragment offset
//!   byte  8     : TTL
//!   byte  9     : protocol
//!   bytes 10–11 : header checksum
//!   bytes 12–15 : source IP
//!   bytes 16–19 : destination IP

pub const IP_PROTO_ICMP: u8 = 1;
pub const IP_PROTO_TCP:  u8 = 6;
pub const IP_PROTO_UDP:  u8 = 17;

/// Parsed IPv4 header.
#[derive(Clone, Copy)]
pub struct Ipv4Hdr {
    pub ihl:      u8,
    pub tos:      u8,
    pub tot_len:  u16,
    pub id:       u16,
    pub frag_off: u16,
    pub ttl:      u8,
    pub proto:    u8,
    pub checksum: u16,
    pub src:      [u8; 4],
    pub dst:      [u8; 4],
}

/// Parse a raw byte slice as an IPv4 packet.
/// Returns `(header, payload_after_header)` on success.
pub fn parse_ipv4(buf: &[u8]) -> Option<(Ipv4Hdr, &[u8])> {
    if buf.len() < 20 { return None; }

    let version_ihl = buf[0];
    let version = version_ihl >> 4;
    let ihl     = version_ihl & 0x0F;

    if version != 4 { return None; }

    let header_len = (ihl as usize) * 4;
    if buf.len() < header_len { return None; }

    let tot_len  = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    // tot_len includes the header; clamp to actual buffer length
    let pkt_end = tot_len.min(buf.len());
    if pkt_end < header_len { return None; }

    let hdr = Ipv4Hdr {
        ihl,
        tos:      buf[1],
        tot_len:  tot_len as u16,
        id:       u16::from_be_bytes([buf[4], buf[5]]),
        frag_off: u16::from_be_bytes([buf[6], buf[7]]),
        ttl:      buf[8],
        proto:    buf[9],
        checksum: u16::from_be_bytes([buf[10], buf[11]]),
        src:      [buf[12], buf[13], buf[14], buf[15]],
        dst:      [buf[16], buf[17], buf[18], buf[19]],
    };

    Some((hdr, &buf[header_len..pkt_end]))
}

/// Build an IPv4 packet (20-byte header, no options) into `out`.
/// Returns total bytes written (header + payload length).
///
/// A monotonically incrementing ID is used for fragment identification.
/// The checksum is computed automatically.
pub fn build_ipv4(
    proto:   u8,
    src:     [u8; 4],
    dst:     [u8; 4],
    payload: &[u8],
    out:     &mut [u8],
) -> usize {
    let total = 20 + payload.len();
    if out.len() < total { return 0; }

    let tot_len = total as u16;

    // Static packet ID counter
    static mut PKT_ID: u16 = 0;
    let id = unsafe {
        PKT_ID = PKT_ID.wrapping_add(1);
        PKT_ID
    };

    out[0]  = 0x45; // version=4, IHL=5
    out[1]  = 0x00; // DSCP/ECN
    out[2..4].copy_from_slice(&tot_len.to_be_bytes());
    out[4..6].copy_from_slice(&id.to_be_bytes());
    out[6]  = 0x40; // Don't Fragment flag set, frag_off=0
    out[7]  = 0x00;
    out[8]  = 64;   // TTL
    out[9]  = proto;
    out[10] = 0x00; // checksum (filled below)
    out[11] = 0x00;
    out[12..16].copy_from_slice(&src);
    out[16..20].copy_from_slice(&dst);

    // Compute header checksum over the 20-byte header
    let csum = ip_checksum(&out[0..20]);
    out[10..12].copy_from_slice(&csum.to_be_bytes());

    // Copy payload
    out[20..total].copy_from_slice(payload);
    total
}

/// Standard Internet checksum (one's complement sum of 16-bit words).
/// Used for IPv4 header, ICMP, and TCP/UDP pseudo-headers.
pub fn ip_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;

    while i + 1 < data.len() {
        let word = u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        sum += word;
        i += 2;
    }
    // Odd byte — pad with zero
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }

    // Fold 32-bit sum into 16 bits
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    !(sum as u16)
}
