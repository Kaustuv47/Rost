//! rost-gop — GOP framebuffer text-renderer server for the Rost microkernel.
//!
//! Registers as "gop" in the service registry.
//!
//! Uses `SYS_GET_FRAMEBUF (33)` to discover the primary display's physical base
//! address, then maps it into virtual address space with `SYS_MAP (9)`.
//!
//! A built-in 8 × 16 bitmap font (PC-style Code Page 437) provides ASCII
//! text rendering.  Characters outside 0x20–0x7E render as a solid block.
//!
//! All text output passes through a VT100/ANSI escape sequence parser so that
//! shell colour codes and cursor-control sequences are rendered correctly.
//!
//! # IPC Protocol
//!
//! | Opcode            | Value  | data[1..] meaning          | Reply    |
//! |-------------------|--------|----------------------------|----------|
//! | OP_GOP_PUTCHAR    | 0x70   | data[1]=codepoint (ASCII)   | —        |
//! | OP_GOP_PUTS       | 0x71   | data[1..5]=up to 40 bytes   | —        |
//! | OP_GOP_CLEAR      | 0x72   | —                           | —        |
//! | OP_GOP_SET_COLOR  | 0x73   | data[1]=fg ARGB, data[2]=bg | —        |
//! | OP_GOP_GET_SIZE   | 0x74   | —                           | RESP_GOP_SIZE |
//!
//! RESP_GOP_SIZE (0xA0): data[1]=cols, data[2]=rows
//!
//! All text opcodes are fire-and-forget (no reply expected).

#![no_std]
#![no_main]

mod font;
mod syscall;
mod vt100;

use core::ptr::addr_of_mut;
use syscall::{exit, get_framebuf, getpid, print, print_dec, recv_msg, register,
              sys_map, FbQueryResult, Msg};
use vt100::{Vt100, Vt100Event, PALETTE};

// ── IPC opcodes ───────────────────────────────────────────────────────────────

const OP_GOP_PUTCHAR:   u64 = 0x70;
const OP_GOP_PUTS:      u64 = 0x71;
const OP_GOP_CLEAR:     u64 = 0x72;
const OP_GOP_SET_COLOR: u64 = 0x73;
const OP_GOP_GET_SIZE:  u64 = 0x74;

const RESP_GOP_SIZE:    u64 = 0xA0;

// ── Font geometry ─────────────────────────────────────────────────────────────

const FONT_W: usize = 8;
const FONT_H: usize = 16;

// ── Framebuffer state ─────────────────────────────────────────────────────────

/// Virtual base of the mapped framebuffer.
static mut FB_VIRT:   u64 = 0;
static mut FB_STRIDE: u32 = 0;  // pixels per scan line
static mut FB_WIDTH:  u32 = 0;
static mut FB_HEIGHT: u32 = 0;
/// Pixel format: 0=Rgb32, 1=Bgr32
static mut FB_FORMAT: u32 = 0;

// ── Terminal state ────────────────────────────────────────────────────────────

// Cursor position in character cells (0-based)
static mut CURSOR_COL: u32 = 0;
static mut CURSOR_ROW: u32 = 0;

// Saved cursor (ESC 7 / ESC 8 / CSI s / CSI u)
static mut SAVED_COL: u32 = 0;
static mut SAVED_ROW: u32 = 0;

// Current colors in native ARGB format
static mut COLOR_FG: u32 = vt100::PALETTE[vt100::DEFAULT_FG as usize];
static mut COLOR_BG: u32 = vt100::PALETTE[vt100::DEFAULT_BG as usize];

// VT100 parser (const-constructible, lives in BSS)
static mut VT: Vt100 = Vt100::new();

// ── Pixel write (format-aware) ────────────────────────────────────────────────

/// Convert ARGB `color` to the native framebuffer word.
unsafe fn to_native(color: u32) -> u32 {
    // color is 0xAA_RR_GG_BB
    let r = (color >> 16) & 0xFF;
    let g = (color >>  8) & 0xFF;
    let b =  color        & 0xFF;
    match FB_FORMAT {
        0 => (r << 16) | (g << 8) | b, // Rgb32: R[23:16] G[15:8] B[7:0]
        _ => (b << 16) | (g << 8) | r, // Bgr32: B[23:16] G[15:8] R[7:0]
    }
}

