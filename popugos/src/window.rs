use std::{fmt, string::String, vec::Vec};

use embedded_graphics::{
    draw_target::DrawTarget,
    pixelcolor::{Rgb888, RgbColor},
    prelude::*,
    primitives::Rectangle,
    Pixel,
};

use crate::sys;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowFlags(u8);

impl WindowFlags {
    pub const TITLE: u8 = 1 << 0;
    pub const CLOSE_BUTTON: u8 = 1 << 1;
    pub const FULLSCREEN_BUTTON: u8 = 1 << 2;
    pub const FRAMELESS: u8 = 1 << 3;

    pub const fn new(bits: u8) -> Self { Self(bits) }
    pub const fn bits(self) -> u8 { self.0 }
    pub const fn contains(self, flag: u8) -> bool { self.0 & flag != 0 }

    pub fn set(&mut self, flag: u8, enabled: bool) {
        if enabled { self.0 |= flag; } else { self.0 &= !flag; }
    }
}

impl Default for WindowFlags {
    fn default() -> Self {
        Self(Self::TITLE | Self::CLOSE_BUTTON)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub client_width: u32,
    pub client_height: u32,
    pub pitch: u32,
    pub focused: bool,
}

impl From<sys::RawWindowInfo> for WindowInfo {
    fn from(value: sys::RawWindowInfo) -> Self {
        Self {
            id: value.id,
            x: value.x,
            y: value.y,
            width: value.w,
            height: value.h,
            client_width: value.client_w,
            client_height: value.client_h,
            pitch: value.pitch,
            focused: value.focused != 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub scancode: u8,
    pub ch: u8,
    pub modifiers: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other(u8),
}

impl From<u8> for MouseButton {
    fn from(value: u8) -> Self {
        match value {
            0 | 1 => Self::Left,
            2 => Self::Right,
            3 => Self::Middle,
            value => Self::Other(value),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseEvent {
    pub x: i32,
    pub y: i32,
    pub button: MouseButton,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    MouseMove { x: i32, y: i32 },
    MouseLeave,
    MouseWheel { x: i32, y: i32, delta: i32 },
    MouseDown(MouseEvent),
    MouseUp(MouseEvent),
    KeyDown(KeyEvent),
    KeyUp(KeyEvent),
    Close,
    Focused(bool),
    Resize { width: u32, height: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowError {
    UnsupportedPlatform,
    CreateFailed,
    InfoFailed,
    PresentFailed,
}

impl fmt::Display for WindowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => f.write_str("PopugOS window API is only available on PopugOS"),
            Self::CreateFailed => f.write_str("window creation failed"),
            Self::InfoFailed => f.write_str("failed to query window information"),
            Self::PresentFailed => f.write_str("failed to present window buffer"),
        }
    }
}

impl std::error::Error for WindowError {}

pub struct WindowBuilder {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    title: String,
    flags: WindowFlags,
}

impl WindowBuilder {
    pub fn new() -> Self {
        Self {
            x: 40,
            y: 40,
            width: 640,
            height: 480,
            title: String::from("PopugOS"),
            flags: WindowFlags::default(),
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self { self.title = title.into(); self }
    pub fn position(mut self, x: i32, y: i32) -> Self { self.x = x; self.y = y; self }
    pub fn size(mut self, width: u32, height: u32) -> Self { self.width = width; self.height = height; self }
    pub fn flags(mut self, flags: WindowFlags) -> Self { self.flags = flags; self }

    pub fn title_bar(mut self, enabled: bool) -> Self {
        self.flags.set(WindowFlags::TITLE, enabled);
        self
    }

    pub fn close_button(mut self, enabled: bool) -> Self {
        self.flags.set(WindowFlags::CLOSE_BUTTON, enabled);
        self
    }

    pub fn frameless(mut self, enabled: bool) -> Self {
        self.flags.set(WindowFlags::FRAMELESS, enabled);
        self
    }

    pub fn build(self) -> Result<Window, WindowError> {
        Window::create(self.x, self.y, self.width, self.height, &self.title, self.flags)
    }
}

impl Default for WindowBuilder {
    fn default() -> Self { Self::new() }
}

pub struct Window {
    id: u32,
    info: WindowInfo,
    buffer: Vec<u8>,
    clip: Option<Rectangle>,
    alive: bool,
}

impl Window {
    pub fn builder() -> WindowBuilder { WindowBuilder::new() }

    fn create(
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        title: &str,
        flags: WindowFlags,
    ) -> Result<Self, WindowError> {
        if !cfg!(target_os = "popugos") {
            return Err(WindowError::UnsupportedPlatform);
        }

        let mut title_buf = [0u8; 32];
        let bytes = title.as_bytes();
        let n = bytes.len().min(31);
        title_buf[..n].copy_from_slice(&bytes[..n]);
        let flags_byte = flags.bits();
        let args = sys::WmCreateArgs {
            x,
            y,
            w: width,
            h: height,
            title: title_buf.as_ptr(),
            flags: &flags_byte,
        };
        let raw_id = unsafe { sys::wm_create(&args) };
        if raw_id == usize::MAX {
            return Err(WindowError::CreateFailed);
        }
        let id = raw_id as u32;

        let mut raw = sys::RawWindowInfo::default();
        if unsafe { sys::wm_info(id, &mut raw) } != 0 || raw.client_w == 0 || raw.client_h == 0 {
            let _ = unsafe { sys::wm_destroy(id) };
            return Err(WindowError::InfoFailed);
        }
        let info = WindowInfo::from(raw);
        // Allocate the (potentially multi-megabyte) backing store lazily in
        // `buffer_mut`.  On i686 PopugOS, keeping the large `Vec` allocation
        // inside this function made optimized code keep the Result sret
        // pointer across the mmap allocator call; the custom ABI currently
        // does not preserve that value reliably for large mappings.
        Ok(Self { id, info, buffer: Vec::new(), clip: None, alive: true })
    }

    pub fn id(&self) -> u32 { self.id }
    pub fn info_cached(&self) -> WindowInfo { self.info }
    pub fn client_width(&self) -> u32 { self.info.client_width }
    pub fn client_height(&self) -> u32 { self.info.client_height }
    pub fn pitch(&self) -> usize { self.info.pitch as usize }
    pub fn buffer(&self) -> &[u8] { &self.buffer }
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        self.ensure_buffer();
        &mut self.buffer
    }

    fn ensure_buffer(&mut self) {
        if self.buffer.is_empty() {
            initialize_buffer(&mut self.buffer, self.info.pitch, self.info.client_height);
        }
    }

    pub fn info(&self) -> Result<WindowInfo, WindowError> {
        let mut raw = sys::RawWindowInfo::default();
        if unsafe { sys::wm_info(self.id, &mut raw) } == 0 {
            Ok(raw.into())
        } else {
            Err(WindowError::InfoFailed)
        }
    }

    pub fn refresh_info(&mut self) -> Result<WindowInfo, WindowError> {
        let info = self.info()?;
        self.info = info;
        Ok(info)
    }

    pub fn move_to(&mut self, x: i32, y: i32) -> bool {
        if unsafe { sys::wm_move(self.id, x, y) } == 0 {
            self.info.x = x;
            self.info.y = y;
            true
        } else {
            false
        }
    }

    pub fn focus(&self) -> bool { unsafe { sys::wm_focus(self.id) == 0 } }

    pub fn set_clip(&mut self, clip: Option<Rectangle>) { self.clip = clip; }

    pub fn intersect_clip(&mut self, clip: Rectangle) {
        self.clip = Some(match self.clip {
            Some(current) => current.intersection(&clip),
            None => clip,
        });
    }

    pub fn present(&self) -> Result<(), WindowError> {
        if unsafe { sys::wm_flip(self.id, self.buffer.as_ptr(), self.buffer.len()) } == 0 {
            Ok(())
        } else {
            Err(WindowError::PresentFailed)
        }
    }

    pub fn present_rect(&self, rect: Rectangle) -> Result<(), WindowError> {
        let bounds = Rectangle::new(Point::zero(), Size::new(self.info.client_width, self.info.client_height));
        let rect = rect.intersection(&bounds);
        if rect.size.width == 0 || rect.size.height == 0 {
            return Ok(());
        }
        let desc = sys::WmFlipRect {
            x: rect.top_left.x as u32,
            y: rect.top_left.y as u32,
            w: rect.size.width,
            h: rect.size.height,
            pitch: self.info.pitch,
            pixels: self.buffer.as_ptr(),
        };
        if unsafe { sys::wm_flip(self.id, (&desc as *const sys::WmFlipRect).cast(), usize::MAX) } == 0 {
            Ok(())
        } else {
            Err(WindowError::PresentFailed)
        }
    }

    /// Non-blocking event poll. Tokio integration lives in `popui`, not here,
    /// so this crate stays a thin PopugOS-specific extension to `std`.
    pub fn poll_event(&mut self) -> Option<Event> {
        loop {
            let mut raw = sys::RawWmEvent::default();
            if unsafe { sys::wm_poll(self.id, &mut raw, 1) } == 0 {
                return None;
            }
            let event = match raw.kind {
                sys::EV_NONE => continue,
                sys::EV_MOUSE_MOVE => Event::MouseMove { x: raw.a, y: raw.b },
                sys::EV_MOUSE_LEAVE => Event::MouseLeave,
                sys::EV_MOUSE_WHEEL => Event::MouseWheel { x: raw.a, y: raw.b, delta: raw.c },
                sys::EV_MOUSE_DOWN => Event::MouseDown(MouseEvent {
                    x: raw.a,
                    y: raw.b,
                    button: (raw.c as u8).into(),
                }),
                sys::EV_MOUSE_UP => Event::MouseUp(MouseEvent {
                    x: raw.a,
                    y: raw.b,
                    button: (raw.c as u8).into(),
                }),
                sys::EV_KEY_DOWN => Event::KeyDown(KeyEvent {
                    scancode: raw.a as u8,
                    ch: raw.b as u8,
                    modifiers: raw.c as u8,
                }),
                sys::EV_KEY_UP => Event::KeyUp(KeyEvent {
                    scancode: raw.a as u8,
                    ch: raw.b as u8,
                    modifiers: raw.c as u8,
                }),
                sys::EV_CLOSE => Event::Close,
                sys::EV_FOCUS_IN => Event::Focused(true),
                sys::EV_FOCUS_OUT => Event::Focused(false),
                sys::EV_RESIZE => {
                    let width = raw.a.max(1) as u32;
                    let height = raw.b.max(1) as u32;
                    self.apply_resize(width, height);
                    Event::Resize { width, height }
                }
                _ => continue,
            };
            return Some(event);
        }
    }

    fn apply_resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        let old_w = self.info.client_width.max(1);
        let old_h = self.info.client_height.max(1);
        let old_pitch = self.info.pitch as usize;
        let old = std::mem::take(&mut self.buffer);

        let _ = self.refresh_info();
        self.info.client_width = width;
        self.info.client_height = height;
        self.info.pitch = width.saturating_mul(4);
        let mut buffer = Vec::new();
        initialize_buffer(&mut buffer, self.info.pitch, height);
        let copy_w = old_w.min(width) as usize;
        let copy_h = old_h.min(height) as usize;
        let row_bytes = copy_w.saturating_mul(4);
        let new_pitch = self.info.pitch as usize;
        for y in 0..copy_h {
            let src = y.saturating_mul(old_pitch);
            let dst = y.saturating_mul(new_pitch);
            if src + row_bytes <= old.len() && dst + row_bytes <= buffer.len() {
                buffer[dst..dst + row_bytes].copy_from_slice(&old[src..src + row_bytes]);
            }
        }
        self.buffer = buffer;
        let _ = self.present();
    }

    pub fn close(mut self) -> bool { self.close_internal() }

    fn close_internal(&mut self) -> bool {
        if !self.alive { return true; }
        self.alive = false;
        unsafe { sys::wm_destroy(self.id) == 0 }
    }
}

impl Drop for Window {
    fn drop(&mut self) { let _ = self.close_internal(); }
}

pub fn screen_size() -> (u32, u32) {
    let mut out = [0u32; 2];
    if unsafe { sys::wm_screen_size(out.as_mut_ptr()) } == 0 {
        (out[0].max(1), out[1].max(1))
    } else {
        (800, 600)
    }
}

#[inline(never)]
fn initialize_buffer(buffer: &mut Vec<u8>, pitch: u32, height: u32) {
    let len = (pitch as usize).saturating_mul(height as usize);
    buffer.clear();
    buffer.reserve_exact(len);
    buffer.resize(len, 0);
    for pixel in buffer.chunks_exact_mut(4) {
        pixel[0] = 0x20;
        pixel[1] = 0x18;
        pixel[2] = 0x10;
        pixel[3] = 0;
    }
}

impl OriginDimensions for Window {
    fn size(&self) -> Size { Size::new(self.info.client_width, self.info.client_height) }
}

impl DrawTarget for Window {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        self.ensure_buffer();
        let width = self.info.client_width as i32;
        let height = self.info.client_height as i32;
        let pitch = self.pitch();
        let len = self.buffer.len();
        for Pixel(point, color) in pixels {
            if point.x < 0 || point.y < 0 || point.x >= width || point.y >= height { continue; }
            if self.clip.map(|clip| !clip.contains(point)).unwrap_or(false) { continue; }
            let offset = point.y as usize * pitch + point.x as usize * 4;
            if offset + 3 >= len { continue; }
            self.buffer[offset] = color.b();
            self.buffer[offset + 1] = color.g();
            self.buffer[offset + 2] = color.r();
            self.buffer[offset + 3] = 0;
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        self.ensure_buffer();
        let bounds = Rectangle::new(Point::zero(), Size::new(self.info.client_width, self.info.client_height));
        let mut area = area.intersection(&bounds);
        if let Some(clip) = self.clip { area = area.intersection(&clip); }
        if area.size.width == 0 || area.size.height == 0 { return Ok(()); }

        let pitch = self.pitch();
        let x0 = area.top_left.x as usize;
        let y0 = area.top_left.y as usize;
        let x1 = x0 + area.size.width as usize;
        let y1 = y0 + area.size.height as usize;
        for y in y0..y1 {
            let row = y * pitch;
            for x in x0..x1 {
                let offset = row + x * 4;
                if offset + 3 >= self.buffer.len() { break; }
                self.buffer[offset] = color.b();
                self.buffer[offset + 1] = color.g();
                self.buffer[offset + 2] = color.r();
                self.buffer[offset + 3] = 0;
            }
        }
        Ok(())
    }
}
