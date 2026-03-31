//! ARP (Address Resolution Protocol) — IPv4 over Ethernet.
//!
//! Packet layout (28 bytes after Ethernet header):
//!   bytes 0–1   : hw type (0x0001 = Ethernet)
//!   bytes 2–3   : proto type (0x0800 = IPv4)
//!   byte  4     : hw addr len (6)
//!   byte  5     : proto addr len (4)
//!   bytes 6–7   : operation (1=request, 2=reply)
//!   bytes 8–13  : sender MAC
//!   bytes 14–17 : sender IP
//!   bytes 18–23 : target MAC
//!   bytes 24–27 : target IP

use crate::eth::{build_eth, ETH_ARP, MAC_BROADCAST};

const ARP_REQUEST: u16 = 1;
const ARP_REPLY:   u16 = 2;
const ARP_LEN:     usize = 28;

// ── ARP table ─────────────────────────────────────────────────────────────────

const ARP_TABLE_SIZE: usize = 8;

struct ArpEntry {
    valid: bool,
    ip:    [u8; 4],
    mac:   [u8; 6],
}

static mut ARP_TABLE: [ArpEntry; ARP_TABLE_SIZE] = [
    ArpEntry { valid: false, ip: [0; 4], mac: [0; 6] },
    ArpEntry { valid: false, ip: [0; 4], mac: [0; 6] },
    ArpEntry { valid: false, ip: [0; 4], mac: [0; 6] },
    ArpEntry { valid: false, ip: [0; 4], mac: [0; 6] },
    ArpEntry { valid: false, ip: [0; 4], mac: [0; 6] },
    ArpEntry { valid: false, ip: [0; 4], mac: [0; 6] },
    ArpEntry { valid: false, ip: [0; 4], mac: [0; 6] },
    ArpEntry { valid: false, ip: [0; 4], mac: [0; 6] },
];

/// Look up the MAC address for a given IPv4 address.
pub fn lookup_mac(ip: [u8; 4]) -> Option<[u8; 6]> {
    unsafe {
        for entry in ARP_TABLE.iter() {
            if entry.valid && entry.ip == ip {
                return Some(entry.mac);
            }
        }
    }
    None
}

/// Store an ARP mapping. If the IP already exists, update it.
/// If the table is full, overwrite the first entry (simple LRU approximation).
pub fn store_arp(ip: [u8; 4], mac: [u8; 6]) {
    unsafe {
        // Update existing entry
        for entry in ARP_TABLE.iter_mut() {
            if entry.valid && entry.ip == ip {
                entry.mac = mac;
                return;
            }
        }
        // Find empty slot
        for entry in ARP_TABLE.iter_mut() {
            if !entry.valid {
                entry.valid = true;
                entry.ip    = ip;
                entry.mac   = mac;
                return;
            }
        }
        // Overwrite slot 0 (table is full)
        ARP_TABLE[0].valid = true;
        ARP_TABLE[0].ip    = ip;
        ARP_TABLE[0].mac   = mac;
    }
}

// ── Packet builders ───────────────────────────────────────────────────────────

/// Build a complete Ethernet + ARP request frame into `out`.
/// Returns the number of bytes written.
pub fn build_arp_request(
    our_mac:   [u8; 6],
    our_ip:    [u8; 4],
    target_ip: [u8; 4],
    out:       &mut [u8],
) -> usize {
    let mut arp = [0u8; ARP_LEN];
    arp[0]  = 0x00; arp[1]  = 0x01; // hw type: Ethernet
    arp[2]  = 0x08; arp[3]  = 0x00; // proto: IPv4
    arp[4]  = 6;                      // hw addr len
    arp[5]  = 4;                      // proto addr len
    arp[6]  = 0x00; arp[7]  = ARP_REQUEST as u8; // operation
    arp[8..14].copy_from_slice(&our_mac);
    arp[14..18].copy_from_slice(&our_ip);
    arp[18..24].copy_from_slice(&[0u8; 6]); // target MAC unknown
    arp[24..28].copy_from_slice(&target_ip);

    build_eth(MAC_BROADCAST, our_mac, ETH_ARP, &arp, out)
}

/// Build a complete Ethernet + ARP reply frame into `out`.
/// Returns the number of bytes written.
pub fn build_arp_reply(
    our_mac: [u8; 6],
    our_ip:  [u8; 4],
    req_mac: [u8; 6],
    req_ip:  [u8; 4],
    out:     &mut [u8],
) -> usize {
    let mut arp = [0u8; ARP_LEN];
    arp[0]  = 0x00; arp[1]  = 0x01;
    arp[2]  = 0x08; arp[3]  = 0x00;
    arp[4]  = 6;
    arp[5]  = 4;
    arp[6]  = 0x00; arp[7]  = ARP_REPLY as u8;
    arp[8..14].copy_from_slice(&our_mac);
    arp[14..18].copy_from_slice(&our_ip);
    arp[18..24].copy_from_slice(&req_mac);
    arp[24..28].copy_from_slice(&req_ip);

    build_eth(req_mac, our_mac, ETH_ARP, &arp, out)
}

// ── Packet parser ─────────────────────────────────────────────────────────────

/// Parse an ARP payload (28 bytes after the Ethernet header).
/// Stores sender IP/MAC in the ARP table.
/// If this is a reply directed at `our_ip`, returns `Some((sender_ip, sender_mac))`.
pub fn handle_arp(
    payload: &[u8],
    our_mac: [u8; 6],
    our_ip:  [u8; 4],
) -> Option<([u8; 4], [u8; 6])> {
    if payload.len() < ARP_LEN { return None; }

    let hw_type   = u16::from_be_bytes([payload[0], payload[1]]);
    let proto     = u16::from_be_bytes([payload[2], payload[3]]);
    let hw_len    = payload[4];
    let proto_len = payload[5];
    let op        = u16::from_be_bytes([payload[6], payload[7]]);

    // Only handle Ethernet + IPv4 ARP
    if hw_type != 0x0001 || proto != 0x0800 || hw_len != 6 || proto_len != 4 {
        return None;
    }

    let mut sender_mac = [0u8; 6];
    let mut sender_ip  = [0u8; 4];
    let mut target_mac = [0u8; 6];
    let mut target_ip  = [0u8; 4];

    sender_mac.copy_from_slice(&payload[8..14]);
    sender_ip.copy_from_slice(&payload[14..18]);
    target_mac.copy_from_slice(&payload[18..24]);
    target_ip.copy_from_slice(&payload[24..28]);

    // Always store the sender mapping
    store_arp(sender_ip, sender_mac);

    // If it's an ARP reply whose target MAC is ours, return the resolved mapping
    if op == ARP_REPLY && target_ip == our_ip && target_mac == our_mac {
        return Some((sender_ip, sender_mac));
    }

    // If it's an ARP request for our IP, return sender info so caller can reply
    if op == ARP_REQUEST && target_ip == our_ip {
        return Some((sender_ip, sender_mac));
    }

    None
}
