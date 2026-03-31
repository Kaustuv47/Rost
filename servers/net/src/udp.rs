//! UDP socket layer.
//!
//! UDP header layout (8 bytes):
//!   bytes 0–1 : source port (big-endian)
//!   bytes 2–3 : destination port (big-endian)
//!   bytes 4–5 : length (header + data, big-endian)
//!   bytes 6–7 : checksum (0 = disabled for UDP/IPv4)

const MAX_SOCKETS: usize = 4;

// ── Socket table ──────────────────────────────────────────────────────────────

pub struct UdpSocket {
    pub active:     bool,
    pub bound_port: u16,
    pub owner_pid:  u32,
    /// Most recently received datagram
    pub rx_ip:      [u8; 4],
    pub rx_port:    u16,
    pub rx_data:    [u8; 48],
    pub rx_len:     usize,
    pub has_data:   bool,
}

impl UdpSocket {
    const fn zeroed() -> Self {
        UdpSocket {
            active:     false,
            bound_port: 0,
            owner_pid:  0,
            rx_ip:      [0; 4],
            rx_port:    0,
            rx_data:    [0; 48],
            rx_len:     0,
            has_data:   false,
        }
    }
}

static mut UDP_SOCKETS: [UdpSocket; MAX_SOCKETS] = [
    UdpSocket::zeroed(),
    UdpSocket::zeroed(),
    UdpSocket::zeroed(),
    UdpSocket::zeroed(),
];

// ── Public API ────────────────────────────────────────────────────────────────

/// Bind `port` to process `pid`. Returns `false` if the port is already taken
/// or the socket table is full.
pub fn bind(port: u16, pid: u32) -> bool {
    unsafe {
        // Check for duplicate
        for s in UDP_SOCKETS.iter() {
            if s.active && s.bound_port == port {
                return false;
            }
        }
        // Find free slot
        for s in UDP_SOCKETS.iter_mut() {
            if !s.active {
                s.active     = true;
                s.bound_port = port;
                s.owner_pid  = pid;
                s.has_data   = false;
                s.rx_len     = 0;
                return true;
            }
        }
        false
    }
}

/// Release a binding for `port`.
pub fn unbind(port: u16) {
    unsafe {
        for s in UDP_SOCKETS.iter_mut() {
            if s.active && s.bound_port == port {
                *s = UdpSocket::zeroed();
                return;
            }
        }
    }
}

/// Deliver an incoming UDP datagram to the bound socket for `dst_port`.
/// Overwrites any unread datagram (simple single-slot buffer).
pub fn deliver(src_ip: [u8; 4], src_port: u16, dst_port: u16, data: &[u8]) {
    unsafe {
        for s in UDP_SOCKETS.iter_mut() {
            if s.active && s.bound_port == dst_port {
                let copy_len = data.len().min(s.rx_data.len());
                s.rx_data[..copy_len].copy_from_slice(&data[..copy_len]);
                s.rx_len   = copy_len;
                s.rx_ip    = src_ip;
                s.rx_port  = src_port;
                s.has_data = true;
                return;
            }
        }
    }
}

/// Check if the socket bound to `port` has a pending datagram.
/// Returns `Some((src_ip, src_port, data_slice))` and clears the pending flag.
pub fn get_pending(port: u16) -> Option<([u8; 4], u16, usize, [u8; 48])> {
    unsafe {
        for s in UDP_SOCKETS.iter_mut() {
            if s.active && s.bound_port == port && s.has_data {
                s.has_data = false;
                return Some((s.rx_ip, s.rx_port, s.rx_len, s.rx_data));
            }
        }
    }
    None
}

// ── Packet building ───────────────────────────────────────────────────────────

/// Build a UDP header + payload into `out`.
/// Returns total bytes written (8 + payload.len()).
/// Checksum is set to 0 (optional for IPv4).
pub fn build_udp(
    src_port: u16,
    dst_port: u16,
    payload:  &[u8],
    out:      &mut [u8],
) -> usize {
    let total = 8 + payload.len();
    if out.len() < total { return 0; }

    let length = total as u16;
    out[0..2].copy_from_slice(&src_port.to_be_bytes());
    out[2..4].copy_from_slice(&dst_port.to_be_bytes());
    out[4..6].copy_from_slice(&length.to_be_bytes());
    out[6] = 0; // checksum high byte (disabled)
    out[7] = 0; // checksum low byte
    out[8..total].copy_from_slice(payload);
    total
}

/// Parse a UDP header. Returns `(src_port, dst_port, payload_slice)`.
pub fn parse_udp(buf: &[u8]) -> Option<(u16, u16, &[u8])> {
    if buf.len() < 8 { return None; }
    let src_port = u16::from_be_bytes([buf[0], buf[1]]);
    let dst_port = u16::from_be_bytes([buf[2], buf[3]]);
    let length   = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    if length < 8 || buf.len() < length { return None; }
    Some((src_port, dst_port, &buf[8..length]))
}
