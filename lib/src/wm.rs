//! High-level window manager API for userspace.
//!
//! Apps create a [`Window`], draw into its local BGRX buffer, then call
//! [`Window::flip`] to present. Raw syscalls stay in `crate::syscall`.
//!
//! [`Window`] implements [`embedded_graphics::draw_target::DrawTarget`] with
//! [`Rgb888`], so it works with **embedded-graphics** and **kolibri-embedded-gui**:
//!
//! ```ignore
//! use embedded_graphics::prelude::*;
//! use kolibri_embedded_gui::ui::Ui;
//!
//! let mut win = Window::create(40, 40, 400, 300, "ui").unwrap();
//! // Kolibri / eg drawing into the client area:
//! // let mut ui = Ui::new_fullscreen(&mut win, my_rgb888_style());
//! win.flip();
//! ```

use alloc::vec;
use alloc::vec::Vec;

use embedded_graphics::{mono_font, pixelcolor::Rgb888, prelude::*, Pixel};
use kolibri_embedded_gui::style::{Spacing, Style};
use embedded_graphics::primitives::Rectangle;
use crate::syscall::{self};

pub use crate::syscall::WindowInfo;


pub fn medsize_rgb888_style() -> Style<Rgb888> {
    Style {
        background_color: Rgb888::new(0x7, 0x10, 0x6), // pretty dark gray
        item_background_color: Rgb888::new(0x2, 0x4, 0x2), // darker gray
        highlight_item_background_color: Rgb888::new(0x1, 0x2, 0x1),
        border_color: Rgb888::WHITE,
        highlight_border_color: Rgb888::WHITE,
        primary_color: Rgb888::CSS_DARK_CYAN,
        secondary_color: Rgb888::YELLOW,
        icon_color: Rgb888::WHITE,
        text_color: Rgb888::WHITE,
        default_widget_height: 16,
        border_width: 0,
        highlight_border_width: 1,
        default_font: mono_font::iso_8859_10::FONT_9X15,
        spacing: Spacing {
            item_spacing: Size::new(8, 4),
            button_padding: Size::new(5, 5),
            default_padding: Size::new(1, 1),
            window_border_padding: Size::new(3, 3),
        },
    }
}

/// Screen resolution reported by the kernel WM.
pub fn screen_size() -> (u32, u32) {
    let mut out = [0u32; 2];
    let ok = unsafe { syscall::wm_screen_size(out.as_mut_ptr()) };
    if ok == 0 {
        (out[0].max(1), out[1].max(1))
    } else {
        (800, 600) // fallback
    }
}

