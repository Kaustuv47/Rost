//! rost-net — ring-3 network stack server for the Rost microkernel.
//!
//! PID assignment: registered as "rost-net" in the service registry.
//!
//! Handles:
//!   - Virtio-net legacy PCI NIC driver
//!   - ARP (request/reply/cache)
//!   - IPv4 (TX/RX)
//!   - ICMP echo (ping client + server)
//!   - UDP (bind/send/recv)
//!   - TCP (single active connection)
//!
//! IPC opcodes: see socket.rs (OP_NET_PING=0x0100 … OP_NET_GET_IP=0x0108)
#![no_std]
#![no_main]

mod arp;
mod eth;
mod icmp;
mod ipv4;
mod pci;
mod socket;
mod syscall;
mod tcp;
mod udp;
mod virtio;

use core::ptr::addr_of_mut;

use arp::{build_arp_reply, build_arp_request, handle_arp, lookup_mac};
use eth::{build_eth, parse_eth, ETH_ARP, ETH_IPV4};
use icmp::{
    build_icmp_echo_reply, build_icmp_echo_request,
    ICMP_ECHO_REQUEST,
};
use ipv4::{build_ipv4, parse_ipv4, IP_PROTO_ICMP, IP_PROTO_TCP, IP_PROTO_UDP};
use socket::*;
use syscall::{clock, exit, getpid, irq_register, print, recv_msg, register, yield_, Msg};
use tcp::{
    build_tcp, fill_tcp_checksum, get_conn, handle_tcp, tcp_close, tcp_connect, TcpEvent,
    TcpState, TCP_ACK, TCP_FIN, TCP_SYN,
};
use udp::{build_udp, deliver, parse_udp};
use virtio::VirtioNet;

// ── Network configuration ─────────────────────────────────────────────────────

const OUR_IP:  [u8; 4] = [10, 0, 2, 15];
const GATEWAY: [u8; 4] = [10, 0, 2, 2];

// Ephemeral port base for outgoing TCP connections
const EPHEMERAL_PORT_BASE: u16 = 49152;

// ── Hex digit table (used in virtio.rs via crate::HEX_DIGITS) ────────────────
pub static HEX_DIGITS: &[u8] = b"0123456789abcdef";

// ── Packet buffers ────────────────────────────────────────────────────────────

/// Shared transmit frame buffer (Ethernet + IP + transport + payload).
static mut PKTBUF: [u8; 1536] = [0u8; 1536];

/// Scratch RX buffer used during blocking ARP/ICMP resolution loops.
static mut RX_SCRATCH: [u8; 1520] = [0u8; 1520];

// ── Async TCP-connect state machine ──────────────────────────────────────────
//
// State transitions:
//   Idle → WaitARP   (on OP_NET_TCP_CONNECT, ARP miss)
//   Idle → WaitSynAck(on OP_NET_TCP_CONNECT, ARP hit: SYN sent)
//   WaitARP → WaitSynAck (on ARP reply: send SYN)
//   WaitSynAck → Idle    (on TCP Established / Reset, or on deadline)
//   WaitARP → Idle       (on deadline)

const TCP_SM_IDLE:        u8 = 0;
const TCP_SM_WAIT_ARP:    u8 = 1;
const TCP_SM_WAIT_SYNACK: u8 = 2;

static mut TCP_SM_STATE:    u8  = TCP_SM_IDLE;
static mut TCP_SM_SENDER:   u32 = 0;
static mut TCP_SM_DST_IP:   u32 = 0; // little-endian
static mut TCP_SM_DST_PORT: u16 = 0;
static mut TCP_SM_CONN_ID:  u8  = 0;
static mut TCP_SM_DEADLINE: u64 = 0;
/// Resolved destination MAC (filled when leaving WaitARP).
static mut TCP_SM_DST_MAC: [u8; 6] = [0u8; 6];

// ── Async ping state machine ──────────────────────────────────────────────────
//
// State transitions:
//   Idle → WaitARP  (on OP_NET_PING, ARP miss)
//   Idle → WaitICMP (on OP_NET_PING, ARP hit)
//   WaitARP  → WaitICMP  (on ARP reply that resolves pending target)
//   WaitICMP → Idle      (on ICMP echo reply matching id+seq, or on deadline)
//   WaitARP  → Idle      (on deadline)

const PING_IDLE:     u8 = 0;
const PING_WAIT_ARP: u8 = 1;
const PING_WAIT_ICMP:u8 = 2;

