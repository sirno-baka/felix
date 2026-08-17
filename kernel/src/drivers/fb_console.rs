//! Software text console on the framebuffer (terminal window content).
//! Glyphs via `embedded-graphics` mono fonts (same stack as window titles).

use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyleBuilder},
    pixelcolor::Rgb888,
    prelude::*,
    text::{Baseline, Text},
};
use crate::drivers::framebuffer::{Framebuffer, FRAMEBUFFER};
use crate::sync::mutex::Mutex;

/// Must match FONT_6X10 character_size.
pub const FONT_W: u32 = 6;
pub const FONT_H: u32 = 10;

const MAX_COLS: usize = 160;
const MAX_ROWS: usize = 64;

pub struct FbConsole {
    pub enabled: bool,
    pub origin_x: u32,
    pub origin_y: u32,
    pub cols: u16,
    pub rows: u16,
    pub col: u16,
    pub row: u16,
    pub fg: Rgb888,
    pub bg: Rgb888,
    cells: [[u8; MAX_COLS]; MAX_ROWS],
}

fn rgb_u32(c: Rgb888) -> u32 {
    ((c.r() as u32) << 16) | ((c.g() as u32) << 8) | (c.b() as u32)
}

impl FbConsole {
    pub const fn new() -> Self {
        Self {
            enabled: false,
            origin_x: 0,
            origin_y: 0,
            cols: 80,
            rows: 25,
            col: 0,
            row: 0,
            fg: Rgb888::new(0xE0, 0xE0, 0xE0),
            bg: Rgb888::new(0x10, 0x18, 0x20),
            cells: [[b' '; MAX_COLS]; MAX_ROWS],
        }
    }

    pub fn setup(&mut self, ox: u32, oy: u32, width_px: u32, height_px: u32) {
        let cols = (width_px / FONT_W).min(MAX_COLS as u32) as u16;
        let rows = (height_px / FONT_H).min(MAX_ROWS as u32) as u16;
        self.origin_x = ox;
        self.origin_y = oy;
        self.cols = cols.max(1);
        self.rows = rows.max(1);
        self.col = 0;
        self.row = 0;
        self.enabled = true;
        for r in 0..MAX_ROWS {
            for c in 0..MAX_COLS {
                self.cells[r][c] = b' ';
            }
        }
        self.redraw_all();
        // Prove the console path works even before shell starts
        self.write_str("[console] ready\n");
    }

    pub fn write_char(&mut self, c: char) {
        if !self.enabled {
            return;
        }
        // Always mirror to E9 for debug
        unsafe {
            core::arch::asm!(
                "out dx, al",
                in("dx") 0xe9u16,
                in("al") c as u8,
                options(nostack, preserves_flags)
            );
        }
        match c {
            '\n' => self.newline(),
            '\r' => self.col = 0,
            '\x08' => self.backspace(),
            ch if ch.is_ascii() && (ch as u8) >= 0x20 => {
                let b = ch as u8;
                if self.col >= self.cols {
                    self.newline();
                }
                let r = self.row as usize;
                let cidx = self.col as usize;
                if r < MAX_ROWS && cidx < MAX_COLS {
                    self.cells[r][cidx] = b;
                    self.draw_cell(self.col, self.row, b);
                }
                self.col = self.col.saturating_add(1);
            }
            _ => {}
        }
    }

    pub fn write_str(&mut self, s: &str) {
        for c in s.chars() {
            self.write_char(c);
        }
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.cols.saturating_sub(1);
        } else {
            return;
        }
        let r = self.row as usize;
        let c = self.col as usize;
        if r < MAX_ROWS && c < MAX_COLS {
            self.cells[r][c] = b' ';
            self.draw_cell(self.col, self.row, b' ');
        }
    }

    fn newline(&mut self) {
        self.col = 0;
        self.row += 1;
        if self.row >= self.rows {
            self.scroll();
            self.row = self.rows - 1;
        }
    }

    fn scroll(&mut self) {
        let rows = self.rows as usize;
        let cols = self.cols as usize;
        for r in 1..rows {
            for c in 0..cols {
                self.cells[r - 1][c] = self.cells[r][c];
            }
        }
        for c in 0..cols {
            self.cells[rows - 1][c] = b' ';
        }
        self.redraw_all();
    }

    pub fn redraw_all(&self) {
        if !self.enabled {
            return;
        }
        let mut guard = FRAMEBUFFER.lock();
        let Some(fb) = guard.as_mut() else {
            return;
        };
        let w = self.cols as u32 * FONT_W;
        let h = self.rows as u32 * FONT_H;
        fb.fill_rect(self.origin_x, self.origin_y, w, h, rgb_u32(self.bg));

        for r in 0..self.rows as usize {
            for c in 0..self.cols as usize {
                let ch = self.cells[r][c];
                if ch != b' ' {
                    self.draw_glyph_on(fb, c as u16, r as u16, ch);
                }
            }
        }
    }

    fn draw_cell(&self, col: u16, row: u16, ch: u8) {
        let mut guard = FRAMEBUFFER.lock();
        if let Some(fb) = guard.as_mut() {
            self.draw_glyph_on(fb, col, row, ch);
        }
    }

    fn draw_glyph_on(&self, fb: &mut Framebuffer, col: u16, row: u16, ch: u8) {
        let gx = self.origin_x + col as u32 * FONT_W;
        let gy = self.origin_y + row as u32 * FONT_H;

        let mut tmp = [0u8; 4];
        let s = (ch as char).encode_utf8(&mut tmp);

        // Opaque cell: bg + fg (same font as window titles — known working)
        let style = MonoTextStyleBuilder::new()
            .font(&FONT_6X10)
            .text_color(self.fg)
            .background_color(self.bg)
            .build();

        let pos = Point::new(gx as i32, gy as i32);
        let _ = Text::with_baseline(s, pos, style, Baseline::Top).draw(fb);
    }
}

pub static FB_CONSOLE: Mutex<FbConsole> = Mutex::new(FbConsole::new());

pub fn is_active() -> bool {
    FB_CONSOLE.lock().enabled
}

pub fn write_str(s: &str) {
    FB_CONSOLE.lock().write_str(s);
}

pub fn write_char(c: char) {
    FB_CONSOLE.lock().write_char(c);
}