/// Color as `0x00RR_GGBB` (same convention as kernel `put_pixel`).
#[inline]
pub fn rgb(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// A client window owned by this process.
///
/// Pixel buffer is BGRX 32bpp, row pitch = `client_w * 4`.
pub struct Window {
    id: u32,
    info: WindowInfo,
    /// Local client surface (BGRX). Drawn by the app, sent on flip.
    buffer: Vec<u8>,
    alive: bool,
}

impl Window {
    /// Create a window. `title` is truncated to 31 bytes.
    /// Returns `None` if the kernel rejects the request (too small, no slots, …).
    pub fn create(x: i32, y: i32, w: u32, h: u32, title: &str) -> Option<Self> {
        let mut title_buf = [0u8; 32];
        let bytes = title.as_bytes();
        let n = bytes.len().min(31);
        title_buf[..n].copy_from_slice(&bytes[..n]);

        let id = unsafe {
            syscall::wm_create(x, y, w, h, title_buf.as_ptr())
        };
        if id == usize::MAX {
            return None;
        }
        let id = id as u32;

        let mut info = WindowInfo {
            id: 0,
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            client_w: 0,
            client_h: 0,
            pitch: 0,
            focused: 0,
        };
        let ok = unsafe { syscall::wm_info(id, &mut info) };
        if ok != 0 || info.client_w == 0 || info.client_h == 0 {
            let _ = unsafe { syscall::wm_destroy(id) };
            return None;
        }

        let size = (info.pitch as usize).saturating_mul(info.client_h as usize);
        let mut buffer = vec![0u8; size];
        // Default dark client background (matches kernel surface init).
        for chunk in buffer.chunks_exact_mut(4) {
            chunk[0] = 0x20; // B
            chunk[1] = 0x18; // G
            chunk[2] = 0x10; // R
            chunk[3] = 0x00;
        }

        Some(Self {
            id,
            info,
            buffer,
            alive: true,
        })
    }

    /// Almost-fullscreen window with equal margins on all sides.
    pub fn create_with_margin(margin: u32, title: &str) -> Option<Self> {
        let (sw, sh) = screen_size();
        let w = sw.saturating_sub(margin.saturating_mul(2)).max(80);
        let h = sh.saturating_sub(margin.saturating_mul(2)).max(60);
        Self::create(margin as i32, margin as i32, w, h, title)
    }

    #[inline]
    pub fn id(&self) -> u32 {
        self.id
    }

    #[inline]
    pub fn info(&self) -> WindowInfo {
        self.info
    }

    #[inline]
    pub fn client_width(&self) -> u32 {
        self.info.client_w
    }

    #[inline]
    pub fn client_height(&self) -> u32 {
        self.info.client_h
    }

    /// Bytes per row (usually `client_w * 4`).
    #[inline]
    pub fn pitch(&self) -> usize {
        self.info.pitch as usize
    }

    #[inline]
    pub fn buffer(&self) -> &[u8] {
        &self.buffer
    }

    #[inline]
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        &mut self.buffer
    }

    /// Refresh cached [`WindowInfo`] from the kernel.
    pub fn refresh_info(&mut self) -> bool {
        let mut info = self.info;
        let ok = unsafe { syscall::wm_info(self.id, &mut info) };
        if ok == 0 {
            self.info = info;
            true
        } else {
            false
        }
    }

    /// Move the window (title bar top-left).
    pub fn move_to(&mut self, x: i32, y: i32) -> bool {
        let ok = unsafe { syscall::wm_move(self.id, x, y) };
        if ok == 0 {
            self.info.x = x;
            self.info.y = y;
            true
        } else {
            false
        }
    }

    /// Raise and focus this window.
    pub fn focus(&self) -> bool {
        unsafe { syscall::wm_focus(self.id) == 0 }
    }

    /// Copy local buffer → kernel surface and compose to the LFB.
    pub fn flip(&self) -> bool {
        unsafe {
            syscall::wm_flip(self.id, self.buffer.as_ptr(), self.buffer.len()) == 0
        }
    }

    /// Destroy the window. Consumes `self`.
    pub fn destroy(mut self) -> bool {
        self.close_internal()
    }

    fn close_internal(&mut self) -> bool {
        if !self.alive {
            return true;
        }
        self.alive = false;
        unsafe { syscall::wm_destroy(self.id) == 0 }
    }

    // ---------- drawing helpers (BGRX buffer) ----------

    /// Fill entire client area. `color` is `0x00RR_GGBB`.
    pub fn fill(&mut self, color: u32) {
        let b = (color & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let r = ((color >> 16) & 0xFF) as u8;
        for chunk in self.buffer.chunks_exact_mut(4) {
            chunk[0] = b;
            chunk[1] = g;
            chunk[2] = r;
            chunk[3] = 0;
        }
    }

    /// Set one pixel. Out-of-bounds is ignored. `color` is `0x00RR_GGBB`.
    pub fn put_pixel(&mut self, x: u32, y: u32, color: u32) {
        if x >= self.info.client_w || y >= self.info.client_h {
            return;
        }
        let pitch = self.pitch();
        let off = (y as usize) * pitch + (x as usize) * 4;
        if off + 3 >= self.buffer.len() {
            return;
        }
        self.buffer[off] = (color & 0xFF) as u8;
        self.buffer[off + 1] = ((color >> 8) & 0xFF) as u8;
        self.buffer[off + 2] = ((color >> 16) & 0xFF) as u8;
        self.buffer[off + 3] = 0;
    }

    /// Axis-aligned filled rectangle. `color` is `0x00RR_GGBB`.
    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        if w == 0 || h == 0 {
            return;
        }
        let cw = self.info.client_w;
        let ch = self.info.client_h;
        let x1 = x.min(cw);
        let y1 = y.min(ch);
        let x2 = x.saturating_add(w).min(cw);
        let y2 = y.saturating_add(h).min(ch);
        if x1 >= x2 || y1 >= y2 {
            return;
        }
        let b = (color & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let r = ((color >> 16) & 0xFF) as u8;
        let pitch = self.pitch();
        for row in y1..y2 {
            let base = (row as usize) * pitch;
            for col in x1..x2 {
                let off = base + (col as usize) * 4;
                if off + 3 >= self.buffer.len() {
                    break;
                }
                self.buffer[off] = b;
                self.buffer[off + 1] = g;
                self.buffer[off + 2] = r;
                self.buffer[off + 3] = 0;
            }
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = self.close_internal();
    }
}

// ---------------------------------------------------------------------------
// embedded-graphics + kolibri-embedded-gui compatibility
// ---------------------------------------------------------------------------

impl OriginDimensions for Window {
    fn size(&self) -> Size {
        Size::new(self.info.client_w, self.info.client_h)
    }
}

impl DrawTarget for Window {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let cw = self.info.client_w as i32;
        let ch = self.info.client_h as i32;
        let pitch = self.pitch();
        let buf_len = self.buffer.len();

        for Pixel(coord, color) in pixels.into_iter() {
            if coord.x < 0 || coord.y < 0 || coord.x >= cw || coord.y >= ch {
                continue;
            }
            let off = (coord.y as usize) * pitch + (coord.x as usize) * 4;
            if off + 3 >= buf_len {
                continue;
            }
            // BGRX
            self.buffer[off] = color.b();
            self.buffer[off + 1] = color.g();
            self.buffer[off + 2] = color.r();
            self.buffer[off + 3] = 0;
        }
        Ok(())
    }

    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        let cw = self.info.client_w;
        let ch = self.info.client_h;
        if area.size.width == 0 || area.size.height == 0 {
            return Ok(());
        }

        let x0 = area.top_left.x.max(0) as u32;
        let y0 = area.top_left.y.max(0) as u32;
        let x1 = (area.top_left.x + area.size.width as i32).max(0) as u32;
        let y1 = (area.top_left.y + area.size.height as i32).max(0) as u32;
        let x0 = x0.min(cw);
        let y0 = y0.min(ch);
        let x1 = x1.min(cw);
        let y1 = y1.min(ch);
        if x0 >= x1 || y0 >= y1 {
            return Ok(());
        }

        let pitch = self.pitch();
        let mut colors = colors.into_iter();

        // Skip colors that fall outside the left/top clip of the source area.
        let skip_x = if area.top_left.x < 0 {
            (-area.top_left.x) as u32
        } else {
            0
        };
        let skip_y = if area.top_left.y < 0 {
            (-area.top_left.y) as u32
        } else {
            0
        };
        let full_w = area.size.width;

        for _ in 0..skip_y {
            for _ in 0..full_w {
                let _ = colors.next();
            }
        }

        for y in y0..y1 {
            for _ in 0..skip_x {
                let _ = colors.next();
            }
            let base = (y as usize) * pitch;
            for x in x0..x1 {
                let Some(color) = colors.next() else {
                    return Ok(());
                };
                let off = base + (x as usize) * 4;
                if off + 3 >= self.buffer.len() {
                    break;
                }
                self.buffer[off] = color.b();
                self.buffer[off + 1] = color.g();
                self.buffer[off + 2] = color.r();
                self.buffer[off + 3] = 0;
            }
            // Drain remaining pixels of this source row past the right edge.
            let drawn = x1 - x0;
            let remaining = full_w.saturating_sub(skip_x + drawn);
            for _ in 0..remaining {
                let _ = colors.next();
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        if area.size.width == 0 || area.size.height == 0 {
            return Ok(());
        }
        let x = area.top_left.x.max(0) as u32;
        let y = area.top_left.y.max(0) as u32;
        // Clamp size against negative origin.
        let x_end = (area.top_left.x + area.size.width as i32).max(0) as u32;
        let y_end = (area.top_left.y + area.size.height as i32).max(0) as u32;
        let w = x_end.saturating_sub(x);
        let h = y_end.saturating_sub(y);
        self.fill_rect(x, y, w, h, rgb(color.r(), color.g(), color.b()));
        Ok(())
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        self.fill(rgb(color.r(), color.g(), color.b()));
        Ok(())
    }
}