static mut PENDING_STATE:     u8       = PING_IDLE;
/// PID that issued the pending OP_NET_PING.
static mut PENDING_SENDER:    u32      = 0;
/// Target IP packed as little-endian u32.
static mut PENDING_TARGET_IP: u32      = 0;
/// Absolute ns deadline for the pending operation.
static mut PENDING_DEADLINE:  u64      = 0;
/// ICMP echo identifier for the pending ping.
static mut PENDING_ICMP_ID:   u16      = 0xBEEF;
/// ICMP echo sequence for the pending ping.
static mut PENDING_ICMP_SEQ:  u16      = 0;
/// Clock value when the ICMP echo request was sent (for RTT calculation).
static mut PENDING_SENT_NS:   u64      = 0;

// ── Entry point ───────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _start() -> ! {
    print(b"[net] rost-net starting...\n");

    register(b"rost-net\0\0\0\0\0\0\0\0");

    let _my_pid = getpid();

    // Initialise the virtio-net driver
    let mut vnet_opt = virtio::init();
    if vnet_opt.is_none() {
        print(b"[net] ERROR: virtio-net device not found\n");
    }

    // Register for hardware interrupts from the virtio-net NIC.
    // The kernel will route the IOAPIC GSI to IDT vector (32+GSI) and,
    // on each interrupt, deliver an IPC message with data[0] = 0xFFFF_0000|gsi.
    if let Some(ref vnet) = vnet_opt {
        let (irq, isr_port) = vnet.irq_info();
        if irq >= 8 && irq <= 15 {
            if irq_register(irq, isr_port) {
                print(b"[net] IRQ registered (interrupt-driven RX)\n");
            } else {
                print(b"[net] IRQ register failed (polling fallback)\n");
            }
        } else {
            print(b"[net] IRQ out of range (polling fallback)\n");
        }
    }

    print(b"[net] entering main loop\n");

    let mut msg = Msg::zeroed();

    loop {
        // Block for up to 1 s (100 ticks at 100 Hz) waiting for an IPC message
        // or a hardware-IRQ notification from the kernel.
        let got_msg = recv_msg(100, &mut msg);

        if got_msg {
            // Check for kernel-delivered hardware IRQ notification:
            //   data[0] = 0xFFFF_0000 | gsi
            if (msg.data[0] >> 16) == 0xFFFF {
                // IRQ fired — drain the RX ring immediately
                if let Some(ref mut vnet) = vnet_opt {
                    poll_rx(vnet);
                }
            } else if let Some(ref mut vnet) = vnet_opt {
                handle_ipc(&msg, vnet);
            } else {
                let op = msg.data[0];
                reply_err(msg.sender, op);
            }
        } else {
            // Timeout — fallback poll in case an IRQ was missed
            if let Some(ref mut vnet) = vnet_opt {
                poll_rx(vnet);
            }
        }

        // Advance the async ping state machine (check deadlines, send replies).
        if let Some(ref mut vnet) = vnet_opt {
            check_pending_deadlines(vnet);
        }

        yield_();
    }
}

// ── RX polling ────────────────────────────────────────────────────────────────

fn poll_rx(vnet: &mut VirtioNet) {
    // Drain all available frames
    loop {
        let n = unsafe {
            let buf = &mut *addr_of_mut!(RX_SCRATCH);
            vnet.recv_packet_into(buf)
        };
        match n {
            Some(len) => {
                let frame = unsafe { &(&*addr_of_mut!(RX_SCRATCH))[..len] };
                handle_packet(frame, vnet);
            }
            None => break,
        }
    }
}

// ── IPC dispatcher ────────────────────────────────────────────────────────────

