//! In-kernel window manager / compositor.
//!
//! Apps create windows via syscalls, draw into a user buffer, then
//! `wm_flip` copies pixels into the window surface and composes to the LFB.
//! Title bars are drawn only by the WM. No resize in v1. Max 8 windows.

use crate::drivers::framebuffer::{FRAMEBUFFER, Framebuffer};
use crate::sync::mutex::Mutex;
use crate::{debugln, println};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_6X10},
    pixelcolor::Rgb888,
    prelude::*,
    text::{Baseline, Text},
};
use crate::drivers::wm_flags::WindowFlags;
use crate::utils::flags::{FlagOp, Flags};

pub const MAX_WINDOWS: usize = 8;
pub const TITLE_H: u32 = 18;
/// Close button size (square) inside the title bar.
pub const CLOSE_SZ: i32 = 14;
pub const CLOSE_PAD: i32 = 2;
/// Bottom-right resize grip.
pub const RESIZE_SZ: i32 = 12;

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
/// Client size changed: a=client_w, b=client_h.
pub const EV_RESIZE: u32 = 9;

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
pub struct WmFlipRect {
    pub x: u32, pub y: u32, pub w: u32, pub h: u32, pub pitch: u32,
    pub pixels: *const u8,
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

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowListItem {
    pub id: u8,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub focused: u8,
    pub visible: u8,
    pub owner_slot: i8,
    pub title: [u8; 32],
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
        // Cap single surface (~8 MiB) — kernel heap is only 16 MiB total.
        if size > 8 * 1024 * 1024 {
            crate::debugln!(
                "[wm] surface too large {}x{} = {} bytes (>8MiB)",
                width,
                height,
                size
            );
            return None;
        }
        let mut pixels = alloc::vec::Vec::new();
        if pixels.try_reserve_exact(size).is_err() {
            crate::debugln!(
                "[wm] surface OOM {}x{} = {} bytes (kernel heap)",
                width,
                height,
                size
            );
            return None;
        }
        pixels.resize(size, 0);
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
    fn copy_rect_from_user(&mut self, src: *const u8, src_pitch: u32, x: u32, y: u32, w: u32, h: u32) {
        if src.is_null() || src_pitch < 4 { return; }
        let w = w.min(self.width.saturating_sub(x)); let h = h.min(self.height.saturating_sub(y));
        let row = (w as usize).saturating_mul(4);
        for yy in 0..h as usize {
            let from = unsafe { src.add((y as usize + yy).saturating_mul(src_pitch as usize) + x as usize * 4) };
            let to = (y as usize + yy).saturating_mul(self.pitch as usize) + x as usize * 4;
            if to + row > self.pixels.len() { break; }
            unsafe { core::ptr::copy_nonoverlapping(from, self.pixels.as_mut_ptr().add(to), row); }
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
    title: [u8; 32],
    flags: WindowFlags,
    surface: Surface,
    owner_slot: i8, // task slot that created it (-1 = kernel)
    events: EventQueue,
}

impl Window {
    // Добавь эти методы в начало impl Window
    fn has_title_bar(&self) -> bool {
        !self.flags.is_enable(WindowFlags::FRAMELESS_WINDOW_HINT) &&
            self.flags.is_enable(WindowFlags::WINDOW_TITLE_HINT)
    }

    fn title_height(&self) -> u32 {
        if self.has_title_bar() { TITLE_H } else { 0 }
    }

    fn client_rect(&self) -> (i32, i32, u32, u32) {
        let th = self.title_height() as i32;
        (self.x, self.y + th, self.w, self.h.saturating_sub(th as u32))
    }


    fn title_str(&self) -> &str {
        let end = self
            .title
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(self.title.len());
        core::str::from_utf8(&self.title[..end]).unwrap_or("")
    }

    fn rect(&self) -> DirtyRect {
        DirtyRect::new(self.x, self.y, self.w, self.h)
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
#[derive(Clone, Copy)]
struct DragState {
    id: u8,
    grab_dx: i32,
    grab_dy: i32,
    /// 0 = title move, 1 = bottom-right resize
    kind: u8,
    orig_w: u32,
    orig_h: u32,
}

#[derive(Clone, Copy, Debug)]
struct DirtyRect {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
}

impl DirtyRect {
    const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }

    fn right(self) -> i32 {
        self.x.saturating_add(self.w as i32)
    }

    fn bottom(self) -> i32 {
        self.y.saturating_add(self.h as i32)
    }

    fn intersects(self, other: Self) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    fn intersection(self, other: Self) -> Option<Self> {
        if !self.intersects(other) {
            return None;
        }
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Some(Self::new(
            x,
            y,
            right.saturating_sub(x) as u32,
            bottom.saturating_sub(y) as u32,
        ))
    }

    fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self::new(
            x,
            y,
            right.saturating_sub(x) as u32,
            bottom.saturating_sub(y) as u32,
        )
    }
}

pub struct Compositor {
    screen_w: u32,
    screen_h: u32,
    bg: u32,
    windows: [Option<Window>; MAX_WINDOWS],
    next_id: u8,
    next_z: u8,
    drag: Option<DragState>,
    dirty: Option<DirtyRect>,
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
            dirty: None,
        }
    }

    pub fn get_window_list(&self) -> Vec<WindowListItem> {
        self.windows
            .iter()
            .filter_map(|opt| opt.as_ref())
            .map(|w| WindowListItem {
                id: w.id,
                x: w.x,
                y: w.y,
                w: w.w,
                h: w.h,
                focused: u8::from(w.focused),
                visible: u8::from(w.visible),
                owner_slot: w.owner_slot,
                title: w.title,
            })
            .collect()
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

    fn mark_dirty(&mut self, rect: DirtyRect) {
        self.dirty = Some(match self.dirty {
            Some(old) => old.union(rect),
            None => rect,
        });
    }

    fn mark_window_dirty(&mut self, id: u8) {
        if let Some(w) = self.find(id) {
            self.mark_dirty(w.rect());
        }
    }

    /// Full redraw. Kept for initial framebuffer setup / explicit full invalidate.
    pub fn compose(&self) {
        let mut guard = FRAMEBUFFER.lock();
        let Some(fb) = guard.as_mut() else {
            return;
        };
        fb.fill_fast(self.bg);

        for id in self.sorted_ids().iter().flatten() {
            if let Some(w) = self.find(*id) {
                self.draw_window_clipped(fb, w, DirtyRect::new(0, 0, self.screen_w, self.screen_h));
            }
        }
        drop(guard);
        crate::drivers::mouse::invalidate_cursor();
    }

    /// Compose one dirty region. The region is first restored from the desktop
    /// background, then every visible window intersecting it is replayed in Z order.
    fn compose_region(&self, fb: &mut Framebuffer, dirty: DirtyRect) {
        if dirty.is_empty() {
            return;
        }

        fill_rect_clipped(fb, dirty, 0, 0, self.screen_w, self.screen_h, self.bg);

        for id in self.sorted_ids().iter().flatten() {
            if let Some(w) = self.find(*id) {
                if w.rect().intersects(dirty) {
                    self.draw_window_clipped(fb, w, dirty);
                }
            }
        }
    }

    /// Redraw a region already containing the background/underlying pixels.
    ///
    /// The target window is opaque, so its new position can be painted
    /// directly without clearing the region first. Higher-z windows are
    /// replayed afterwards for occlusion.
    fn compose_opaque_region(&self, fb: &mut Framebuffer, id: u8, region: DirtyRect) {
        if region.is_empty() {
            return;
        }

        let Some(target) = self.find(id) else {
            return;
        };
        if !target.visible || !target.rect().intersects(region) {
            return;
        }

        let target_z = target.z;

        // Paint the moved window first. No background clear here.
        self.draw_window_clipped(fb, target, region);

        // Restore occlusion from windows above it.
        for oid in self.sorted_ids().iter().flatten() {
            if *oid == id {
                continue;
            }

            if let Some(w) = self.find(*oid) {
                if w.z > target_z && w.rect().intersects(region) {
                    self.draw_window_clipped(fb, w, region);
                }
            }
        }
    }

    /// Smooth interactive window movement.
    ///
    /// Paint the new window position first, then restore only the part of
    /// the old position that became exposed. This deliberately avoids the
    /// clear-first path used by compose_region(), which causes visible
    /// background flashes during mouse dragging.
    fn compose_move(&self, id: u8, old: DirtyRect, new: DirtyRect) {
        if old.is_empty() && new.is_empty() {
            return;
        }

        let mut guard = FRAMEBUFFER.lock();
        let Some(fb) = guard.as_mut() else {
            return;
        };

        // 1. Paint new position first.
        self.compose_opaque_region(fb, id, new);

        // 2. Restore old exposed pixels only.
        if let Some(overlap) = old.intersection(new) {
            let old_right = old.right();
            let old_bottom = old.bottom();
            let overlap_right = overlap.right();
            let overlap_bottom = overlap.bottom();

            // old - new = at most four rectangles.
            let strips = [
                // top
                DirtyRect::new(
                    old.x,
                    old.y,
                    old.w,
                    overlap.y.saturating_sub(old.y) as u32,
                ),
                // bottom
                DirtyRect::new(
                    old.x,
                    overlap_bottom,
                    old.w,
                    old_bottom.saturating_sub(overlap_bottom) as u32,
                ),
                // left
                DirtyRect::new(
                    old.x,
                    overlap.y,
                    overlap.x.saturating_sub(old.x) as u32,
                    overlap.h,
                ),
                // right
                DirtyRect::new(
                    overlap_right,
                    overlap.y,
                    old_right.saturating_sub(overlap_right) as u32,
                    overlap.h,
                ),
            ];

            for rect in strips {
                if !rect.is_empty() {
                    self.compose_region(fb, rect);
                }
            }
        } else {
            // Completely different position: restore the entire old area.
            self.compose_region(fb, old);
        }

        drop(guard);
        crate::drivers::mouse::invalidate_cursor();
    }

    /// Region-aware client update. It is deliberately routed through the same
    /// dirty-region compositor so exposed pixels and Z-order are always correct.
    pub fn compose_client_rect(&self, id: u8, rx: u32, ry: u32, rw: u32, rh: u32) {
        let Some(w) = self.find(id) else { return; };
        if !w.visible || rw == 0 || rh == 0 {
            return;
        }

        let rect = DirtyRect::new(
            w.x.saturating_add(rx as i32),
            w.y.saturating_add(w.title_height() as i32).saturating_add(ry as i32),
            rw,
            rh,
        );

        let mut guard = FRAMEBUFFER.lock();
        let Some(fb) = guard.as_mut() else { return; };
        self.compose_region(fb, rect);
        drop(guard);
        crate::drivers::mouse::invalidate_cursor();
    }

    /// Compatibility wrapper: compose exactly this window's current rectangle.
    pub fn compose_window(&self, id: u8) {
        let Some(w) = self.find(id) else { return; };
        if !w.visible {
            return;
        }

        let mut guard = FRAMEBUFFER.lock();
        let Some(fb) = guard.as_mut() else { return; };
        self.compose_region(fb, w.rect());
        drop(guard);
        crate::drivers::mouse::invalidate_cursor();
    }

    /// Consume the compositor dirty region.
    ///
    /// No full-screen clear and no whole-window replay: only the merged dirty
    /// rectangle is restored and only intersecting windows are redrawn.
    pub fn compose_dirty(&mut self) {
        let Some(dirty) = self.dirty.take() else {
            return;
        };

        let mut guard = FRAMEBUFFER.lock();
        let Some(fb) = guard.as_mut() else {
            self.dirty = Some(dirty);
            return;
        };

        self.compose_region(fb, dirty);
        drop(guard);
        crate::drivers::mouse::invalidate_cursor();
    }

    fn draw_window_clipped(&self, fb: &mut Framebuffer, w: &Window, clip: DirtyRect) {
        let window_rect = w.rect();
        let Some(_) = window_rect.intersection(clip) else {
            return;
        };

        let is_frameless = w.flags.is_enable(WindowFlags::FRAMELESS_WINDOW_HINT);
        let th = w.title_height();

        if th > 0 {
            let title_color = if w.focused { 0x003A_7CA5 } else { 0x0040_4850 };
            fill_rect_clipped(
                fb,
                clip,
                w.x,
                w.y,
                w.w,
                th,
                title_color,
            );

            fill_rect_clipped(
                fb,
                clip,
                w.x,
                w.y + th as i32 - 1,
                w.w,
                1,
                0x0010_1010,
            );

            draw_title_text_clipped(
                fb,
                clip,
                w.x + 6,
                w.y + 4,
                w.title_str(),
            );

            if w.flags.is_enable(WindowFlags::WINDOW_CLOSE_BUTTON_HINT) {
                draw_close_button_clipped(fb, clip, w);
            }
        }

        let (cx, cy, cw, ch) = w.client_rect();
        if ch > 0 {
            if let Some(client_clip) = DirtyRect::new(cx, cy, cw, ch).intersection(clip) {
                let sx = client_clip.x.saturating_sub(cx) as u32;
                let sy = client_clip.y.saturating_sub(cy) as u32;
                blit_surface_rect(
                    fb,
                    client_clip.x.max(0) as u32,
                    client_clip.y.max(0) as u32,
                    sx,
                    sy,
                    client_clip.w,
                    client_clip.h,
                    &w.surface,
                );
            }
        }

        if !is_frameless {
            let wr = window_rect;
            fill_rect_clipped(fb, clip, wr.x, wr.y, wr.w, 1, 0x0000_0000);
            if wr.h > 0 {
                fill_rect_clipped(fb, clip, wr.x, wr.y + wr.h as i32 - 1, wr.w, 1, 0x0000_0000);
            }
            fill_rect_clipped(fb, clip, wr.x, wr.y, 1, wr.h, 0x0000_0000);
            if wr.w > 0 {
                fill_rect_clipped(fb, clip, wr.x + wr.w as i32 - 1, wr.y, 1, wr.h, 0x0000_0000);
            }
        }
    }

    fn draw_window(&self, fb: &mut Framebuffer, w: &Window) {
        self.draw_window_clipped(
            fb,
            w,
            DirtyRect::new(0, 0, self.screen_w, self.screen_h),
        );
    }
}

fn fill_rect_clipped(
    fb: &mut Framebuffer,
    clip: DirtyRect,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    color: u32,
) {
    let Some(r) = DirtyRect::new(x, y, w, h).intersection(clip) else {
        return;
    };
    if r.x < 0 || r.y < 0 {
        // The compositor's dirty region is normally screen-clipped, but keep
        // this helper safe for windows partially outside the screen.
        let screen = DirtyRect::new(0, 0, fb.info.width as u32, fb.info.height as u32);
        let Some(r) = r.intersection(screen) else { return; };
        fb.fill_rect(r.x as u32, r.y as u32, r.w, r.h, color);
        return;
    }
    fb.fill_rect(r.x as u32, r.y as u32, r.w, r.h, color);
}

struct ClippedFramebuffer<'a> {
    fb: &'a mut Framebuffer,
    clip: DirtyRect,
}

impl OriginDimensions for ClippedFramebuffer<'_> {
    fn size(&self) -> Size {
        Size::new(self.fb.info.width as u32, self.fb.info.height as u32)
    }
}

