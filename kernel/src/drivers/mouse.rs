//! PS/2 mouse driver (IRQ12) + software cursor.
//!
//! Protocol: standard 3-byte packets. Cursor is drawn with a small
//! under-buffer so we can erase/redraw without full-screen compose.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use crate::drivers::framebuffer::FRAMEBUFFER;
use crate::drivers::pic::PICS;
use crate::io::{inb, outb, io_wait};
use crate::{debugln, println};

/// IRQ12 remapped: 32 + 12 = 44
pub const MOUSE_INT: u8 = 44;

const PS2_DATA: u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const PS2_CMD: u16 = 0x64;

const STATUS_OUTPUT: u8 = 1 << 0;
const STATUS_INPUT: u8 = 1 << 1;
const STATUS_AUX: u8 = 1 << 5;

static READY: AtomicBool = AtomicBool::new(false);
static CYCLE: AtomicU8 = AtomicU8::new(0);
static PACKET: [AtomicU8; 3] = [AtomicU8::new(0), AtomicU8::new(0), AtomicU8::new(0)];

/// Absolute cursor position (screen pixels).
static POS_X: AtomicU32 = AtomicU32::new(400);
static POS_Y: AtomicU32 = AtomicU32::new(300);
/// Bit0=left, bit1=right, bit2=middle
static BUTTONS: AtomicU8 = AtomicU8::new(0);
static PREV_BUTTONS: AtomicU8 = AtomicU8::new(0);

/// Software cursor under-buffer (12×20, 32bpp).
const CUR_W: usize = 12;
const CUR_H: usize = 20;
static mut UNDER: [u32; CUR_W * CUR_H] = [0; CUR_W * CUR_H];
static mut CUR_DRAWN: bool = false;
static mut CUR_OX: i32 = 0;
static mut CUR_OY: i32 = 0;

/// Simple arrow bitmap (1 = foreground).
const CURSOR_MASK: [[u8; CUR_W]; CUR_H] = [
    [1,0,0,0,0,0,0,0,0,0,0,0],
    [1,1,0,0,0,0,0,0,0,0,0,0],
    [1,1,1,0,0,0,0,0,0,0,0,0],
    [1,1,1,1,0,0,0,0,0,0,0,0],
    [1,1,1,1,1,0,0,0,0,0,0,0],
    [1,1,1,1,1,1,0,0,0,0,0,0],
    [1,1,1,1,1,1,1,0,0,0,0,0],
    [1,1,1,1,1,1,1,1,0,0,0,0],
    [1,1,1,1,1,1,1,1,1,0,0,0],
    [1,1,1,1,1,1,1,0,0,0,0,0],
    [1,1,1,1,1,1,1,0,0,0,0,0],
    [1,1,0,0,1,1,1,1,0,0,0,0],
    [1,0,0,0,0,1,1,1,0,0,0,0],
    [0,0,0,0,0,1,1,1,1,0,0,0],
    [0,0,0,0,0,0,1,1,0,0,0,0],
    [0,0,0,0,0,0,0,0,0,0,0,0],
    [0,0,0,0,0,0,0,0,0,0,0,0],
    [0,0,0,0,0,0,0,0,0,0,0,0],
    [0,0,0,0,0,0,0,0,0,0,0,0],
    [0,0,0,0,0,0,0,0,0,0,0,0],
];

pub fn is_ready() -> bool {
    READY.load(Ordering::Relaxed)
}

pub fn position() -> (i32, i32) {
    (
        POS_X.load(Ordering::Relaxed) as i32,
        POS_Y.load(Ordering::Relaxed) as i32,
    )
}

pub fn buttons() -> u8 {
    BUTTONS.load(Ordering::Relaxed)
}

/// Snapshot for userspace / WM.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MouseState {
    pub x: i32,
    pub y: i32,
    pub buttons: u8,
    pub _pad: [u8; 3],
}

pub fn state() -> MouseState {
    MouseState {
        x: POS_X.load(Ordering::Relaxed) as i32,
        y: POS_Y.load(Ordering::Relaxed) as i32,
        buttons: BUTTONS.load(Ordering::Relaxed),
        _pad: [0; 3],
    }
}

fn wait_input_clear() {
    for _ in 0..100_000 {
        if inb(PS2_STATUS) & STATUS_INPUT == 0 {
            return;
        }
        io_wait();
    }
}