fn handle_ipc(msg: &Msg, vnet: &mut VirtioNet) {
    let op     = msg.data[0];
    let sender = msg.sender;

    match op {
        OP_NET_GET_IP => {
            // Return our IP as a little-endian u32 packed into data[1]
            let ip_u32 = u32::from_le_bytes(OUR_IP);
            reply_data(sender, OP_NET_GET_IP, ip_u32 as u64);
        }

        OP_NET_PING => {
            handle_ping(msg, vnet);
        }

        OP_NET_UDP_BIND => {
            let port = msg.data[1] as u16;
            let ok   = udp::bind(port, sender);
            reply_data(sender, OP_NET_UDP_BIND, if ok { 0 } else { 1 });
        }

        OP_NET_UDP_SEND => {
            handle_udp_send(msg, vnet);
        }

        OP_NET_UDP_RECV => {
            let port = msg.data[1] as u16;
            match udp::get_pending(port) {
                Some((src_ip, src_port, rx_len, rx_data)) => {
                    let mut reply = Msg::zeroed();
                    reply.data[0] = OP_NET_UDP_RECV;
                    reply.data[1] = u32::from_le_bytes(src_ip) as u64;
                    reply.data[2] = src_port as u64;
                    // Pack up to 48 bytes of payload into data[3..7] (40 bytes of msg space)
                    let copy_len = rx_len.min(40);
                    pack_bytes_into_words(&rx_data[..copy_len], &mut reply.data[3..8]);
                    reply.data[7] = (reply.data[7] & 0xFFFF_FFFF_0000_0000) | (copy_len as u64);
                    send_reply(sender, &reply);
                }
                None => {
                    // No data available — reply with empty (caller should retry)
                    reply_data(sender, OP_NET_UDP_RECV, 0xFFFF);
                }
            }
        }

        OP_NET_TCP_CONNECT => {
            handle_tcp_connect(msg, vnet);
        }

        OP_NET_TCP_SEND => {
            handle_tcp_send(msg, vnet);
        }

        OP_NET_TCP_RECV => {
            let conn = get_conn();
            if conn.rx_len > 0 {
                let mut reply = Msg::zeroed();
                reply.data[0] = OP_NET_TCP_RECV;
                let copy_len = conn.rx_len.min(40);
                reply.data[1] = copy_len as u64;
                pack_bytes_into_words(&conn.rx_buf[..copy_len], &mut reply.data[2..7]);
                conn.rx_len = 0;
                send_reply(sender, &reply);
            } else {
                reply_data(sender, OP_NET_TCP_RECV, 0xFFFF);
            }
        }

        OP_NET_TCP_CLOSE => {
            handle_tcp_close(msg, vnet);
            reply_data(sender, OP_NET_TCP_CLOSE, 0);
        }

        _ => {
            // Unknown opcode — reply with error
            reply_err(sender, op);
        }
    }
}

// ── Ping implementation (non-blocking, interrupt-driven) ─────────────────────
//
// OP_NET_PING no longer spins — it starts the state machine and returns.
// The reply is delivered asynchronously when the ARP reply and ICMP echo
// reply arrive (driven by hardware IRQ → poll_rx → handle_packet).

fn handle_ping(msg: &Msg, vnet: &mut VirtioNet) {
    let sender    = msg.sender;
    let ip_raw    = msg.data[1] as u32;
    let target_ip = ip_raw.to_le_bytes();

    // Reject if another ping is already in flight
    if unsafe { PENDING_STATE } != PING_IDLE {
        reply_data(sender, OP_NET_PING, 0xFFFE); // busy
        return;
    }

    let deadline = clock() + 2_000_000_000; // 2-second timeout

    // Fast path: ARP cache hit → skip ARP phase, go straight to WaitICMP
    if let Some(dst_mac) = lookup_mac(target_ip) {
        let (id, seq) = advance_ping_seq();
        let sent_ns   = clock();
        if !build_and_send_icmp_request(target_ip, dst_mac, vnet, id, seq) {
            reply_data(sender, OP_NET_PING, 0xFFFF);
            return;
        }
        unsafe {
            PENDING_STATE     = PING_WAIT_ICMP;
            PENDING_SENDER    = sender;
            PENDING_TARGET_IP = ip_raw;
            PENDING_DEADLINE  = deadline;
            PENDING_ICMP_ID   = id;
            PENDING_ICMP_SEQ  = seq;
            PENDING_SENT_NS   = sent_ns;
        }
        return;
    }

    // Slow path: ARP miss → send ARP request, wait for reply
    let pktbuf = unsafe { &mut *addr_of_mut!(PKTBUF) };
    let len = build_arp_request(vnet.mac, OUR_IP, target_ip, pktbuf);
    if len == 0 {
        reply_data(sender, OP_NET_PING, 0xFFFF);
        return;
    }
    vnet.send_packet(&pktbuf[..len]);

    unsafe {
        PENDING_STATE     = PING_WAIT_ARP;
        PENDING_SENDER    = sender;
        PENDING_TARGET_IP = ip_raw;
        PENDING_DEADLINE  = deadline;
    }
}

/// Advance and return the next (id, seq) for an outgoing ICMP echo request.
#[inline]
fn advance_ping_seq() -> (u16, u16) {
    unsafe {
        PENDING_ICMP_SEQ = PENDING_ICMP_SEQ.wrapping_add(1);
        (PENDING_ICMP_ID, PENDING_ICMP_SEQ)
    }
}

