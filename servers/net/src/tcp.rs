//! Minimal TCP implementation — single connection, active open only.
//!
//! TCP header layout (20 bytes minimum):
//!   bytes 0–1   : source port
//!   bytes 2–3   : destination port
//!   bytes 4–7   : sequence number
//!   bytes 8–11  : acknowledgement number
//!   byte  12    : data offset (high nibble, in 32-bit words) | reserved
//!   byte  13    : flags (URG/ACK/PSH/RST/SYN/FIN in bits 5..0)
//!   bytes 14–15 : window size
//!   bytes 16–17 : checksum
//!   bytes 18–19 : urgent pointer
//!   bytes 20+   : options (if data_offset > 5)

use crate::ipv4::ip_checksum;

// ── TCP flags ─────────────────────────────────────────────────────────────────

pub const TCP_FIN: u8 = 0x01;
pub const TCP_SYN: u8 = 0x02;
pub const TCP_RST: u8 = 0x04;
pub const TCP_PSH: u8 = 0x08;
pub const TCP_ACK: u8 = 0x10;
pub const TCP_URG: u8 = 0x20;

// ── TCP connection state machine ──────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TcpState {
    Closed,
    SynSent,
    Established,
    FinWait1,
    /// Closed (after FIN exchange complete) — reuses Closed in practice
    CloseWait,
}

// ── Events returned from handle_tcp ──────────────────────────────────────────

#[derive(Clone, Copy)]
pub enum TcpEvent {
    Established,
    DataReceived,
    Closed,
    Reset,
}

// ── Single TCP connection ─────────────────────────────────────────────────────

pub struct TcpConn {
    pub state:       TcpState,
    pub local_port:  u16,
    pub remote_port: u16,
    pub remote_ip:   [u8; 4],
    /// Our next send sequence number
    pub seq:         u32,
    /// Acknowledgement number (next byte we expect from peer)
    pub ack:         u32,
    pub conn_id:     u8,
    pub owner_pid:   u32,
    pub rx_buf:      [u8; 512],
    pub rx_len:      usize,
}

impl TcpConn {
    pub const fn zeroed() -> Self {
        TcpConn {
            state:       TcpState::Closed,
            local_port:  0,
            remote_port: 0,
            remote_ip:   [0; 4],
            seq:         0,
            ack:         0,
            conn_id:     0,
            owner_pid:   0,
            rx_buf:      [0u8; 512],
            rx_len:      0,
        }
    }
}

static mut TCP_CONN: TcpConn = TcpConn::zeroed();

// Initial sequence number (arbitrary)
const INITIAL_SEQ: u32 = 0x1234_5678;

// ── Public API ────────────────────────────────────────────────────────────────

/// Retrieve a mutable reference to the single TCP connection.
pub fn get_conn() -> &'static mut TcpConn {
    unsafe { &mut TCP_CONN }
}

/// Begin an active TCP open (SYN sent state).
/// Returns the conn_id (always 1) or 0xFF on failure.
pub fn tcp_connect(
    remote_ip:   [u8; 4],
    remote_port: u16,
    local_port:  u16,
    owner_pid:   u32,
) -> u8 {
    let conn = unsafe { &mut TCP_CONN };
    if conn.state != TcpState::Closed {
        return 0xFF; // already in use
    }
    conn.state       = TcpState::SynSent;
    conn.remote_ip   = remote_ip;
    conn.remote_port = remote_port;
    conn.local_port  = local_port;
    conn.seq         = INITIAL_SEQ;
    conn.ack         = 0;
    conn.conn_id     = 1;
    conn.owner_pid   = owner_pid;
    conn.rx_len      = 0;
    1
}

/// Close the TCP connection state (does not send FIN — caller must do that).
pub fn tcp_close() {
    let conn = unsafe { &mut TCP_CONN };
    *conn = TcpConn::zeroed();
}

// ── Packet handler ────────────────────────────────────────────────────────────

