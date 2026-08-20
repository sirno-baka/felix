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
/// Close button size (square) inside the title bar.
pub const CLOSE_SZ: i32 = 14;
pub const CLOSE_PAD: i32 = 2;

const EV_CAP: usize = 32;

/// Userspace-visible window event (`repr(C)` — must match libfelix).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct WmEvent {
    /// See `EV_*` constants.
    pub kind: u32,
    pub a: i32,
    pub b: i32,
    pub c: i32,
    pub d: i32,
}

pub const EV_NONE: u32 = 0;
pub const EV_MOUSE_MOVE: u32 = 1;
pub const EV_MOUSE_DOWN: u32 = 2;
pub const EV_MOUSE_UP: u32 = 3;
pub const EV_KEY_DOWN: u32 = 4;
pub const EV_KEY_UP: u32 = 5;
pub const EV_CLOSE: u32 = 6;
pub const EV_FOCUS_IN: u32 = 7;
pub const EV_FOCUS_OUT: u32 = 8;

/// Fixed ring of window events (drop oldest on overflow).
struct EventQueue {
    buf: [WmEvent; EV_CAP],
    head: usize,
    tail: usize,
}

impl EventQueue {
    const fn new() -> Self {
        Self {
            buf: [WmEvent {
                kind: 0,
                a: 0,
                b: 0,
                c: 0,
                d: 0,
            }; EV_CAP],
            head: 0,
            tail: 0,
        }
    }

    fn push(&mut self, ev: WmEvent) {
        let next = (self.tail + 1) % EV_CAP;
        if next == self.head {
            // full — drop oldest
            self.head = (self.head + 1) % EV_CAP;
        }
        self.buf[self.tail] = ev;
        self.tail = next;
    }

    fn pop(&mut self) -> Option<WmEvent> {
        if self.head == self.tail {
            return None;
        }
        let ev = self.buf[self.head];
        self.head = (self.head + 1) % EV_CAP;
        Some(ev)
    }

    fn is_empty(&self) -> bool {
        self.head == self.tail
    }
}

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

    /// Caller MUST hold interrupts disabled so timer cannot switch CR3
    /// while we touch the user pointer.
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

/// Disable IF for the duration of `f`. Restores previous IF state.
#[inline]
fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    let eflags: u32;
    unsafe {
        core::arch::asm!("pushfd; pop {}", out(reg) eflags);
        core::arch::asm!("cli");
    }
    let r = f();
    if (eflags & (1 << 9)) != 0 {
        unsafe {
            core::arch::asm!("sti");
        }
    }
    r
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
    events: EventQueue,
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

/// Active title-bar drag: window id + cursor offset from window origin.
struct DragState {
    id: u8,
    grab_dx: i32,
    grab_dy: i32,
}

pub struct Compositor {
    screen_w: u32,
    screen_h: u32,
    bg: u32,
    windows: [Option<Window>; MAX_WINDOWS],
    next_id: u8,
    next_z: u8,
    drag: Option<DragState>,
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
            drag: None,
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

    /// Full redraw: clear desktop + all windows (create/destroy/move/focus).
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
        drop(guard);
        crate::drivers::mouse::invalidate_cursor();
    }

    /// Fast path for flip: only re-blit the dirty window's client surface.
    /// No full-screen clear → no flicker. Higher-z windows that overlap this
    /// one are re-drawn after so occlusion stays correct.
    pub fn compose_window(&self, id: u8) {
        let Some(w) = self.find(id) else {
            return;
        };
        if !w.visible {
            return;
        }

        let mut guard = FRAMEBUFFER.lock();
        let Some(fb) = guard.as_mut() else {
            return;
        };

        // 1. Blit this window (title + client)
        self.draw_window(fb, w);

        // 2. Re-draw any windows above it that intersect (occlusion)
        let my_z = w.z;
        let (wx, wy, ww, wh) = (w.x, w.y, w.w as i32, w.h as i32);
        for oid in self.sorted_ids().iter().flatten() {
            if *oid == id {
                continue;
            }
            if let Some(ow) = self.find(*oid) {
                if ow.z <= my_z {
                    continue;
                }
                // AABB intersect?
                let ox2 = ow.x + ow.w as i32;
                let oy2 = ow.y + ow.h as i32;
                if ow.x < wx + ww && ox2 > wx && ow.y < wy + wh && oy2 > wy {
                    self.draw_window(fb, ow);
                }
            }
        }
        drop(guard);
        crate::drivers::mouse::invalidate_cursor();
    }

    /// Compose only windows marked dirty.
    ///
    /// - single dirty window → `compose_window` (no clear, no flicker)
    /// - multiple / layout change → full `compose`
    pub fn compose_dirty(&mut self) {
        let mut dirty_ids: [Option<u8>; MAX_WINDOWS] = [None; MAX_WINDOWS];
        let mut n = 0;
        for w in self.windows.iter().flatten() {
            if w.dirty && w.visible {
                dirty_ids[n] = Some(w.id);
                n += 1;
            }
        }
        if n == 0 {
            return;
        }
        if n == 1 {
            if let Some(id) = dirty_ids[0] {
                self.compose_window(id);
            }
        } else {
            self.compose();
        }
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
        draw_close_button(fb, w);

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

    // Fast path: 32bpp — memcpy rows under cli so timer cannot switch CR3
    // away from the PD that has the LFB mapped.
    if fb.info.bpp == 32 {
        let fb_pitch = fb.info.pitch as usize;
        let src_pitch = surf.pitch as usize;
        let row_bytes = (w as usize) * 4;
        let fb_base = fb.virt_base as *mut u8;
        without_interrupts(|| unsafe {
            for row in 0..h as usize {
                let src = surf.pixels.as_ptr().add(row * src_pitch);
                let dst = fb_base.add((dy as usize + row) * fb_pitch + (dx as usize) * 4);
                core::ptr::copy_nonoverlapping(src, dst, row_bytes);
            }
        });
        return;
    }

    without_interrupts(|| {
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
                fb.put_pixel_raw(dx + col, dst_y, color);
            }
        }
    });
}