/// Check whether the pending ping has exceeded its deadline.
/// Called once per main-loop iteration.
fn check_pending_deadlines(_vnet: &mut VirtioNet) {
    let now = clock();

    // ── Ping deadline ─────────────────────────────────────────────────────────
    if unsafe { PENDING_STATE } != PING_IDLE && now >= unsafe { PENDING_DEADLINE } {
        let sender = unsafe { PENDING_SENDER };
        unsafe { PENDING_STATE = PING_IDLE; }
        reply_data(sender, OP_NET_PING, 0xFFFF); // timeout
    }

    // ── TCP connect deadline ──────────────────────────────────────────────────
    if unsafe { TCP_SM_STATE } != TCP_SM_IDLE && now >= unsafe { TCP_SM_DEADLINE } {
        let sender = unsafe { TCP_SM_SENDER };
        unsafe { TCP_SM_STATE = TCP_SM_IDLE; }
        tcp_close();
        reply_data(sender, OP_NET_TCP_CONNECT, 0xFF); // timeout
    }
}

fn build_and_send_icmp_request(
    target_ip: [u8; 4],
    dst_mac:   [u8; 6],
    vnet:      &mut VirtioNet,
    id:        u16,
    seq:       u16,
) -> bool {
    // ICMP payload: 8 bytes of arbitrary data
    let icmp_payload = b"rost-net";

    // Build ICMP into a local stack buffer
    let mut icmp_buf = [0u8; 64];
    let icmp_len = build_icmp_echo_request(id, seq, icmp_payload, &mut icmp_buf);

    // Build IPv4 packet
    let mut ip_buf = [0u8; 128];
    let ip_len = build_ipv4(
        IP_PROTO_ICMP,
        OUR_IP,
        target_ip,
        &icmp_buf[..icmp_len],
        &mut ip_buf,
    );

    // Build Ethernet frame into PKTBUF
    let pktbuf = unsafe { &mut *addr_of_mut!(PKTBUF) };
    let eth_len = build_eth(dst_mac, vnet.mac, ETH_IPV4, &ip_buf[..ip_len], pktbuf);
    if eth_len == 0 { return false; }

    vnet.send_packet(&pktbuf[..eth_len])
}

// ── ARP resolution ────────────────────────────────────────────────────────────

/// Look up a target IP in the ARP cache.
///
/// If the cache misses, send an ARP request and return `None` — the ARP reply
/// will arrive asynchronously via the IRQ-driven RX path and populate the
/// cache.  The caller should reply `EAGAIN` (0xFFFD) so the client can retry
/// after a short wait.
///
/// This is non-blocking: no polling loop, no 2-second stall.
fn resolve_mac(target_ip: [u8; 4], vnet: &mut VirtioNet) -> Option<[u8; 6]> {
    if let Some(mac) = lookup_mac(target_ip) {
        return Some(mac);
    }
    // Cache miss — kick an ARP request; reply arrives via IRQ → poll_rx
    let pktbuf = unsafe { &mut *addr_of_mut!(PKTBUF) };
    let len = build_arp_request(vnet.mac, OUR_IP, target_ip, pktbuf);
    if len > 0 { vnet.send_packet(&pktbuf[..len]); }
    None
}

// ── UDP send ──────────────────────────────────────────────────────────────────

fn handle_udp_send(msg: &Msg, vnet: &mut VirtioNet) {
    let sender   = msg.sender;
    let dst_ip_u32 = msg.data[1] as u32;
    let dst_ip   = dst_ip_u32.to_le_bytes();
    let ports    = msg.data[2]; // dst_port in low 16, src_port in high 16
    let dst_port = (ports & 0xFFFF) as u16;
    let src_port = ((ports >> 16) & 0xFFFF) as u16;
    let payload_len = (msg.data[7] & 0xFFFF) as usize;
    let payload_len = payload_len.min(40);

    // Unpack payload from data[3..7]
    let mut payload_buf = [0u8; 40];
    unpack_words_to_bytes(&msg.data[3..8], &mut payload_buf);
    let payload = &payload_buf[..payload_len];

    // Resolve destination MAC (non-blocking; EAGAIN if ARP cache miss)
    let dst_mac = match resolve_mac(dst_ip, vnet) {
        Some(m) => m,
        None => {
            // ARP request kicked; client should retry after ~20 ms
            reply_data(sender, OP_NET_UDP_SEND, 0xFFFD); // EAGAIN
            return;
        }
    };

    // Build UDP
    let mut udp_buf = [0u8; 64];
    let udp_len = build_udp(src_port, dst_port, payload, &mut udp_buf);

    // Build IPv4
    let mut ip_buf = [0u8; 128];
    let ip_len = build_ipv4(IP_PROTO_UDP, OUR_IP, dst_ip, &udp_buf[..udp_len], &mut ip_buf);

    // Build Ethernet
    let pktbuf = unsafe { &mut *addr_of_mut!(PKTBUF) };
    let eth_len = build_eth(dst_mac, vnet.mac, ETH_IPV4, &ip_buf[..ip_len], pktbuf);

    if eth_len > 0 && vnet.send_packet(&pktbuf[..eth_len]) {
        reply_data(sender, OP_NET_UDP_SEND, 0);
    } else {
        reply_err(sender, OP_NET_UDP_SEND);
    }
}