impl DrawTarget for ClippedFramebuffer<'_> {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            let x = point.x;
            let y = point.y;
            if x < self.clip.x
                || x >= self.clip.right()
                || y < self.clip.y
                || y >= self.clip.bottom()
                || x < 0
                || y < 0
                || x >= self.fb.info.width as i32
                || y >= self.fb.info.height as i32
            {
                continue;
            }

            let packed = ((color.r() as u32) << 16) | ((color.g() as u32) << 8) | color.b() as u32;
            self.fb.put_pixel_raw(x as u32, y as u32, packed);
        }
        Ok(())
    }
}

fn draw_title_text_clipped(
    fb: &mut Framebuffer,
    clip: DirtyRect,
    x: i32,
    y: i32,
    text: &str,
) {
    let style = MonoTextStyle::new(&FONT_6X10, Rgb888::new(0xF0, 0xF0, 0xF0));
    let pos = Point::new(x, y);
    let mut target = ClippedFramebuffer { fb, clip };
    without_interrupts(|| {
        let _ = Text::with_baseline(text, pos, style, Baseline::Top).draw(&mut target);
    });
}

fn draw_close_button_clipped(fb: &mut Framebuffer, clip: DirtyRect, w: &Window) {
    let (x1, y1, x2, y2) = close_rect(w);
    if x2 <= x1 || y2 <= y1 {
        return;
    }

    fill_rect_clipped(
        fb,
        clip,
        x1,
        y1,
        (x2 - x1) as u32,
        (y2 - y1) as u32,
        0x00C0_4040,
    );

    let x0 = x1 + 3;
    let y0 = y1 + 3;
    let x1b = x2 - 4;
    let y1b = y2 - 4;

    let mut px = x0;
    let mut py = y0;
    while px <= x1b && py <= y1b {
        put_pixel_clipped(fb, clip, px, py, 0x00F0_F0_F0);
        if px + 1 <= x1b {
            put_pixel_clipped(fb, clip, px + 1, py, 0x00F0_F0_F0);
        }
        px += 1;
        py += 1;
    }

    px = x1b;
    py = y0;
    while px >= x0 && py <= y1b {
        put_pixel_clipped(fb, clip, px, py, 0x00F0_F0_F0);
        if px > x0 {
            put_pixel_clipped(fb, clip, px - 1, py, 0x00F0_F0_F0);
        }
        if px == i32::MIN {
            break;
        }
        px -= 1;
        py += 1;
    }
}