fn draw_title_text(fb: &mut Framebuffer, x: u32, y: u32, text: &str) {
    let style = MonoTextStyle::new(&FONT_6X10, Rgb888::new(0xF0, 0xF0, 0xF0));
    let pos = Point::new(x as i32, y as i32);
    let _ = Text::with_baseline(text, pos, style, Baseline::Top).draw(fb);
}

/// Close button rect in screen coords.
fn close_rect(w: &Window) -> (i32, i32, i32, i32) {
    let x1 = w.x + w.w as i32 - CLOSE_SZ - CLOSE_PAD;
    let y1 = w.y + CLOSE_PAD;
    let x2 = x1 + CLOSE_SZ;
    let y2 = y1 + CLOSE_SZ;
    (x1, y1, x2, y2)
}

fn hit_close(w: &Window, x: i32, y: i32) -> bool {
    let (x1, y1, x2, y2) = close_rect(w);
    x >= x1 && x < x2 && y >= y1 && y < y2
}

fn hit_title(w: &Window, x: i32, y: i32) -> bool {
    x >= w.x
        && x < w.x + w.w as i32
        && y >= w.y
        && y < w.y + TITLE_H as i32
}

fn draw_close_button(fb: &mut Framebuffer, w: &Window) {
    let (x1, y1, x2, y2) = close_rect(w);
    if x2 <= 0 || y2 <= 0 {
        return;
    }
    let bx = x1.max(0) as u32;
    let by = y1.max(0) as u32;
    let bw = (x2 - x1).max(0) as u32;
    let bh = (y2 - y1).max(0) as u32;
    // background
    fb.fill_rect(bx, by, bw, bh, 0x00C0_4040);
    // X via two diagonals (pixel-ish)
    let x0 = x1 + 3;
    let y0 = y1 + 3;
    let x1b = x2 - 4;
    let y1b = y2 - 4;
    // main diagonal
    let mut px = x0;
    let mut py = y0;
    while px <= x1b && py <= y1b {
        if px >= 0 && py >= 0 {
            fb.put_pixel(px as u32, py as u32, 0x00F0_F0_F0);
            if px + 1 <= x1b {
                fb.put_pixel((px + 1) as u32, py as u32, 0x00F0_F0_F0);
            }
        }
        px += 1;
        py += 1;
    }
    // anti diagonal
    px = x1b;
    py = y0;
    while px >= x0 && py <= y1b {
        if px >= 0 && py >= 0 {
            fb.put_pixel(px as u32, py as u32, 0x00F0_F0_F0);
            if px > x0 {
                fb.put_pixel((px - 1) as u32, py as u32, 0x00F0_F0_F0);
            }
        }
        if px == 0 {
            break;
        }
        px -= 1;
        py += 1;
    }
}

pub static WM: Mutex<Compositor> = Mutex::new(Compositor::empty());