fn wait_output_full() -> bool {
    for _ in 0..100_000 {
        if inb(PS2_STATUS) & STATUS_OUTPUT != 0 {
            return true;
        }
        io_wait();
    }
    false
}

fn mouse_write(val: u8) {
    wait_input_clear();
    outb(PS2_CMD, 0xD4); // next byte → mouse
    wait_input_clear();
    outb(PS2_DATA, val);
}

fn mouse_read() -> u8 {
    if wait_output_full() {
        inb(PS2_DATA)
    } else {
        0
    }
}

/// Initialize 8042 aux device + enable streaming. Call with IF=0 after PIC init.
pub fn init() {
    debugln!("[mouse] init");

    // Enable auxiliary device
    wait_input_clear();
    outb(PS2_CMD, 0xA8);
    io_wait();

    // Read controller command byte
    wait_input_clear();
    outb(PS2_CMD, 0x20);
    let mut cmd = if wait_output_full() { inb(PS2_DATA) } else { 0x47 };
    cmd |= 0x02;  // enable IRQ12
    cmd &= !0x20; // enable mouse clock (clear disable bit)

    wait_input_clear();
    outb(PS2_CMD, 0x60);
    wait_input_clear();
    outb(PS2_DATA, cmd);

    // Reset mouse
    mouse_write(0xFF);
    let ack = mouse_read();
    if ack != 0xFA {
        debugln!("[mouse] reset no ACK ({:#x}), trying anyway", ack);
    }
    let _aa = mouse_read(); // 0xAA self-test
    let _id = mouse_read(); // device id

    // Defaults
    mouse_write(0xF6);
    let _ = mouse_read();

    // Enable data reporting
    mouse_write(0xF4);
    let ack2 = mouse_read();
    if ack2 != 0xFA {
        debugln!("[mouse] enable stream no ACK ({:#x})", ack2);
    }

    // Center cursor on screen if FB known
    if let Some(fb) = FRAMEBUFFER.lock().as_ref() {
        POS_X.store((fb.info.width as u32) / 2, Ordering::Relaxed);
        POS_Y.store((fb.info.height as u32) / 2, Ordering::Relaxed);
    }

    CYCLE.store(0, Ordering::Relaxed);
    READY.store(true, Ordering::SeqCst);
    debugln!("[mouse] ready");
}

#[naked]
pub extern "C" fn mouse_irq() {
    unsafe {
        core::arch::asm!(
            "cli",
            "pusha",
            "call mouse_handler",
            "popa",
            "iretd",
            options(noreturn)
        );
    }
}

#[no_mangle]
pub extern "C" fn mouse_handler() {
    // Only consume AUX data
    let status = inb(PS2_STATUS);
    if status & STATUS_OUTPUT == 0 {
        PICS.end_interrupt(MOUSE_INT);
        return;
    }
    // If not AUX, leave byte for keyboard (shouldn't happen on IRQ12)
    if status & STATUS_AUX == 0 {
        let _ = inb(PS2_DATA);
        PICS.end_interrupt(MOUSE_INT);
        return;
    }

    let byte = inb(PS2_DATA);
    let cycle = CYCLE.load(Ordering::Relaxed);

    // Byte 0 must have bit 3 set (always 1 in valid packets)
    if cycle == 0 && (byte & 0x08) == 0 {
        PICS.end_interrupt(MOUSE_INT);
        return;
    }

    PACKET[cycle as usize].store(byte, Ordering::Relaxed);
    let next = cycle + 1;
    if next >= 3 {
        CYCLE.store(0, Ordering::Relaxed);
        process_packet();
    } else {
        CYCLE.store(next, Ordering::Relaxed);
    }

    PICS.end_interrupt(MOUSE_INT);
}