// ── TCP connect (non-blocking, interrupt-driven) ──────────────────────────────
//
// OP_NET_TCP_CONNECT starts the TCP-connect state machine and returns
// immediately.  The reply is delivered asynchronously when the SYN-ACK
// arrives via the IRQ-driven RX path (handle_tcp_incoming → tcp_sm_on_established).

fn handle_tcp_connect(msg: &Msg, vnet: &mut VirtioNet) {
    let sender     = msg.sender;
    let dst_ip_u32 = msg.data[1] as u32;
    let dst_ip     = dst_ip_u32.to_le_bytes();
    let dst_port   = msg.data[2] as u16;
    let local_port = EPHEMERAL_PORT_BASE;

    // Reject if another connect is already in flight
    if unsafe { TCP_SM_STATE } != TCP_SM_IDLE {
        reply_data(sender, OP_NET_TCP_CONNECT, 0xFF);
        return;
    }

    let conn_id = tcp_connect(dst_ip, dst_port, local_port, sender);
    if conn_id == 0xFF {
        reply_data(sender, OP_NET_TCP_CONNECT, 0xFF);
        return;
    }

    let deadline = clock() + 5_000_000_000; // 5-second timeout

    // Fast path: MAC already in ARP cache → send SYN immediately
    if let Some(dst_mac) = resolve_mac(dst_ip, vnet) {
        if tcp_sm_send_syn(dst_ip, dst_mac, vnet) {
            unsafe {
                TCP_SM_STATE    = TCP_SM_WAIT_SYNACK;
                TCP_SM_SENDER   = sender;
                TCP_SM_DST_IP   = dst_ip_u32;
                TCP_SM_DST_PORT = dst_port;
                TCP_SM_CONN_ID  = conn_id;
                TCP_SM_DEADLINE = deadline;
                TCP_SM_DST_MAC  = dst_mac;
            }
        } else {
            tcp_close();
            reply_data(sender, OP_NET_TCP_CONNECT, 0xFF);
        }
        return;
    }

    // Slow path: ARP miss → wait for ARP reply, then send SYN
    unsafe {
        TCP_SM_STATE    = TCP_SM_WAIT_ARP;
        TCP_SM_SENDER   = sender;
        TCP_SM_DST_IP   = dst_ip_u32;
        TCP_SM_DST_PORT = dst_port;
        TCP_SM_CONN_ID  = conn_id;
        TCP_SM_DEADLINE = deadline;
    }
}

/// Send a TCP SYN to dst_ip/dst_mac.  Returns true on success.
fn tcp_sm_send_syn(dst_ip: [u8; 4], dst_mac: [u8; 6], vnet: &mut VirtioNet) -> bool {
    let conn = get_conn();
    let mut tcp_buf = [0u8; 64];
    let tcp_len = build_tcp(
        conn.local_port,
        conn.remote_port,
        conn.seq,
        0,
        TCP_SYN,
        65535,
        &[],
        &mut tcp_buf,
    );
    fill_tcp_checksum(OUR_IP, dst_ip, &mut tcp_buf[..tcp_len]);
    conn.seq = conn.seq.wrapping_add(1);

    let mut ip_buf = [0u8; 128];
    let ip_len = build_ipv4(IP_PROTO_TCP, OUR_IP, dst_ip, &tcp_buf[..tcp_len], &mut ip_buf);
    let pktbuf = unsafe { &mut *addr_of_mut!(PKTBUF) };
    let eth_len = build_eth(dst_mac, vnet.mac, ETH_IPV4, &ip_buf[..ip_len], pktbuf);
    eth_len > 0 && vnet.send_packet(&pktbuf[..eth_len])
}

/// Called by handle_arp_packet when the TCP-connect SM is in WaitARP and
/// the ARP reply resolves our pending destination.
fn tcp_sm_on_arp_resolved(dst_ip: [u8; 4], dst_mac: [u8; 6], vnet: &mut VirtioNet) {
    if tcp_sm_send_syn(dst_ip, dst_mac, vnet) {
        unsafe {
            TCP_SM_STATE   = TCP_SM_WAIT_SYNACK;
            TCP_SM_DST_MAC = dst_mac;
        }
    } else {
        let sender = unsafe { TCP_SM_SENDER };
        unsafe { TCP_SM_STATE = TCP_SM_IDLE; }
        tcp_close();
        reply_data(sender, OP_NET_TCP_CONNECT, 0xFF);
    }
}