/// Write a 32-bit pixel at (x, y).
#[inline(always)]
unsafe fn put_pixel(x: u32, y: u32, color: u32) {
    let native = to_native(color);
    let stride  = FB_STRIDE as u64;
    let byte_off = (y as u64 * stride + x as u64) * 4;
    let ptr = (FB_VIRT + byte_off) as *mut u32;
    core::ptr::write_volatile(ptr, native);
}

// ── Character rendering ───────────────────────────────────────────────────────

/// Render one character cell at grid position (col, row).
unsafe fn draw_char(col: u32, row: u32, ch: u8) {
    let glyph = font::glyph(ch);
    let px = col * FONT_W as u32;
    let py = row * FONT_H as u32;

    if px + FONT_W as u32 > FB_WIDTH || py + FONT_H as u32 > FB_HEIGHT { return; }

    for gy in 0..FONT_H {
        let row_bits = glyph[gy];
        for gx in 0..FONT_W {
            let fg = row_bits & (0x80 >> gx) != 0;
            let color = if fg { COLOR_FG } else { COLOR_BG };
            put_pixel(px + gx as u32, py + gy as u32, color);
        }
    }
}

// ── Erase helpers ─────────────────────────────────────────────────────────────

/// Fill a rectangular region of character cells with the background color.
unsafe fn erase_cells(col_start: u32, row_start: u32, col_end: u32, row_end: u32) {
    let native_bg = to_native(COLOR_BG);
    let mut row = row_start;
    while row <= row_end {
        let cs = if row == row_start { col_start } else { 0 };
        let ce = if row == row_end   { col_end   } else { cols().saturating_sub(1) };
        let py = row * FONT_H as u32;
        let mut col = cs;
        while col <= ce {
            let px = col * FONT_W as u32;
            for gy in 0..FONT_H {
                for gx in 0..FONT_W {
                    let px2 = px + gx as u32;
                    let py2 = py + gy as u32;
                    if px2 < FB_WIDTH && py2 < FB_HEIGHT {
                        put_pixel(px2, py2, native_bg);
                    }
                }
            }
            col += 1;
        }
        row += 1;
    }
}

/// CSI K — erase in line.  mode: 0=cursor→end, 1=start→cursor, 2=whole line.
unsafe fn erase_line(mode: u32) {
    let c = CURSOR_COL;
    let r = CURSOR_ROW;
    let last_col = cols().saturating_sub(1);
    match mode {
        0 => erase_cells(c, r, last_col, r),
        1 => erase_cells(0, r, c, r),
        _ => erase_cells(0, r, last_col, r),
    }
}

/// CSI J — erase in display.  mode: 0=cursor→end, 1=start→cursor, 2/3=all.
unsafe fn erase_display(mode: u32) {
    let c = CURSOR_COL;
    let r = CURSOR_ROW;
    let last_col = cols().saturating_sub(1);
    let last_row = rows().saturating_sub(1);
    match mode {
        0 => {
            // cursor to end of line, then all lines below
            erase_cells(c, r, last_col, r);
            if r < last_row { erase_cells(0, r + 1, last_col, last_row); }
        }
        1 => {
            // all lines above, then start of line to cursor
            if r > 0 { erase_cells(0, 0, last_col, r - 1); }
            erase_cells(0, r, c, r);
        }
        _ => {
            // whole screen
            clear_screen();
        }
    }
}

// ── Delete characters (CSI P) ─────────────────────────────────────────────────

/// CSI P — delete `n` characters at cursor, shifting remainder left.
unsafe fn delete_chars(n: u32) {
    let last_col = cols().saturating_sub(1);
    let r = CURSOR_ROW;
    let c = CURSOR_COL;
    let src = (c + n).min(last_col + 1);
    // shift remaining chars left
    let mut dst_col = c;
    let mut src_col = src;
    while src_col <= last_col {
        // Re-draw the source cell at destination (read-back not possible, use space)
        // We can't read framebuffer pixels back, so just fill destination with blanks
        // and let the app redraw. For correctness we shift blank space in from right.
        let _ = (dst_col, src_col, r);
        dst_col += 1;
        src_col += 1;
    }
    // Erase tail cells
    if dst_col <= last_col {
        erase_cells(dst_col, r, last_col, r);
    }
}