/// Run `f` under kernel CR3 so LFB large-page PDE is present.
#[inline]
fn with_lfb<R>(f: impl FnOnce() -> R) -> R {
    unsafe {
        let kpd = crate::memory::paging::KERNEL_PD_PHYS;
        let mut old: u32;
        core::arch::asm!("mov {}, cr3", out(reg) old);
        if kpd != 0 && old != kpd {
            // ensure LFB large PDE exists on kernel PD
            let kdir = crate::memory::paging::phys_to_virt(kpd)
                as *mut crate::memory::paging::PageDirectory;
            let idx = (crate::drivers::framebuffer::FB_VIRT_BASE >> 22) as usize;
            let pde = (*kdir).entries[idx];
            if pde & 1 == 0 || (pde & (1 << 7)) == 0 {
                crate::drivers::framebuffer::map_lfb_large(&mut *kdir);
            }
            core::arch::asm!("mov cr3, {}", in(reg) kpd);
        }
        let r = f();
        if kpd != 0 && old != kpd {
            core::arch::asm!("mov cr3, {}", in(reg) old);
        }
        r
    }
}

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
        wm.drag = None;
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
    // Copy title under *current* (user) CR3 — user pages vanish after with_lfb.
    let mut title_buf = [0u8; 32];
    let tbytes = title.as_bytes();
    let n = tbytes.len().min(31);
    title_buf[..n].copy_from_slice(&tbytes[..n]);

    with_lfb(|| {
        let mut wm = WM.lock();
        let slot = wm.slot_free()?;
        let client_w = w;
        let client_h = h.saturating_sub(TITLE_H);
        let surface = Surface::new(client_w, client_h)?;
        println!("surface: {:p}", &surface);
        let id = wm.next_id;
        wm.next_id = wm.next_id.wrapping_add(1).max(1);
        let z = wm.next_z;
        wm.next_z = wm.next_z.wrapping_add(1);

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
            events: EventQueue::new(),
        });
        wm.compose_dirty();
        Some(id as u32)
    })
}

pub fn destroy_window(id: u32) -> bool {
    with_lfb(|| {
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
    })
}

pub fn move_window(id: u32, x: i32, y: i32) -> bool {
    with_lfb(|| {
        let mut wm = WM.lock();
        if let Some(w) = wm.find_mut(id as u8) {
            w.x = x;
            w.y = y;
            w.events = EventQueue::new();
            wm.compose();
            true
        } else {
            false
        }
    })
}

pub fn window_info(id: u32) -> Option<WindowInfo> {
    let wm = WM.lock();
    wm.find(id as u8).map(|w| w.info())
}

/// Copy pixels from user buffer into surface (full client, BGRX 32bpp),
/// mark dirty and compose.
///
/// User buffer is only valid under the *current* task CR3. A timer tick
/// mid-copy would switch page directories and page-fault (e.g. CR2=0x40e000).
pub fn flip(id: u32, user_pixels: *const u8, len: usize) -> bool {
    // copy_from_user must run under user CR3; compose under kernel CR3.
    let mut wm = WM.lock();
    if let Some(w) = wm.find_mut(id as u8) {
        if !user_pixels.is_null() && len > 0 {
            without_interrupts(|| {
                w.surface.copy_from_user(user_pixels, len);
            });
        }
        w.dirty = true;
        drop(wm);
        with_lfb(|| {
            WM.lock().compose_dirty();
        });
        true
    } else {
        false
    }
}

pub fn focus_window(id: u32) -> bool {
    with_lfb(|| {
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
    })
}

pub fn screen_size() -> (u32, u32) {
    let wm = WM.lock();
    (wm.screen_w, wm.screen_h)
}

/// Left button down: close / start title drag / focus.
pub fn on_mouse_down(x: i32, y: i32) {
    let Some(mut wm) = WM.try_lock() else {
        return;
    };
    let ids = wm.sorted_ids();
    for id in ids.iter().rev().flatten() {
        let Some(w) = wm.find(*id) else {
            continue;
        };
        if !w.visible {
            continue;
        }
        let x1 = w.x;
        let y1 = w.y;
        let x2 = w.x + w.w as i32;
        let y2 = w.y + w.h as i32;
        if x < x1 || x >= x2 || y < y1 || y >= y2 {
            continue;
        }

        let target = *id;

        // Close button?
        if hit_close(w, x, y) {
            // destroy without holding extra borrows
            for slot in wm.windows.iter_mut() {
                if slot.as_ref().map(|ww| ww.id) == Some(target) {
                    *slot = None;
                    break;
                }
            }
            wm.drag = None;
            wm.compose();
            return;
        }

        // Focus + raise + optional client click / title drag
        let in_title = hit_title(w, x, y);
        let cx = x - w.x;
        let cy = y - (w.y + TITLE_H as i32);
        let client_h = w.h.saturating_sub(TITLE_H) as i32;
        let in_client = !in_title && cx >= 0 && cy >= 0 && cx < w.w as i32 && cy < client_h;

        let new_z = wm.next_z;
        wm.next_z = wm.next_z.wrapping_add(1);
        let mut grab_dx = 0;
        let mut grab_dy = 0;
        let mut start_drag = false;
        for win in wm.windows.iter_mut().flatten() {
            let is = win.id == target;
            let was = win.focused;
            win.focused = is;
            if is {
                win.z = new_z;
                win.dirty = true;
                if !was {
                    win.events.push(WmEvent {
                        kind: EV_FOCUS_IN,
                        a: 0,
                        b: 0,
                        c: 0,
                        d: 0,
                    });
                }
                if in_title && !hit_close(win, x, y) {
                    grab_dx = x - win.x;
                    grab_dy = y - win.y;
                    start_drag = true;
                }
                if in_client {
                    win.events.push(WmEvent {
                        kind: EV_MOUSE_DOWN,
                        a: cx,
                        b: cy,
                        c: 1,
                        d: 0,
                    });
                }
            } else if was {
                win.events.push(WmEvent {
                    kind: EV_FOCUS_OUT,
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                });
            }
        }
        if start_drag {
            wm.drag = Some(DragState {
                id: target,
                grab_dx,
                grab_dy,
            });
        } else {
            wm.drag = None;
        }
        wm.compose_dirty();
        return;
    }
    wm.drag = None;
}