/// Called by handle_tcp_incoming when a TCP event occurs while the SM is
/// in WaitSynAck.
fn tcp_sm_on_established(vnet: &mut VirtioNet) {
    let sender   = unsafe { TCP_SM_SENDER };
    let conn_id  = unsafe { TCP_SM_CONN_ID };
    let dst_ip   = unsafe { TCP_SM_DST_IP  }.to_le_bytes();
    let dst_mac  = unsafe { TCP_SM_DST_MAC };
    unsafe { TCP_SM_STATE = TCP_SM_IDLE; }

    let c = get_conn();
    send_tcp_packet(
        dst_ip, dst_mac,
        c.local_port, c.remote_port,
        c.seq, c.ack,
        TCP_ACK, 65535, &[],
        vnet,
    );
    reply_data(sender, OP_NET_TCP_CONNECT, conn_id as u64);
}

fn tcp_sm_on_reset() {
    let sender = unsafe { TCP_SM_SENDER };
    unsafe { TCP_SM_STATE = TCP_SM_IDLE; }
    tcp_close();
    reply_data(sender, OP_NET_TCP_CONNECT, 0xFF);
}

// ── TCP send ──────────────────────────────────────────────────────────────────

fn handle_tcp_send(msg: &Msg, vnet: &mut VirtioNet) {
    let sender   = msg.sender;
    let _conn_id = msg.data[1] as u8;
    let length   = (msg.data[2] as usize).min(40);

    let mut data_buf = [0u8; 40];
    unpack_words_to_bytes(&msg.data[3..8], &mut data_buf);
    let payload = &data_buf[..length];

    let conn = get_conn();
    if conn.state != TcpState::Established {
        reply_err(sender, OP_NET_TCP_SEND);
        return;
    }

    let dst_ip  = conn.remote_ip;
    let dst_mac = match lookup_mac(dst_ip) {
        Some(m) => m,
        None    => { reply_err(sender, OP_NET_TCP_SEND); return; }
    };

    let local_port  = conn.local_port;
    let remote_port = conn.remote_port;
    let seq = conn.seq;
    let ack = conn.ack;
    conn.seq = conn.seq.wrapping_add(payload.len() as u32);

    send_tcp_packet(
        dst_ip,
        dst_mac,
        local_port,
        remote_port,
        seq,
        ack,
        TCP_ACK | tcp::TCP_PSH,
        65535,
        payload,
        vnet,
    );

    reply_data(sender, OP_NET_TCP_SEND, 0);
}

// ── TCP close ─────────────────────────────────────────────────────────────────

fn handle_tcp_close(msg: &Msg, vnet: &mut VirtioNet) {
    let _conn_id = msg.data[1] as u8;
    let conn = get_conn();
    if conn.state == TcpState::Established {
        let dst_ip  = conn.remote_ip;
        let dst_mac = lookup_mac(dst_ip).unwrap_or([0xFF; 6]);

        let local_port  = conn.local_port;
        let remote_port = conn.remote_port;
        let seq = conn.seq;
        let ack = conn.ack;
        conn.seq = conn.seq.wrapping_add(1);
        conn.state = TcpState::FinWait1;

        send_tcp_packet(
            dst_ip,
            dst_mac,
            local_port,
            remote_port,
            seq,
            ack,
            TCP_FIN | TCP_ACK,
            65535,
            &[],
            vnet,
        );
    }
    tcp_close();
}

// ── Packet reception handler ──────────────────────────────────────────────────

fn handle_packet(frame: &[u8], vnet: &mut VirtioNet) {
    let eth = match parse_eth(frame) {
        Some(e) => e,
        None    => return,
    };

    match eth.ethertype {
        ETH_ARP  => handle_arp_packet(eth.payload, eth.src, vnet),
        ETH_IPV4 => {
            if let Some((iph, payload)) = parse_ipv4(eth.payload) {
                handle_ipv4_packet(iph, payload, eth.src, vnet);
            }
        }
        _ => {}
    }
}