fn process_packet() {
    let b0 = PACKET[0].load(Ordering::Relaxed);
    let b1 = PACKET[1].load(Ordering::Relaxed);
    let b2 = PACKET[2].load(Ordering::Relaxed);

    // Overflow → ignore move
    if b0 & 0xC0 != 0 {
        return;
    }

    let mut dx = b1 as i32;
    let mut dy = b2 as i32;
    if b0 & 0x10 != 0 {
        dx |= !0xFF; // sign extend X
    }
    if b0 & 0x20 != 0 {
        dy |= !0xFF; // sign extend Y
    }
    // PS/2 Y is opposite to screen Y
    dy = -dy;

    let buttons = b0 & 0x07;
    let prev = BUTTONS.swap(buttons, Ordering::Relaxed);
    PREV_BUTTONS.store(prev, Ordering::Relaxed);

    let (sw, sh) = screen_size();
    let mut x = POS_X.load(Ordering::Relaxed) as i32 + dx;
    let mut y = POS_Y.load(Ordering::Relaxed) as i32 + dy;
    if x < 0 {
        x = 0;
    }
    if y < 0 {
        y = 0;
    }
    if x >= sw {
        x = sw - 1;
    }
    if y >= sh {
        y = sh - 1;
    }
    POS_X.store(x as u32, Ordering::Relaxed);
    POS_Y.store(y as u32, Ordering::Relaxed);

    // Left button edges + drag move → WM (close / title drag / focus)
    if (buttons & 1) != 0 && (prev & 1) == 0 {
        crate::drivers::wm::on_mouse_down(x, y);
    } else if (buttons & 1) == 0 && (prev & 1) != 0 {
        crate::drivers::wm::on_mouse_up(x, y);
    } else if (buttons & 1) != 0 && (dx != 0 || dy != 0) {
        crate::drivers::wm::on_mouse_move(x, y);
    }

    // Redraw cursor (try_lock only — never block in IRQ)
    redraw_cursor();
}

fn screen_size() -> (i32, i32) {
    if let Some(guard) = FRAMEBUFFER.try_lock() {
        if let Some(fb) = guard.as_ref() {
            return (fb.info.width as i32, fb.info.height as i32);
        }
    }
    (800, 600)
}

/// Restore previous under-pixels and draw cursor at current position.
/// Safe from IRQ if FRAMEBUFFER.try_lock succeeds.
pub fn redraw_cursor() {
    let Some(mut guard) = FRAMEBUFFER.try_lock() else {
        return;
    };
    let Some(fb) = guard.as_mut() else {
        return;
    };

    let (x, y) = position();

    unsafe {
        // Restore old
        if CUR_DRAWN {
            for row in 0..CUR_H {
                for col in 0..CUR_W {
                    let px = CUR_OX + col as i32;
                    let py = CUR_OY + row as i32;
                    if px >= 0 && py >= 0 {
                        let c = UNDER[row * CUR_W + col];
                        fb.put_pixel_raw(px as u32, py as u32, c);
                    }
                }
            }
        }

        // Save under new position + draw
        for row in 0..CUR_H {
            for col in 0..CUR_W {
                let px = x + col as i32;
                let py = y + row as i32;
                let color = if px >= 0
                    && py >= 0
                    && (px as u32) < fb.info.width as u32
                    && (py as u32) < fb.info.height as u32
                {
                    read_pixel(fb, px as u32, py as u32)
                } else {
                    0
                };
                UNDER[row * CUR_W + col] = color;

                if CURSOR_MASK[row][col] != 0
                    && px >= 0
                    && py >= 0
                    && (px as u32) < fb.info.width as u32
                    && (py as u32) < fb.info.height as u32
                {
                    // White fill + black outline-ish via neighbors already in mask
                    let edge = row == 0
                        || col == 0
                        || CURSOR_MASK[row.saturating_sub(1)][col] == 0
                        || (col + 1 < CUR_W && CURSOR_MASK[row][col + 1] == 0);
                    let c = if edge { 0x00_00_00 } else { 0x00_F0_F0_F0 };
                    fb.put_pixel_raw(px as u32, py as u32, c);
                }
            }
        }
        CUR_OX = x;
        CUR_OY = y;
        CUR_DRAWN = true;
    }
}

fn read_pixel(fb: &crate::drivers::framebuffer::Framebuffer, x: u32, y: u32) -> u32 {
    if x >= fb.info.width as u32 || y >= fb.info.height as u32 {
        return 0;
    }
    let bpp = ((fb.info.bpp as u32 + 7) / 8) as usize;
    let offset = (y * fb.info.pitch as u32 + x * bpp as u32) as usize;
    let ptr = (fb.virt_base as *const u8).wrapping_add(offset);
    unsafe {
        let b = *ptr as u32;
        let g = *ptr.add(1) as u32;
        let r = *ptr.add(2) as u32;
        (r << 16) | (g << 8) | b
    }
}

/// After full-screen compose the under-buffer is stale — force redraw.
pub fn invalidate_cursor() {
    unsafe {
        CUR_DRAWN = false;
    }
    redraw_cursor();
}
