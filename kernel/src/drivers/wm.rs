//! In-kernel window manager / compositor.
//!
//! Apps create windows via syscalls, draw into a user buffer, then
//! `wm_flip` copies pixels into the window surface and composes to the LFB.
//! Title bars are drawn only by the WM. No resize in v1. Max 8 windows.

use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyle},
    pixelcolor::Rgb888,
    prelude::*,
    text::{Baseline, Text},
};
use crate::drivers::framebuffer::{Framebuffer, FRAMEBUFFER};
use crate::sync::mutex::Mutex;
use crate::{debugln, println};

pub const MAX_WINDOWS: usize = 8;
pub const TITLE_H: u32 = 18;

/// Set after successful `init()` — kernel print goes to E9 only.
pub static WM_READY: AtomicBool = AtomicBool::new(false);

pub fn is_ready() -> bool {
    WM_READY.load(Ordering::Relaxed)
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct WindowInfo {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub client_w: u32,
    pub client_h: u32,
    pub pitch: u32,
    pub focused: u32,
}

struct Surface {
    width: u32,
    height: u32,
    pitch: u32,
    pixels: Vec<u8>,
}

impl Surface {
    fn new(width: u32, height: u32) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        let pitch = width.saturating_mul(4);
        let size = (pitch as usize).checked_mul(height as usize)?;
        // Cap single surface (~8 MiB)
        if size > 8 * 1024 * 1024 {
            return None;
        }
        let mut pixels = vec![0u8; size];
        // dark client bg
        for chunk in pixels.chunks_exact_mut(4) {
            chunk[0] = 0x20; // B
            chunk[1] = 0x18; // G
            chunk[2] = 0x10; // R
            chunk[3] = 0x00;
        }
        Some(Self {
            width,
            height,
            pitch,
            pixels,
        })
    }

    fn clear(&mut self, color: u32) {
        let b = (color & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let r = ((color >> 16) & 0xFF) as u8;
        for chunk in self.pixels.chunks_exact_mut(4) {
            chunk[0] = b;
            chunk[1] = g;
            chunk[2] = r;
            chunk[3] = 0;
        }
    }

    fn copy_from_user(&mut self, src: *const u8, len: usize) {
        if src.is_null() || len == 0 {
            return;
        }
        let n = len.min(self.pixels.len());
        unsafe {
            core::ptr::copy_nonoverlapping(src, self.pixels.as_mut_ptr(), n);
        }
    }
}

struct Window {
    id: u8,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    z: u8,
    focused: bool,
    visible: bool,
    dirty: bool,
    title: [u8; 32],
    surface: Surface,
    owner_slot: i8, // task slot that created it (-1 = kernel)
}

impl Window {
    fn client_rect(&self) -> (i32, i32, u32, u32) {
        let cx = self.x;
        let cy = self.y + TITLE_H as i32;
        let cw = self.w;
        let ch = self.h.saturating_sub(TITLE_H);
        (cx, cy, cw, ch)
    }

    fn title_str(&self) -> &str {
        let end = self
            .title
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(self.title.len());
        core::str::from_utf8(&self.title[..end]).unwrap_or("")
    }

    fn info(&self) -> WindowInfo {
        WindowInfo {
            id: self.id as u32,
            x: self.x,
            y: self.y,
            w: self.w,
            h: self.h,
            client_w: self.surface.width,
            client_h: self.surface.height,
            pitch: self.surface.pitch,
            focused: if self.focused { 1 } else { 0 },
        }
    }
}

pub struct Compositor {
    screen_w: u32,
    screen_h: u32,
    bg: u32,
    windows: [Option<Window>; MAX_WINDOWS],
    next_id: u8,
    next_z: u8,
}

impl Compositor {
    pub const fn empty() -> Self {
        Self {
            screen_w: 0,
            screen_h: 0,
            bg: 0x0020_2830,
            windows: [None, None, None, None, None, None, None, None],
            next_id: 1,
            next_z: 1,
        }
    }

    fn slot_free(&self) -> Option<usize> {
        self.windows.iter().position(|w| w.is_none())
    }

    fn find_mut(&mut self, id: u8) -> Option<&mut Window> {
        self.windows
            .iter_mut()
            .filter_map(|s| s.as_mut())
            .find(|w| w.id == id)
    }

    fn find(&self, id: u8) -> Option<&Window> {
        self.windows
            .iter()
            .filter_map(|s| s.as_ref())
            .find(|w| w.id == id)
    }

    fn sorted_ids(&self) -> [Option<u8>; MAX_WINDOWS] {
        let mut ids: [Option<u8>; MAX_WINDOWS] = [None; MAX_WINDOWS];
        let mut zs: [u8; MAX_WINDOWS] = [0; MAX_WINDOWS];
        let mut n = 0;
        for w in self.windows.iter().flatten() {
            if !w.visible {
                continue;
            }
            ids[n] = Some(w.id);
            zs[n] = w.z;
            n += 1;
        }
        // insertion sort by z ascending
        for i in 1..n {
            let mut j = i;
            while j > 0 && zs[j - 1] > zs[j] {
                zs.swap(j - 1, j);
                ids.swap(j - 1, j);
                j -= 1;
            }
        }
        ids
    }

    pub fn compose(&self) {
        let mut guard = FRAMEBUFFER.lock();
        let Some(fb) = guard.as_mut() else {
            return;
        };
        fb.fill_fast(self.bg);

        for id in self.sorted_ids().iter().flatten() {
            if let Some(w) = self.find(*id) {
                self.draw_window(fb, w);
            }
        }
    }

    /// Compose only windows marked dirty (still redraws full stack for simplicity
    /// of occlusion — dirty flag skips work only when nothing is dirty).
    pub fn compose_dirty(&mut self) {
        let any = self
            .windows
            .iter()
            .flatten()
            .any(|w| w.dirty && w.visible);
        if !any {
            return;
        }
        self.compose();
        for w in self.windows.iter_mut().flatten() {
            w.dirty = false;
        }
    }

    fn draw_window(&self, fb: &mut Framebuffer, w: &Window) {
        let x = w.x.max(0) as u32;
        let y = w.y.max(0) as u32;
        let sw = self.screen_w;
        let sh = self.screen_h;
        if x >= sw || y >= sh {
            return;
        }

        let win_w = w.w.min(sw - x);
        let win_h = w.h.min(sh - y);

        let title_color = if w.focused {
            0x003A_7CA5
        } else {
            0x0040_4850
        };
        let th = TITLE_H.min(win_h);
        fb.fill_rect(x, y, win_w, th, title_color);
        if th > 0 {
            fb.fill_rect(x, y + th.saturating_sub(1), win_w, 1, 0x0010_1010);
        }
        draw_title_text(fb, x + 6, y + 4, w.title_str());

        // client surface blit
        let cy = y + TITLE_H;
        if cy >= y + win_h {
            return;
        }
        let ch = (y + win_h).saturating_sub(cy);
        let cw = win_w.min(w.surface.width);
        let ch = ch.min(w.surface.height);
        blit_surface(fb, x, cy, cw, ch, &w.surface);

        // border
        fb.fill_rect(x, y, win_w, 1, 0x0000_0000);
        if win_h > 0 {
            fb.fill_rect(x, y + win_h - 1, win_w, 1, 0x0000_0000);
        }
        fb.fill_rect(x, y, 1, win_h, 0x0000_0000);
        if win_w > 0 {
            fb.fill_rect(x + win_w - 1, y, 1, win_h, 0x0000_0000);
        }
    }
}

fn blit_surface(fb: &mut Framebuffer, dx: u32, dy: u32, w: u32, h: u32, surf: &Surface) {
    let fb_w = fb.info.width as u32;
    let fb_h = fb.info.height as u32;
    if dx >= fb_w || dy >= fb_h || w == 0 || h == 0 {
        return;
    }
    let w = w.min(fb_w - dx).min(surf.width);
    let h = h.min(fb_h - dy).min(surf.height);

    // put_pixel already does without_interrupts; OK for v1 correctness.
    for row in 0..h {
        let src_off = (row as usize) * (surf.pitch as usize);
        let dst_y = dy + row;
        for col in 0..w {
            let s = src_off + (col as usize) * 4;
            if s + 3 >= surf.pixels.len() {
                break;
            }
            let b = surf.pixels[s] as u32;
            let g = surf.pixels[s + 1] as u32;
            let r = surf.pixels[s + 2] as u32;
            let color = (r << 16) | (g << 8) | b;
            fb.put_pixel(dx + col, dst_y, color);
        }
    }
}

fn draw_title_text(fb: &mut Framebuffer, x: u32, y: u32, text: &str) {
    let style = MonoTextStyle::new(&FONT_6X10, Rgb888::new(0xF0, 0xF0, 0xF0));
    let pos = Point::new(x as i32, y as i32);
    let _ = Text::with_baseline(text, pos, style, Baseline::Top).draw(fb);
}

pub static WM: Mutex<Compositor> = Mutex::new(Compositor::empty());

/// After FB init: clear screen, mark ready. Windows are created by apps.
pub fn init() {
    debugln!("[wm] init");
    let (sw, sh) = {
        let guard = FRAMEBUFFER.lock();
        match guard.as_ref() {
            Some(fb) => (fb.info.width as u32, fb.info.height as u32),
            None => {
                debugln!("[wm] no framebuffer");
                return;
            }
        }
    };

    {
        let mut wm = WM.lock();
        wm.screen_w = sw;
        wm.screen_h = sh;
        wm.bg = 0x0020_2830;
        wm.windows = [None, None, None, None, None, None, None, None];
        wm.next_id = 1;
        wm.next_z = 1;
        wm.compose();
    }

    WM_READY.store(true, Ordering::SeqCst);
    // Kernel logs stay on E9 — do not use println to FB.
    debugln!("[wm] ready {}x{}", sw, sh);
}

pub fn create_window(
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    title: &str,
    owner_slot: i8,
) -> Option<u32> {
    if w < 40 || h < TITLE_H + 8 {
        return None;
    }
    let mut wm = WM.lock();
    let slot = wm.slot_free()?;
    let client_w = w;
    let client_h = h.saturating_sub(TITLE_H);
    let surface = Surface::new(client_w, client_h)?;
    let id = wm.next_id;
    wm.next_id = wm.next_id.wrapping_add(1).max(1);
    let z = wm.next_z;
    wm.next_z = wm.next_z.wrapping_add(1);

    let mut title_buf = [0u8; 32];
    let tbytes = title.as_bytes();
    let n = tbytes.len().min(31);
    title_buf[..n].copy_from_slice(&tbytes[..n]);

    // unfocus others
    for win in wm.windows.iter_mut().flatten() {
        win.focused = false;
    }

    wm.windows[slot] = Some(Window {
        id,
        x,
        y,
        w,
        h,
        z,
        focused: true,
        visible: true,
        dirty: true,
        title: title_buf,
        surface,
        owner_slot,
    });
    wm.compose_dirty();
    Some(id as u32)
}

pub fn destroy_window(id: u32) -> bool {
    let mut wm = WM.lock();
    let id = id as u8;
    for slot in wm.windows.iter_mut() {
        if slot.as_ref().map(|w| w.id) == Some(id) {
            *slot = None;
            wm.compose();
            return true;
        }
    }
    false
}

pub fn move_window(id: u32, x: i32, y: i32) -> bool {
    let mut wm = WM.lock();
    if let Some(w) = wm.find_mut(id as u8) {
        w.x = x;
        w.y = y;
        w.dirty = true;
        wm.compose_dirty();
        true
    } else {
        false
    }
}

pub fn window_info(id: u32) -> Option<WindowInfo> {
    let wm = WM.lock();
    wm.find(id as u8).map(|w| w.info())
}

/// Copy pixels from user buffer into surface (full client, RGBX/BGRx 32bpp),
/// mark dirty and compose.
pub fn flip(id: u32, user_pixels: *const u8, len: usize) -> bool {
    let mut wm = WM.lock();
    if let Some(w) = wm.find_mut(id as u8) {
        if !user_pixels.is_null() && len > 0 {
            w.surface.copy_from_user(user_pixels, len);
        }
        w.dirty = true;
        wm.compose_dirty();
        true
    } else {
        false
    }
}

pub fn focus_window(id: u32) -> bool {
    let mut wm = WM.lock();
    let id = id as u8;
    if wm.find(id).is_none() {
        return false;
    }
    let new_z = wm.next_z;
    wm.next_z = wm.next_z.wrapping_add(1);
    for w in wm.windows.iter_mut().flatten() {
        let is_target = w.id == id;
        w.focused = is_target;
        if is_target {
            w.z = new_z;
            w.dirty = true;
        }
    }
    wm.compose_dirty();
    true
}

pub fn screen_size() -> (u32, u32) {
    let wm = WM.lock();
    (wm.screen_w, wm.screen_h)
}