#[inline]
fn put_pixel_clipped(fb: &mut Framebuffer, clip: DirtyRect, x: i32, y: i32, color: u32) {
    if x >= clip.x
        && x < clip.right()
        && y >= clip.y
        && y < clip.bottom()
        && x >= 0
        && y >= 0
        && x < fb.info.width as i32
        && y < fb.info.height as i32
    {
        fb.put_pixel_raw(x as u32, y as u32, color);
    }
}

fn blit_surface_rect(fb: &mut Framebuffer, dx: u32, dy: u32, sx: u32, sy: u32, w: u32, h: u32, surf: &Surface) {
    if sx >= surf.width || sy >= surf.height || dx >= fb.info.width as u32 || dy >= fb.info.height as u32 { return; }
    let w = w.min(surf.width - sx).min(fb.info.width as u32 - dx);
    let h = h.min(surf.height - sy).min(fb.info.height as u32 - dy);
    let dst_pitch = fb.info.pitch as usize; let src_pitch = surf.pitch as usize; let row = w as usize * 4;
    for y in 0..h as usize {
        let src = (sy as usize + y) * src_pitch + sx as usize * 4;
        let dst = (dy as usize + y) * dst_pitch + dx as usize * 4;
        unsafe { core::ptr::copy_nonoverlapping(surf.pixels.as_ptr().add(src), (fb.virt_base as *mut u8).add(dst), row); }
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
    if !w.has_title_bar() {
        return false;
    }
    x >= w.x && x < w.x + w.w as i32 && y >= w.y && y < w.y + TITLE_H as i32
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
        wm.dirty = None;
        wm.compose();
    }

    WM_READY.store(true, Ordering::SeqCst);
    // Kernel logs stay on E9 — do not use println to FB.
    debugln!("[wm] ready {}x{}", sw, sh);
}