/// Process an incoming TCP segment.
/// `buf` is the TCP header + payload (after IP header).
/// Returns `Some(TcpEvent)` if something interesting happened.
pub fn handle_tcp(
    src_ip:  [u8; 4],
    buf:     &[u8],
    our_ip:  [u8; 4],
) -> Option<TcpEvent> {
    let _ = our_ip; // reserved for future filtering
    if buf.len() < 20 { return None; }

    let src_port  = u16::from_be_bytes([buf[0], buf[1]]);
    let dst_port  = u16::from_be_bytes([buf[2], buf[3]]);
    let seq_num   = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let ack_num   = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
    let data_off  = (buf[12] >> 4) as usize * 4;
    let flags     = buf[13];
    let _window   = u16::from_be_bytes([buf[14], buf[15]]);

    let payload = if data_off <= buf.len() { &buf[data_off..] } else { &[] };

    let conn = unsafe { &mut TCP_CONN };

    // Only handle traffic for our connection
    if conn.state == TcpState::Closed { return None; }
    if conn.remote_ip   != src_ip    { return None; }
    if conn.remote_port != src_port  { return None; }
    if conn.local_port  != dst_port  { return None; }

    match conn.state {
        TcpState::SynSent => {
            // Expecting SYN-ACK
            if flags & (TCP_SYN | TCP_ACK) == (TCP_SYN | TCP_ACK) {
                conn.ack = seq_num.wrapping_add(1); // next expected byte
                conn.seq = ack_num;                  // peer ACK'd our SYN
                conn.state = TcpState::Established;
                return Some(TcpEvent::Established);
            }
            if flags & TCP_RST != 0 {
                conn.state = TcpState::Closed;
                return Some(TcpEvent::Reset);
            }
        }
        TcpState::Established => {
            if flags & TCP_RST != 0 {
                conn.state = TcpState::Closed;
                return Some(TcpEvent::Reset);
            }
            if flags & TCP_FIN != 0 {
                conn.ack = seq_num.wrapping_add(1);
                conn.state = TcpState::CloseWait;
                return Some(TcpEvent::Closed);
            }
            // Data segment
            if !payload.is_empty() {
                let copy_len = payload.len().min(conn.rx_buf.len() - conn.rx_len);
                let dst = &mut conn.rx_buf[conn.rx_len..conn.rx_len + copy_len];
                dst.copy_from_slice(&payload[..copy_len]);
                conn.rx_len += copy_len;
                conn.ack = conn.ack.wrapping_add(payload.len() as u32);
                return Some(TcpEvent::DataReceived);
            }
        }
        TcpState::FinWait1 => {
            // We sent FIN; waiting for ACK + peer FIN
            if flags & TCP_ACK != 0 && flags & TCP_FIN != 0 {
                conn.ack = seq_num.wrapping_add(1);
                conn.state = TcpState::Closed;
                return Some(TcpEvent::Closed);
            }
            if flags & TCP_FIN != 0 {
                conn.ack = seq_num.wrapping_add(1);
                conn.state = TcpState::Closed;
                return Some(TcpEvent::Closed);
            }
        }
        TcpState::CloseWait | TcpState::Closed => {}
    }

    None
}

// ── Packet builder ────────────────────────────────────────────────────────────

/// Build a TCP segment into `out` (no IP header).
/// Returns the number of bytes written.
pub fn build_tcp(
    src_p:   u16,
    dst_p:   u16,
    seq:     u32,
    ack:     u32,
    flags:   u8,
    window:  u16,
    payload: &[u8],
    out:     &mut [u8],
) -> usize {
    let total = 20 + payload.len();
    if out.len() < total { return 0; }

    out[0..2].copy_from_slice(&src_p.to_be_bytes());
    out[2..4].copy_from_slice(&dst_p.to_be_bytes());
    out[4..8].copy_from_slice(&seq.to_be_bytes());
    out[8..12].copy_from_slice(&ack.to_be_bytes());
    out[12] = 0x50; // data offset = 5 (20 bytes), reserved = 0
    out[13] = flags;
    out[14..16].copy_from_slice(&window.to_be_bytes());
    out[16] = 0; // checksum placeholder
    out[17] = 0;
    out[18] = 0; // urgent pointer
    out[19] = 0;
    out[20..total].copy_from_slice(payload);

    total
}

/// Compute the TCP checksum with pseudo-header.
///
/// TCP pseudo-header (12 bytes):
///   src_ip(4) + dst_ip(4) + zero(1) + proto=6(1) + tcp_len(2)
pub fn tcp_checksum(src: [u8; 4], dst: [u8; 4], tcp_data: &[u8]) -> u16 {
    let tcp_len = tcp_data.len() as u16;

    // Build pseudo-header + TCP data into a temporary on-stack buffer.
    // Maximum TCP segment we support: 20 (hdr) + 40 (payload) = 60 bytes → pseudo = 72.
    // Use a fixed-size array large enough for our use cases.
    const MAX_PSEUDO: usize = 12 + 20 + 512 + 20; // generous upper bound
    let total = 12 + tcp_data.len();

    if total > MAX_PSEUDO {
        // Fallback: skip checksum (return 0)
        return 0;
    }

    let mut buf = [0u8; MAX_PSEUDO];
    buf[0..4].copy_from_slice(&src);
    buf[4..8].copy_from_slice(&dst);
    buf[8]  = 0;
    buf[9]  = 6; // TCP protocol number
    buf[10..12].copy_from_slice(&tcp_len.to_be_bytes());
    buf[12..12 + tcp_data.len()].copy_from_slice(tcp_data);

    ip_checksum(&buf[..total])
}

/// Fill in the checksum field (bytes 16–17) in an already-built TCP segment.
pub fn fill_tcp_checksum(src: [u8; 4], dst: [u8; 4], tcp_seg: &mut [u8]) {
    if tcp_seg.len() < 20 { return; }
    tcp_seg[16] = 0;
    tcp_seg[17] = 0;
    // We need to compute over the mutable slice, so take a shared ref
    let csum = {
        let shared: &[u8] = tcp_seg;
        tcp_checksum(src, dst, shared)
    };
    tcp_seg[16..18].copy_from_slice(&csum.to_be_bytes());
}
