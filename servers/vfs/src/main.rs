//! Rost VFS Server — virtual filesystem for the Rost microkernel.
//!
//! Runs as a ring-3 ELF binary (PID 3 by convention).
//! All storage access goes through the IPC protocol defined in proto.rs.
//! Currently backed by a static RAM disk (fs.rs).  A real block driver
//! (servers/block-drv) would replace the RAM disk once the ELF loader exists.
//!
//! # IPC protocol summary
//!
//! Client sends OP_LIST or OP_READ; VFS replies with RESP_ENTRY/RESP_DATA
//! messages followed by RESP_DONE, or RESP_ERROR on failure.
//! See proto.rs for the full wire format.
//!
//! # Build
//! ```sh
//! cd servers
//! cargo build --target x86_64-unknown-none
//! # produces: target/x86_64-unknown-none/debug/rost-vfs
//! ```
#![no_std]
#![no_main]

mod fs;
mod proto;
mod syscall;

use proto::*;
use syscall::{Msg, recv_msg, send_msg};

/// Magic constant recognised by init as EVENT_VFS_READY.
const EVENT_VFS_READY: u64 = 0x5246_5342_5f52_4459; // "RFSB_RDY"

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Signal init (PID 1) that we are alive and ready for IPC.
    syscall::notify(1, EVENT_VFS_READY);

    dispatch_loop()
}

fn dispatch_loop() -> ! {
    loop {
        let mut req = Msg::zeroed();
        // Block indefinitely until a client sends a request.
        if !recv_msg(u64::MAX, &mut req) {
            continue;
        }

        let requester = req.sender;

        match req.data[0] {
            OP_LIST => handle_list(requester),
            OP_READ => handle_read(
                requester,
                req.data[1],        // byte_offset
                req.data[2],        // name word 0
                req.data[3],        // name word 1
            ),
            _ => {
                // Unknown opcode — send RESP_ERROR back.
                let mut resp = Msg::zeroed();
                resp.data[0] = RESP_ERROR;
                resp.data[1] = 2; // ENOSYS
                send_msg(requester as u64, &resp);
            }
        }
    }
}

// ── OP_LIST handler ───────────────────────────────────────────────────────────

fn handle_list(requester: u32) {
    for entry in fs::FILES {
        let mut resp = Msg::zeroed();
        resp.data[0] = RESP_ENTRY;
        resp.data[1] = entry.flags as u64;
        resp.data[2] = entry.data.len() as u64;
        // Pack filename (up to 40 bytes) into data[3..7].
        pack_name_into(&mut resp.data[3..8], entry.name);
        send_msg(requester as u64, &resp);
    }
    // Send end-of-listing sentinel.
    let mut done = Msg::zeroed();
    done.data[0] = RESP_DONE;
    send_msg(requester as u64, &done);
}

// ── OP_READ handler ───────────────────────────────────────────────────────────

fn handle_read(requester: u32, byte_offset: u64, name_w0: u64, name_w1: u64) {
    // Reconstruct filename from the two packed words (up to 16 bytes).
    let name_buf = unpack_name(name_w0, name_w1);

    let entry = match fs::find(&name_buf) {
        Some(e) => e,
        None => {
            let mut resp = Msg::zeroed();
            resp.data[0] = RESP_ERROR;
            resp.data[1] = 1; // ENOENT
            send_msg(requester as u64, &resp);
            return;
        }
    };

    let total = entry.data.len();
    let offset = byte_offset as usize;

    if offset >= total {
        // Past end of file — signal completion.
        let mut resp = Msg::zeroed();
        resp.data[0] = RESP_DONE;
        send_msg(requester as u64, &resp);
        return;
    }

    let end   = core::cmp::min(offset + CHUNK_SIZE, total);
    let chunk = &entry.data[offset..end];

    let mut resp = Msg::zeroed();
    resp.data[0] = RESP_DATA;
    resp.data[1] = total as u64;
    resp.data[2] = chunk.len() as u64;
    // Pack chunk bytes into data[3..7] (up to 40 bytes).
    pack_bytes_into(&mut resp.data[3..8], chunk);
    send_msg(requester as u64, &resp);
}

// ── Packing helpers ───────────────────────────────────────────────────────────

/// Pack `src` bytes (up to `words.len() * 8`) little-endian into `words`.
fn pack_bytes_into(words: &mut [u64], src: &[u8]) {
    for (i, &b) in src.iter().enumerate() {
        let wi = i / 8;
        let bi = i % 8;
        if wi < words.len() {
            words[wi] |= (b as u64) << (bi * 8);
        }
    }
}

/// Pack a filename (null-terminated, up to `words.len() * 8` bytes) into words.
fn pack_name_into(words: &mut [u64], name: &[u8]) {
    pack_bytes_into(words, name);
    // The name is naturally null-terminated because Msg::zeroed() starts at 0.
}

/// Reconstruct a 16-byte name buffer from two u64 words (little-endian bytes).
fn unpack_name(w0: u64, w1: u64) -> [u8; 16] {
    let mut buf = [0u8; 16];
    for i in 0..8 { buf[i]   = (w0 >> (i * 8)) as u8; }
    for i in 0..8 { buf[8+i] = (w1 >> (i * 8)) as u8; }
    buf
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    syscall::notify(1, 0x5246_5342_5f45_5252); // "RFSB_ERR"
    syscall::exit(1);
}
