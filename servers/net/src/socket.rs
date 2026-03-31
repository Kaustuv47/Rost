//! IPC opcode constants and reply helpers for the rost-net server.

use crate::syscall::{send_msg, Msg};

// ── IPC Opcode constants ──────────────────────────────────────────────────────

pub const OP_NET_PING:        u64 = 0x0100;
pub const OP_NET_UDP_BIND:    u64 = 0x0101;
pub const OP_NET_UDP_SEND:    u64 = 0x0102;
pub const OP_NET_UDP_RECV:    u64 = 0x0103;
pub const OP_NET_TCP_CONNECT: u64 = 0x0104;
pub const OP_NET_TCP_SEND:    u64 = 0x0105;
pub const OP_NET_TCP_RECV:    u64 = 0x0106;
pub const OP_NET_TCP_CLOSE:   u64 = 0x0107;
pub const OP_NET_GET_IP:      u64 = 0x0108;

/// Reply with just the opcode (success, no payload).
pub fn reply_ok(to_pid: u32, op: u64) {
    let mut m = Msg::zeroed();
    m.data[0] = op;
    m.data[1] = 0;
    send_msg(to_pid as u64, &m);
}

/// Reply with opcode + one data value.
pub fn reply_data(to_pid: u32, op: u64, val: u64) {
    let mut m = Msg::zeroed();
    m.data[0] = op;
    m.data[1] = val;
    send_msg(to_pid as u64, &m);
}

/// Reply with opcode + 0xFFFF error indicator.
pub fn reply_err(to_pid: u32, op: u64) {
    let mut m = Msg::zeroed();
    m.data[0] = op;
    m.data[1] = 0xFFFF;
    send_msg(to_pid as u64, &m);
}

/// Reply with opcode + two data values.
pub fn reply_data2(to_pid: u32, op: u64, val1: u64, val2: u64) {
    let mut m = Msg::zeroed();
    m.data[0] = op;
    m.data[1] = val1;
    m.data[2] = val2;
    send_msg(to_pid as u64, &m);
}

/// Reply with opcode + full data array (for UDP/TCP payload replies).
pub fn reply_full(to_pid: u32, reply: &Msg) {
    send_msg(to_pid as u64, reply);
}
