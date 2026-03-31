//! Ethernet II frame parsing and building.
//!
//! Frame layout:
//!   bytes 0–5  : destination MAC
//!   bytes 6–11 : source MAC
//!   bytes 12–13: EtherType (big-endian)
//!   bytes 14+  : payload

pub const ETH_ARP:  u16 = 0x0806;
pub const ETH_IPV4: u16 = 0x0800;

/// Parsed Ethernet II frame.
pub struct EthFrame<'a> {
    pub dst:       [u8; 6],
    pub src:       [u8; 6],
    pub ethertype: u16,
    pub payload:   &'a [u8],
}

/// Parse a raw byte slice as an Ethernet II frame.
/// Returns `None` if the buffer is shorter than 14 bytes.
pub fn parse_eth(buf: &[u8]) -> Option<EthFrame<'_>> {
    if buf.len() < 14 {
        return None;
    }
    let mut dst = [0u8; 6];
    let mut src = [0u8; 6];
    dst.copy_from_slice(&buf[0..6]);
    src.copy_from_slice(&buf[6..12]);
    let ethertype = u16::from_be_bytes([buf[12], buf[13]]);
    Some(EthFrame {
        dst,
        src,
        ethertype,
        payload: &buf[14..],
    })
}

/// Build an Ethernet II frame into `out`.
/// Returns the total number of bytes written.
/// `out` must be at least `14 + payload.len()` bytes long.
pub fn build_eth(
    dst:     [u8; 6],
    src:     [u8; 6],
    etype:   u16,
    payload: &[u8],
    out:     &mut [u8],
) -> usize {
    let total = 14 + payload.len();
    if out.len() < total { return 0; }

    out[0..6].copy_from_slice(&dst);
    out[6..12].copy_from_slice(&src);
    out[12..14].copy_from_slice(&etype.to_be_bytes());
    out[14..total].copy_from_slice(payload);
    total
}

/// The broadcast MAC address.
pub const MAC_BROADCAST: [u8; 6] = [0xFF; 6];