pub fn create_window(x: i32, y: i32, client_w: u32, client_h: u32, title: &str, flags: WindowFlags, owner_slot: i8) -> Option<u32> {
    // Минимальный размер клиентской области
    if client_w < 40 || client_h < 40 {
        return None;
    }

    let mut title_buf = [0u8; 32];
    let tbytes = title.as_bytes();
    let n = tbytes.len().min(31);
    title_buf[..n].copy_from_slice(&tbytes[..n]);

    // Вычисляем итоговые размеры окна на основе флагов
    let has_title = !flags.is_enable(WindowFlags::FRAMELESS_WINDOW_HINT) &&
        flags.is_enable(WindowFlags::WINDOW_TITLE_HINT);

    let total_w = client_w;
    let total_h = client_h + if has_title { TITLE_H } else { 0 };

    with_lfb(|| {
        let mut wm = WM.lock();
        let slot = wm.slot_free()?;

        // Surface создается строго с клиентскими размерами
        let surface = Surface::new(client_w, client_h)?;

        let id = wm.next_id;
        wm.next_id = wm.next_id.wrapping_add(1).max(1);
        let z = wm.next_z;
        wm.next_z = wm.next_z.wrapping_add(1);

        let old_focus_rect = wm.windows.iter().flatten()
            .find(|win| win.focused)
            .map(|win| win.rect());

        for win in wm.windows.iter_mut().flatten() {
            win.focused = false;
        }

        if let Some(rect) = old_focus_rect {
            wm.mark_dirty(rect);
        }

        wm.windows[slot] = Some(Window {
            id,
            x,
            y,
            w: total_w,       // Сохраняем ОБЩУЮ ширину
            h: total_h,       // Сохраняем ОБЩУЮ высоту
            z,
            focused: true,
            visible: true,
            title: title_buf,
            flags,
            surface,
            owner_slot,
            events: EventQueue::new(),
        });
        wm.mark_window_dirty(id);
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
                let rect = slot.as_ref().map(|w| w.rect()).unwrap_or(DirtyRect::new(0, 0, 0, 0));
                *slot = None;
                wm.mark_dirty(rect);
                wm.compose_dirty();
                return true;
            }
        }
        false
    })
}