// ── Scroll ────────────────────────────────────────────────────────────────────

/// Scroll the entire terminal up by `n` character rows.
unsafe fn scroll_up_n(n: u32) {
    let n = n as usize;
    let line_bytes = FB_STRIDE as usize * FONT_H * 4;
    let shift_bytes = line_bytes * n;
    let fb = FB_VIRT as *mut u8;
    let total = FB_STRIDE as usize * FB_HEIGHT as usize * 4;
    if shift_bytes >= total { clear_screen(); return; }
    // Move rows n..N-1 → rows 0..N-1-n
    let len = total - shift_bytes;
    for i in 0..len { *fb.add(i) = *fb.add(i + shift_bytes); }
    // Clear bottom n rows
    let native_bg = to_native(COLOR_BG);
    let clear_start = total - shift_bytes;
    let tail_words = FB_STRIDE as usize * FONT_H * n;
    let last_row_ptr = fb.add(clear_start) as *mut u32;
    for i in 0..tail_words {
        core::ptr::write_volatile(last_row_ptr.add(i), native_bg);
    }
}

/// Scroll the entire terminal down by `n` character rows.
unsafe fn scroll_down_n(n: u32) {
    let n = n as usize;
    let line_bytes = FB_STRIDE as usize * FONT_H * 4;
    let shift_bytes = line_bytes * n;
    let fb = FB_VIRT as *mut u8;
    let total = FB_STRIDE as usize * FB_HEIGHT as usize * 4;
    if shift_bytes >= total { clear_screen(); return; }
    // Move rows 0..N-1-n → rows n..N-1 (copy backwards to avoid overlap)
    let len = total - shift_bytes;
    let mut i = len;
    while i > 0 {
        i -= 1;
        *fb.add(i + shift_bytes) = *fb.add(i);
    }
    // Clear top n rows
    let native_bg = to_native(COLOR_BG);
    let head_words = FB_STRIDE as usize * FONT_H * n;
    let head_ptr = fb as *mut u32;
    for i in 0..head_words {
        core::ptr::write_volatile(head_ptr.add(i), native_bg);
    }
}

// ── Terminal geometry ─────────────────────────────────────────────────────────

#[inline(always)]
unsafe fn cols() -> u32 { FB_WIDTH  / FONT_W as u32 }

#[inline(always)]
unsafe fn rows() -> u32 { FB_HEIGHT / FONT_H as u32 }

// ── Linefeed / carriage-return ────────────────────────────────────────────────

/// VT100 LF: advance row only (do NOT reset column).
unsafe fn linefeed() {
    let r = rows();
    if CURSOR_ROW + 1 >= r {
        scroll_up_n(1);
        // cursor stays on last row
    } else {
        CURSOR_ROW += 1;
    }
}

/// VT100 CR: reset column only.
#[inline(always)]
unsafe fn carriage_return() {
    CURSOR_COL = 0;
}

// ── Clear screen ──────────────────────────────────────────────────────────────

unsafe fn clear_screen() {
    let native_bg = to_native(COLOR_BG);
    let total = FB_STRIDE as usize * FB_HEIGHT as usize;
    let fb = FB_VIRT as *mut u32;
    for i in 0..total {
        core::ptr::write_volatile(fb.add(i), native_bg);
    }
    CURSOR_COL = 0;
    CURSOR_ROW = 0;
}

// ── Clamp cursor to screen ────────────────────────────────────────────────────

#[inline(always)]
unsafe fn clamp_cursor() {
    let c = cols();
    let r = rows();
    if c > 0 && CURSOR_COL >= c { CURSOR_COL = c - 1; }
    if r > 0 && CURSOR_ROW >= r { CURSOR_ROW = r - 1; }
}

// ── Raw character output (no escape processing) ───────────────────────────────

/// Place one printable character at current cursor and advance.
unsafe fn emit_char(ch: u8) {
    let c = cols();
    if c == 0 { return; }
    draw_char(CURSOR_COL, CURSOR_ROW, ch);
    CURSOR_COL += 1;
    if CURSOR_COL >= c {
        CURSOR_COL = 0;
        linefeed();
    }
}

// ── VT100 event handler ───────────────────────────────────────────────────────