fn handle_arp_packet(payload: &[u8], _eth_src: [u8; 6], vnet: &mut VirtioNet) {
    // handle_arp stores the sender in the ARP cache and returns Some if it's
    // a request for our IP or a reply to us.
    if let Some((req_ip, req_mac)) = handle_arp(payload, vnet.mac, OUR_IP) {
        // Check if this is a request for our IP (opcode 1, target = our IP)
        // build_arp_reply sends a unicast reply to req_mac
        let pktbuf = unsafe { &mut *addr_of_mut!(PKTBUF) };
        let len = build_arp_reply(vnet.mac, OUR_IP, req_mac, req_ip, pktbuf);
        if len > 0 {
            // Only send ARP reply if the ARP was a request (not a reply)
            // We check: the first two bytes of payload are hw_type (0x0001),
            // bytes 6–7 are operation.
            if payload.len() >= 8 {
                let op = u16::from_be_bytes([payload[6], payload[7]]);
                if op == 1 {
                    // ARP request — send reply
                    vnet.send_packet(&pktbuf[..len]);
                }
            }
        }
    }

    // ── Async ping state machine: ARP reply may resolve our pending target ────
    if unsafe { PENDING_STATE } == PING_WAIT_ARP {
        let pending_ip_raw = unsafe { PENDING_TARGET_IP };
        let pending_ip     = pending_ip_raw.to_le_bytes();

        if let Some(dst_mac) = lookup_mac(pending_ip) {
            let (id, seq) = advance_ping_seq();
            let sent_ns   = clock();
            if build_and_send_icmp_request(pending_ip, dst_mac, vnet, id, seq) {
                unsafe {
                    PENDING_STATE    = PING_WAIT_ICMP;
                    PENDING_ICMP_ID  = id;
                    PENDING_ICMP_SEQ = seq;
                    PENDING_SENT_NS  = sent_ns;
                }
            } else {
                let sender = unsafe { PENDING_SENDER };
                unsafe { PENDING_STATE = PING_IDLE; }
                reply_data(sender, OP_NET_PING, 0xFFFF);
            }
        }
    }

    // ── TCP-connect state machine: ARP reply may resolve pending destination ─
    if unsafe { TCP_SM_STATE } == TCP_SM_WAIT_ARP {
        let tcp_ip_raw = unsafe { TCP_SM_DST_IP };
        let tcp_ip     = tcp_ip_raw.to_le_bytes();

        if let Some(dst_mac) = lookup_mac(tcp_ip) {
            tcp_sm_on_arp_resolved(tcp_ip, dst_mac, vnet);
        }
    }
}

fn handle_ipv4_packet(
    iph:     ipv4::Ipv4Hdr,
    payload: &[u8],
    _eth_src: [u8; 6],
    vnet:    &mut VirtioNet,
) {
    // Only process packets destined for us or broadcast
    let broadcast_ip: [u8; 4] = [255, 255, 255, 255];
    if iph.dst != OUR_IP && iph.dst != broadcast_ip {
        return;
    }

    match iph.proto {
        IP_PROTO_ICMP => handle_icmp_packet(iph.src, payload, vnet),
        IP_PROTO_UDP  => handle_udp_packet(iph.src, payload),
        IP_PROTO_TCP  => handle_tcp_incoming(iph.src, payload, vnet),
        _             => {}
    }
}

fn handle_icmp_packet(src_ip: [u8; 4], payload: &[u8], vnet: &mut VirtioNet) {
    if payload.is_empty() { return; }
    let icmp_type = payload[0];

    match icmp_type {
        ICMP_ECHO_REQUEST => {
            // Respond to incoming pings
            if payload.len() < 8 { return; }
            let id  = u16::from_be_bytes([payload[4], payload[5]]);
            let seq = u16::from_be_bytes([payload[6], payload[7]]);
            let ping_data = if payload.len() > 8 { &payload[8..] } else { b"" };

            let dst_mac = match lookup_mac(src_ip) {
                Some(m) => m,
                None    => return,
            };

            let mut icmp_buf = [0u8; 64];
            let icmp_len = build_icmp_echo_reply(id, seq, ping_data, &mut icmp_buf);

            let mut ip_buf = [0u8; 128];
            let ip_len = build_ipv4(
                IP_PROTO_ICMP,
                OUR_IP,
                src_ip,
                &icmp_buf[..icmp_len],
                &mut ip_buf,
            );

            let pktbuf = unsafe { &mut *addr_of_mut!(PKTBUF) };
            let eth_len = build_eth(dst_mac, vnet.mac, ETH_IPV4, &ip_buf[..ip_len], pktbuf);
            if eth_len > 0 {
                vnet.send_packet(&pktbuf[..eth_len]);
            }
        }

        icmp::ICMP_ECHO_REPLY => {
            // Check if this matches our pending ping (async state machine)
            if unsafe { PENDING_STATE } != PING_WAIT_ICMP { return; }
            let id  = if payload.len() >= 6 { u16::from_be_bytes([payload[4], payload[5]]) } else { 0 };
            let seq = if payload.len() >= 8 { u16::from_be_bytes([payload[6], payload[7]]) } else { 0 };
            let our_id  = unsafe { PENDING_ICMP_ID  };
            let our_seq = unsafe { PENDING_ICMP_SEQ };
            if id == our_id && seq == our_seq {
                let sent_ns = unsafe { PENDING_SENT_NS };
                let sender  = unsafe { PENDING_SENDER  };
                let rtt_ms  = (clock().saturating_sub(sent_ns)) / 1_000_000;
                unsafe { PENDING_STATE = PING_IDLE; }
                reply_data(sender, OP_NET_PING, rtt_ms);
            }
        }

        _ => {}
    }
}