/// Pointer move: title drag or client MouseMove.
pub fn on_mouse_move(x: i32, y: i32) {
    let Some(mut wm) = WM.try_lock() else {
        return;
    };

    if let Some(drag) = &wm.drag {
        let id = drag.id;
        let nx = x - drag.grab_dx;
        let ny = y - drag.grab_dy;
        let max_x = wm.screen_w.saturating_sub(40) as i32;
        let max_y = wm.screen_h.saturating_sub(TITLE_H) as i32;
        let nx = nx.clamp(-((wm.screen_w as i32) / 2), max_x);
        let ny = ny.clamp(0, max_y);

        if let Some(w) = wm.find_mut(id) {
            if w.x != nx || w.y != ny {
                w.x = nx;
                w.y = ny;
                w.events = EventQueue::new(); // очистить stale events
                wm.compose();
            }
        } else {
            wm.drag = None;
        }
        return;
    }

    let ids = wm.sorted_ids();
    for id in ids.iter().rev().flatten() {
        let Some(w) = wm.find_mut(*id) else {
            continue;
        };
        if !w.visible {
            continue;
        }
        let cx = x - w.x;
        let cy = y - (w.y + TITLE_H as i32);
        let client_h = w.h.saturating_sub(TITLE_H) as i32;
        if cx >= 0 && cy >= 0 && cx < w.w as i32 && cy < client_h {
            w.events.push(WmEvent {
                kind: EV_MOUSE_MOVE,
                a: cx,
                b: cy,
                c: 0,
                d: 0,
            });
            return;
        }
        if x >= w.x && x < w.x + w.w as i32 && y >= w.y && y < w.y + w.h as i32 {
            return;
        }
    }
}

/// Left button up: end drag + client MouseUp.
pub fn on_mouse_up(x: i32, y: i32) {
    let Some(mut wm) = WM.try_lock() else {
        return;
    };
    wm.drag = None;

    let ids = wm.sorted_ids();
    for id in ids.iter().rev().flatten() {
        let Some(w) = wm.find_mut(*id) else {
            continue;
        };
        if !w.visible {
            continue;
        }
        let cx = x - w.x;
        let cy = y - (w.y + TITLE_H as i32);
        let client_h = w.h.saturating_sub(TITLE_H) as i32;
        if cx >= 0 && cy >= 0 && cx < w.w as i32 && cy < client_h {
            w.events.push(WmEvent {
                kind: EV_MOUSE_UP,
                a: cx,
                b: cy,
                c: 1,
                d: 0,
            });
            return;
        }
        if x >= w.x && x < w.x + w.w as i32 && y >= w.y && y < w.y + w.h as i32 {
            return;
        }
    }
}

/// Key to focused window. `mods`: bit0=shift, bit1=ctrl. `ch`=0 if none.
pub fn push_key(down: bool, scancode: u8, ch: u8, mods: u8) {
    let Some(mut wm) = WM.try_lock() else {
        return;
    };
    for w in wm.windows.iter_mut().flatten() {
        if w.focused && w.visible {
            println!("[KERNEL] push_key");
            w.events.push(WmEvent {
                kind: if down { EV_KEY_DOWN } else { EV_KEY_UP },
                a: scancode as i32,
                b: ch as i32,
                c: mods as i32,
                d: 0,
            });
            return;
        }
    }
}

/// Copy up to `max` events into user buffer. Returns count.
pub fn poll_events(id: u32, out: *mut WmEvent, max: usize) -> usize {
    if out.is_null() || max == 0 {
        return 0;
    }
    let mut wm = WM.lock();
    let Some(w) = wm.find_mut(id as u8) else {
        return 0;
    };
    let mut n = 0;
    while n < max {
        let Some(ev) = w.events.pop() else {
            break;
        };
        unsafe {
            *out.add(n) = ev;
        }
        n += 1;
    }
    n
}

/// Legacy: treat as down (focus).
pub fn on_mouse_click(x: i32, y: i32) {
    on_mouse_down(x, y);
}