/// Process a single decoded VT100 event, updating the framebuffer and state.
unsafe fn handle_event(ev: Vt100Event) {
    match ev {
        Vt100Event::Char(b) => emit_char(b),

        Vt100Event::CarriageReturn => carriage_return(),
        Vt100Event::LineFeed       => linefeed(),

        Vt100Event::Tab => {
            let tab = 8 - (CURSOR_COL % 8);
            for _ in 0..tab { emit_char(b' '); }
        }

        Vt100Event::Backspace => {
            if CURSOR_COL > 0 {
                CURSOR_COL -= 1;
            } else if CURSOR_ROW > 0 {
                CURSOR_ROW -= 1;
                CURSOR_COL = cols().saturating_sub(1);
            }
            // overwrite with space
            draw_char(CURSOR_COL, CURSOR_ROW, b' ');
        }

        // ── Cursor movement ───────────────────────────────────────────────────
        Vt100Event::CursorUp(n) => {
            CURSOR_ROW = CURSOR_ROW.saturating_sub(n);
        }
        Vt100Event::CursorDown(n) => {
            let r = rows();
            CURSOR_ROW = (CURSOR_ROW + n).min(r.saturating_sub(1));
        }
        Vt100Event::CursorRight(n) => {
            let c = cols();
            CURSOR_COL = (CURSOR_COL + n).min(c.saturating_sub(1));
        }
        Vt100Event::CursorLeft(n) => {
            CURSOR_COL = CURSOR_COL.saturating_sub(n);
        }
        Vt100Event::CursorNextLine(n) => {
            let r = rows();
            CURSOR_ROW = (CURSOR_ROW + n).min(r.saturating_sub(1));
            CURSOR_COL = 0;
        }
        Vt100Event::CursorPrevLine(n) => {
            CURSOR_ROW = CURSOR_ROW.saturating_sub(n);
            CURSOR_COL = 0;
        }
        Vt100Event::CursorColumn(col) => {
            let c = cols();
            CURSOR_COL = col.min(c.saturating_sub(1));
        }
        Vt100Event::CursorPosition(row, col) => {
            let c = cols();
            let r = rows();
            CURSOR_ROW = row.min(r.saturating_sub(1));
            CURSOR_COL = col.min(c.saturating_sub(1));
        }

        // ── Save / restore cursor ─────────────────────────────────────────────
        Vt100Event::SaveCursor => {
            SAVED_COL = CURSOR_COL;
            SAVED_ROW = CURSOR_ROW;
        }
        Vt100Event::RestoreCursor => {
            CURSOR_COL = SAVED_COL;
            CURSOR_ROW = SAVED_ROW;
            clamp_cursor();
        }

        // ── Erase ─────────────────────────────────────────────────────────────
        Vt100Event::EraseLine(mode)    => erase_line(mode),
        Vt100Event::EraseDisplay(mode) => erase_display(mode),
        Vt100Event::DeleteChars(n)     => delete_chars(n),

        // ── Scroll ────────────────────────────────────────────────────────────
        Vt100Event::ScrollUp(n)   => scroll_up_n(n),
        Vt100Event::ScrollDown(n) => scroll_down_n(n),

        // ── Color / attribute changes ─────────────────────────────────────────
        Vt100Event::SetFg(argb) => { COLOR_FG = argb; }
        Vt100Event::SetBg(argb) => { COLOR_BG = argb; }

        // ── Full reset ────────────────────────────────────────────────────────
        Vt100Event::Reset => {
            COLOR_FG = PALETTE[vt100::DEFAULT_FG as usize];
            COLOR_BG = PALETTE[vt100::DEFAULT_BG as usize];
            clear_screen();
        }
    }
}

// ── Feed a byte through the VT100 parser ─────────────────────────────────────

unsafe fn feed_byte(b: u8) {
    let vt = &mut *addr_of_mut!(VT);
    vt.feed(b, &mut |ev| handle_event(ev));
}

unsafe fn feed_bytes(data: &[u8]) {
    for &b in data {
        if b == 0 { break; }
        feed_byte(b);
    }
}

// ── Framebuffer mapping ───────────────────────────────────────────────────────