fn handle_udp_packet(src_ip: [u8; 4], payload: &[u8]) {
    if let Some((src_port, dst_port, data)) = parse_udp(payload) {
        deliver(src_ip, src_port, dst_port, data);
    }
}

fn handle_tcp_incoming(src_ip: [u8; 4], payload: &[u8], vnet: &mut VirtioNet) {
    if let Some(event) = handle_tcp(src_ip, payload, OUR_IP) {
        match event {
            TcpEvent::Established => {
                // If the TCP-connect state machine is waiting for SYN-ACK,
                // let it handle the ACK and reply to the caller.
                if unsafe { TCP_SM_STATE } == TCP_SM_WAIT_SYNACK {
                    tcp_sm_on_established(vnet);
                    return;
                }
                // Fallback: no pending connect SM — send ACK anyway (shouldn't happen)
                let conn = get_conn();
                let dst_ip  = conn.remote_ip;
                let dst_mac = lookup_mac(dst_ip).unwrap_or([0xFF; 6]);
                send_tcp_packet(
                    dst_ip, dst_mac,
                    conn.local_port, conn.remote_port,
                    conn.seq, conn.ack,
                    TCP_ACK, 65535, &[],
                    vnet,
                );
            }
            TcpEvent::DataReceived => {
                // ACK the received data
                let conn = get_conn();
                let dst_ip  = conn.remote_ip;
                let dst_mac = lookup_mac(dst_ip).unwrap_or([0xFF; 6]);
                send_tcp_packet(
                    dst_ip, dst_mac,
                    conn.local_port, conn.remote_port,
                    conn.seq, conn.ack,
                    TCP_ACK, 65535, &[],
                    vnet,
                );
            }
            TcpEvent::Reset => {
                if unsafe { TCP_SM_STATE } == TCP_SM_WAIT_SYNACK {
                    tcp_sm_on_reset();
                } else {
                    tcp_close();
                }
            }
            TcpEvent::Closed => {
                tcp_close();
            }
        }
    }
}

// ── TCP packet transmission helper ───────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn send_tcp_packet(
    dst_ip:  [u8; 4],
    dst_mac: [u8; 6],
    src_p:   u16,
    dst_p:   u16,
    seq:     u32,
    ack:     u32,
    flags:   u8,
    window:  u16,
    payload: &[u8],
    vnet:    &mut VirtioNet,
) {
    let mut tcp_buf = [0u8; 80];
    let tcp_len = build_tcp(src_p, dst_p, seq, ack, flags, window, payload, &mut tcp_buf);
    if tcp_len == 0 { return; }
    fill_tcp_checksum(OUR_IP, dst_ip, &mut tcp_buf[..tcp_len]);

    let mut ip_buf = [0u8; 128];
    let ip_len = build_ipv4(IP_PROTO_TCP, OUR_IP, dst_ip, &tcp_buf[..tcp_len], &mut ip_buf);
    if ip_len == 0 { return; }

    let pktbuf = unsafe { &mut *addr_of_mut!(PKTBUF) };
    let eth_len = build_eth(dst_mac, vnet.mac, ETH_IPV4, &ip_buf[..ip_len], pktbuf);
    if eth_len > 0 {
        vnet.send_packet(&pktbuf[..eth_len]);
    }
}

// ── IPC send helper ───────────────────────────────────────────────────────────

fn send_reply(to_pid: u32, msg: &Msg) {
    syscall::send_msg(to_pid as u64, msg);
}

// ── Byte ↔ word packing helpers ───────────────────────────────────────────────

/// Pack up to 40 bytes into 5 × u64 words (little-endian, zero-padded).
fn pack_bytes_into_words(bytes: &[u8], words: &mut [u64]) {
    for w in words.iter_mut() {
        *w = 0;
    }
    for (i, &b) in bytes.iter().enumerate() {
        let word_idx = i / 8;
        let byte_idx = i % 8;
        if word_idx < words.len() {
            words[word_idx] |= (b as u64) << (byte_idx * 8);
        }
    }
}

/// Unpack up to 40 bytes from 5 × u64 words (little-endian).
fn unpack_words_to_bytes(words: &[u64], out: &mut [u8]) {
    for (i, slot) in out.iter_mut().enumerate() {
        let word_idx = i / 8;
        let byte_idx = i % 8;
        if word_idx < words.len() {
            *slot = (words[word_idx] >> (byte_idx * 8)) as u8;
        }
    }
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    print(b"[net] PANIC\n");
    exit(255);
}