/// Drop every window owned by `owner_slot` (process exit / kill).
pub fn destroy_windows_of(owner_slot: i8) {
    if owner_slot < 0 {
        return;
    }

    with_lfb(|| {
        let mut wm = WM.lock();
        let mut any = false;
        let mut dirty_rects: [Option<DirtyRect>; MAX_WINDOWS] = [None; MAX_WINDOWS];
        let mut n = 0;

        for slot in wm.windows.iter_mut() {
            if slot.as_ref().map(|w| w.owner_slot) == Some(owner_slot) {
                if let Some(w) = slot.as_ref() {
                    dirty_rects[n] = Some(w.rect());
                    n += 1;
                }
                *slot = None;
                any = true;
            }
        }

        for rect in dirty_rects.iter().take(n).flatten() {
            wm.mark_dirty(*rect);
        }

        if any {
            wm.drag = None;
            wm.compose_dirty();
        }
    });
}

pub fn move_window(id: u32, x: i32, y: i32) -> bool {
    with_lfb(|| {
        let mut wm = WM.lock();
        let id = id as u8;
        let old = wm.find(id).map(|w| w.rect());
        if let Some(w) = wm.find_mut(id) {
            w.x = x;
            w.y = y;
            w.events = EventQueue::new();
            let new_rect = w.rect();
            if let Some(old) = old {
                wm.mark_dirty(old);
            }
            wm.mark_dirty(new_rect);
            wm.compose_dirty();
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
    let mut partial = None;
    let mut wm = WM.lock();
    if let Some(w) = wm.find_mut(id as u8) {
        if !user_pixels.is_null() && len > 0 {
            without_interrupts(|| {
                if len == usize::MAX {
                    let desc = unsafe { core::ptr::read_unaligned(user_pixels as *const WmFlipRect) };
                    w.surface.copy_rect_from_user(desc.pixels, desc.pitch, desc.x, desc.y, desc.w, desc.h);
                    partial = Some((desc.x, desc.y, desc.w, desc.h));
                } else { w.surface.copy_from_user(user_pixels, len); }
            });
        }
        drop(wm);
        with_lfb(|| {
            if let Some((x, y, w, h)) = partial {
                WM.lock().compose_client_rect(id as u8, x, y, w, h);
            } else {
                let mut wm = WM.lock();
                wm.mark_window_dirty(id as u8);
                wm.compose_dirty();
            }
        });
        true
    } else { false }
}

pub fn focus_window(id: u32) -> bool {
    with_lfb(|| {
        let mut wm = WM.lock();
        let id = id as u8;
        if wm.find(id).is_none() {
            return false;
        }

        let mut dirty = None;
        for w in wm.windows.iter().flatten() {
            if w.focused && w.id != id {
                dirty = Some(w.rect());
                break;
            }
        }

        let new_z = wm.next_z;
        wm.next_z = wm.next_z.wrapping_add(1);
        for w in wm.windows.iter_mut().flatten() {
            let is_target = w.id == id;
            w.focused = is_target;
            if is_target {
                w.z = new_z;
                dirty = Some(match dirty {
                    Some(old) => old.union(w.rect()),
                    None => w.rect(),
                });
            }
        }

        if let Some(rect) = dirty {
            wm.mark_dirty(rect);
        }
        wm.compose_dirty();
        true
    })
}

pub fn screen_size() -> (u32, u32) {
    let wm = WM.lock();
    (wm.screen_w, wm.screen_h)
}

fn hit_resize(w: &Window, x: i32, y: i32) -> bool {
    let gx = w.x + w.w as i32 - RESIZE_SZ;
    let gy = w.y + w.h as i32 - RESIZE_SZ;
    x >= gx && x < w.x + w.w as i32 && y >= gy && y < w.y + w.h as i32
}

fn apply_resize(w: &mut Window, new_w: u32, new_h: u32) {
    let new_w = new_w.clamp(80, 1600);
    // Минимальная высота зависит от наличия заголовка
    let min_h = w.title_height() + 40;
    let new_h = new_h.clamp(min_h, 1200);

    w.w = new_w;
    w.h = new_h;

    let cw = new_w;
    let ch = new_h.saturating_sub(w.title_height());

    if w.surface.width != cw || w.surface.height != ch {
        if let Some(mut surf) = Surface::new(cw, ch) {
            let copy_w = w.surface.width.min(cw) as usize;
            let copy_h = w.surface.height.min(ch) as usize;
            let src_pitch = w.surface.pitch as usize;
            let dst_pitch = surf.pitch as usize;
            let row = copy_w.saturating_mul(4);
            for y in 0..copy_h {
                let s = y.saturating_mul(src_pitch);
                let d = y.saturating_mul(dst_pitch);
                if s + row <= w.surface.pixels.len() && d + row <= surf.pixels.len() {
                    surf.pixels[d..d + row]
                        .copy_from_slice(&w.surface.pixels[s..s + row]);
                }
            }
            w.surface = surf;
        }
    }
    w.events.push(WmEvent {
        kind: EV_RESIZE,
        a: w.surface.width as i32,
        b: w.surface.height as i32,
        c: 0,
        d: 0,
    });
}

/// Left button down: close / start title drag / resize / focus.
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

        // Close button: tear the window down in the WM immediately.
        // Also SIGTERM the owner — if it is hung / ignores the event, the
        // frame is already gone. Default signal action reaps the task.
        if hit_close(w, x, y) {
            let mut owner = -1i8;
            let old_rect = wm.find(target).map(|win| win.rect());
            for slot in wm.windows.iter_mut() {
                if slot.as_ref().map(|ww| ww.id) == Some(target) {
                    owner = slot.as_ref().map(|ww| ww.owner_slot).unwrap_or(-1);
                    *slot = None;
                    break;
                }
            }
            if let Some(rect) = old_rect {
                wm.mark_dirty(rect);
            }
            wm.drag = None;
            wm.compose_dirty();
            drop(wm);
            if owner > 0 {
                // Not a queued signal — mark zombie immediately so ignore/hang
                // cannot keep the task runnable.
                let _ = crate::signal::force_kill(owner, crate::signal::SIGKILL);
            }
            return;
        }

        if hit_resize(w, x, y) {
            let orig_w = w.w;
            let orig_h = w.h;
            let old_rect = wm.find(target).map(|w| w.rect());
            let old_focus_rect = wm.windows.iter().flatten()
                .find(|w| w.focused && w.id != target)
                .map(|w| w.rect());

            let new_z = wm.next_z;
            wm.next_z = wm.next_z.wrapping_add(1);
            let mut new_rect = None;
            for win in wm.windows.iter_mut().flatten() {
                let is = win.id == target;
                win.focused = is;
                if is {
                    win.z = new_z;
                    new_rect = Some(win.rect());
                }
            }
            if let Some(r) = old_rect {
                wm.mark_dirty(r);
            }
            if let Some(r) = old_focus_rect {
                wm.mark_dirty(r);
            }
            if let Some(r) = new_rect {
                wm.mark_dirty(r);
            }
            wm.drag = Some(DragState {
                id: target,
                grab_dx: x,
                grab_dy: y,
                kind: 1,
                orig_w,
                orig_h,
            });
            wm.compose_dirty();
            return;
        }

        // Focus + raise + optional client click / title drag
        let old_target_rect = w.rect();
        let old_focus_rect = wm.windows.iter().flatten()
            .find(|win| win.focused && win.id != target)
            .map(|win| win.rect());

        let target_was_focused = w.focused;

        let in_title = hit_title(w, x, y);
        let cx = x - w.x;
        let cy = y - (w.y + w.title_height() as i32);
        let client_h = w.h.saturating_sub(w.title_height()) as i32;
        let in_client =
            !in_title &&
                cx >= 0 &&
                cy >= 0 &&
                cx < w.w as i32 &&
                cy < client_h;

        let mut grab_dx = 0;
        let mut grab_dy = 0;
        let mut start_drag = false;

        // Only change focus/Z-order when clicking another window.
        // Clicking the already focused window must not trigger a redraw.
        if !target_was_focused {
            let new_z = wm.next_z;
            wm.next_z = wm.next_z.wrapping_add(1);

            for win in wm.windows.iter_mut().flatten() {
                let is = win.id == target;
                let was = win.focused;

                win.focused = is;

                if is {
                    win.z = new_z;

                    if !was {
                        win.events.push(WmEvent {
                            kind: EV_FOCUS_IN,
                            a: 0,
                            b: 0,
                            c: 0,
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

            wm.mark_dirty(old_target_rect);

            if let Some(rect) = old_focus_rect {
                wm.mark_dirty(rect);
            }

            if let Some(rect) = wm.find(target).map(|win| win.rect()) {
                wm.mark_dirty(rect);
            }
        }

        // These events are needed regardless of whether focus changed.
        if let Some(win) = wm.find_mut(target) {
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
        }

        if start_drag {
            wm.drag = Some(DragState {
                id: target,
                grab_dx,
                grab_dy,
                kind: 0,
                orig_w: 0,
                orig_h: 0,
            });
        } else {
            wm.drag = None;
        }

        // Redraw only when focus/Z-order actually changed.
        if !target_was_focused {
            wm.compose_dirty();
        }

        return;
    }
    wm.drag = None;
}

/// Pointer move: title drag or client MouseMove.
pub fn on_mouse_move(x: i32, y: i32) {
    let Some(mut wm) = WM.try_lock() else {
        return;
    };

    if let Some(drag) = wm.drag {
        let id = drag.id;
        if drag.kind == 1 {
            let dw = x - drag.grab_dx;
            let dh = y - drag.grab_dy;
            let nw = (drag.orig_w as i32 + dw).clamp(80, wm.screen_w as i32) as u32;
            let nh = (drag.orig_h as i32 + dh).clamp((TITLE_H as i32) + 40, wm.screen_h as i32) as u32;
            let old_rect = wm.find(id).map(|w| w.rect());
            if let Some(w) = wm.find_mut(id) {
                if w.w != nw || w.h != nh {
                    w.w = nw;
                    w.h = nh;
                    let new_rect = w.rect();
                    if let Some(old) = old_rect {
                        wm.mark_dirty(old);
                    }
                    wm.mark_dirty(new_rect);
                    wm.compose_dirty();
                }
            } else {
                wm.drag = None;
            }
            return;
        }
        let nx = x - drag.grab_dx;
        let ny = y - drag.grab_dy;
        let max_x = wm.screen_w.saturating_sub(40) as i32;
        let max_y = wm.screen_h.saturating_sub(TITLE_H) as i32;
        let nx = nx.clamp(-((wm.screen_w as i32) / 2), max_x);
        let ny = ny.clamp(0, max_y);

        let old_rect = wm.find(id).map(|w| w.rect());
        if let Some(w) = wm.find_mut(id) {
            if w.x != nx || w.y != ny {
                w.x = nx;
                w.y = ny;
                w.events = EventQueue::new(); // очистить stale events
                let new_rect = w.rect();
                if let Some(old) = old_rect {
                    // Do not use the generic clear-first compositor while
                    // dragging. It would expose the background for a frame.
                    wm.compose_move(id, old, new_rect);
                } else {
                    wm.mark_dirty(new_rect);
                    wm.compose_dirty();
                }
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
        let th = w.title_height() as i32;
        let cx = x - w.x;
        let cy = y - (w.y + th);
        let client_h = w.h.saturating_sub(th as u32) as i32;
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
    let finishing = wm.drag.take();
    if let Some(drag) = finishing {
        if drag.kind == 1 {
            let old_rect = wm.find(drag.id).map(|w| w.rect());
            if let Some(w) = wm.find_mut(drag.id) {
                let nw = w.w;
                let nh = w.h;
                apply_resize(w, nw, nh);
                let new_rect = w.rect();
                if let Some(old) = old_rect {
                    wm.mark_dirty(old);
                }
                wm.mark_dirty(new_rect);
            }
            wm.compose_dirty();
        }
    }

    let ids = wm.sorted_ids();
    for id in ids.iter().rev().flatten() {
        let Some(w) = wm.find_mut(*id) else {
            continue;
        };
        if !w.visible {
            continue;
        }
        let th = w.title_height() as i32;
        let cx = x - w.x;
        let cy = y - (w.y + th);
        let client_h = w.h.saturating_sub(th as u32) as i32;
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

/// Заполняет пользовательский буфер списком окон. Возвращает количество записанных окон.
pub fn window_list(out: *mut WindowListItem, max: usize) -> usize {
    if out.is_null() || max == 0 {
        return 0;
    }

    let wm = WM.lock();
    let mut count = 0;

    for w in wm.windows.iter().flatten() {
        if count >= max {
            break; // Буфер заполнен
        }

        let item = WindowListItem {
            id: w.id,
            x: w.x,
            y: w.y,
            w: w.w,
            h: w.h,
            focused: w.focused as u8,
            visible: w.visible as u8,
            owner_slot: w.owner_slot,
            title: w.title,
        };

        unsafe {
            *out.add(count) = item;
        }
        count += 1;
    }

    count
}

/// Legacy: treat as down (focus).
pub fn on_mouse_click(x: i32, y: i32) {
    on_mouse_down(x, y);
}