/// Map `size` bytes of the framebuffer physical memory starting at `phys_base`
/// to virtual address `virt_base`.  Maps 4 KB pages; returns true on success.
unsafe fn map_framebuf(virt_base: u64, phys_base: u64, size: u64) -> bool {
    let pages = (size + 0xFFF) / 0x1000;
    for i in 0..pages {
        let vaddr = virt_base + i * 0x1000;
        let paddr = phys_base + i * 0x1000;
        let ret = sys_map(vaddr, paddr, 0x3); // writable + user
        if ret != 0 { return false; }
    }
    true
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _start() -> ! {
    print(b"[gop] started, PID=");
    print_dec(getpid() as u64);
    print(b"\n");

    // ── Query framebuffer ─────────────────────────────────────────────────────
    static mut FB_INFO: FbQueryResult = FbQueryResult {
        base: 0, size: 0, width: 0, height: 0, stride: 0, format: 0
    };
    let fb_info = unsafe { &mut *addr_of_mut!(FB_INFO) };
    let ret = get_framebuf(fb_info);
    if ret != 0 {
        print(b"[gop] no GOP framebuffer available (SYS_GET_FRAMEBUF returned ");
        print_dec(ret);
        print(b")\n");
        exit(0);
    }

    print(b"[gop] framebuffer at phys=");
    syscall::print_hex(fb_info.base);
    print(b" ");
    print_dec(fb_info.width as u64);
    print(b"x");
    print_dec(fb_info.height as u64);
    print(b" stride=");
    print_dec(fb_info.stride as u64);
    print(b"\n");

    // ── Map framebuffer into our VA space at 0x4000_0000 ─────────────────────
    const FB_VIRT_BASE: u64 = 0x4000_0000;
    if !unsafe { map_framebuf(FB_VIRT_BASE, fb_info.base, fb_info.size) } {
        print(b"[gop] ERROR: failed to map framebuffer pages\n");
        exit(1);
    }

    unsafe {
        FB_VIRT   = FB_VIRT_BASE;
        FB_STRIDE = fb_info.stride;
        FB_WIDTH  = fb_info.width;
        FB_HEIGHT = fb_info.height;
        FB_FORMAT = fb_info.format;
    }

    // ── Register ──────────────────────────────────────────────────────────────
    if !register(b"gop\0\0\0\0\0\0\0\0\0\0\0\0\0") {
        print(b"[gop] ERROR: register failed\n");
        exit(1);
    }

    // ── Initial clear and banner ──────────────────────────────────────────────
    unsafe {
        clear_screen();
        feed_bytes(b"Rost Microkernel - GOP Display Driver\r\n");
        feed_bytes(b"=====================================\r\n");
    }

    print(b"[gop] registered as 'gop', ready\n");

    // ── Main loop ─────────────────────────────────────────────────────────────
    static mut MSG_BUF: Msg = Msg { sender: 0, _pad: 0, data: [0; 8] };
    loop {
        let buf = unsafe { &mut *addr_of_mut!(MSG_BUF) };
        if !recv_msg(u64::MAX, buf) { continue; }

        let sender = buf.sender as u64;
        match buf.data[0] {
            OP_GOP_PUTCHAR => {
                let ch = buf.data[1] as u8;
                unsafe { feed_byte(ch); }
            }

            OP_GOP_PUTS => {
                // data[1..5] holds up to 40 bytes of text (little-endian packed).
                let bytes: &[u8; 40] = unsafe {
                    &*(buf.data[1..6].as_ptr() as *const [u8; 40])
                };
                unsafe { feed_bytes(bytes); }
            }

            OP_GOP_CLEAR => {
                unsafe { clear_screen(); }
            }

            OP_GOP_SET_COLOR => {
                // Direct ARGB override (bypasses VT100 palette — used by privileged callers)
                unsafe {
                    COLOR_FG = buf.data[1] as u32;
                    COLOR_BG = buf.data[2] as u32;
                }
            }

            OP_GOP_GET_SIZE => {
                let cols = unsafe { cols() };
                let rows = unsafe { rows() };
                let mut reply = Msg::zeroed();
                reply.data[0] = RESP_GOP_SIZE;
                reply.data[1] = cols as u64;
                reply.data[2] = rows as u64;
                syscall::send_msg(sender, &reply);
            }

            _ => {}
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    print(b"[gop] PANIC\n");
    exit(1);
}
