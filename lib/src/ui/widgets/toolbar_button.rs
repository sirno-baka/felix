use super::{font, IconImage};
use crate::ui::{Constraints, EventResult, Rect, UiEvent, Widget};
use alloc::string::String;
use embedded_graphics::{
    mono_font::MonoTextStyle,
    pixelcolor::Rgb888,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
    Pixel,
};
use taffy::geometry::Size as TSize;

const FONT_W: i32 = 9;
const FONT_H: i32 = 18;
const ICON_SIZE: i32 = 20;
const GAP: i32 = 5;

const BG: Rgb888 = Rgb888::new(0xEE, 0xEE, 0xE8);
const BG_HOT: Rgb888 = Rgb888::new(0xD9, 0xE8, 0xFA);
const BG_DOWN: Rgb888 = Rgb888::new(0xC3, 0xD8, 0xF2);
const BORDER_HOT: Rgb888 = Rgb888::new(0x7A, 0xA7, 0xD9);
const TEXT: Rgb888 = Rgb888::new(0x18, 0x18, 0x18);
const TEXT_DISABLED: Rgb888 = Rgb888::new(0x88, 0x88, 0x88);

pub struct ToolbarButton {
    label: String,
    icon: Option<IconImage>,
    rect: Rect,
    hot: bool,
    down: bool,
    enabled: bool,
    dirty: bool,
}

impl ToolbarButton {
    pub fn new(label: &str) -> Self {
        Self {
            label: String::from(label),
            icon: None,
            rect: Rect::default(),
            hot: false,
            down: false,
            enabled: true,
            dirty: true,
        }
    }

    pub fn with_icon(label: &str, icon: IconImage) -> Self {
        let mut out = Self::new(label);
        out.icon = Some(icon);
        out
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn set_label(&mut self, label: &str) {
        if self.label != label {
            self.label = String::from(label);
            self.dirty = true;
        }
    }

    pub fn icon(&self) -> Option<&IconImage> {
        self.icon.as_ref()
    }

    pub fn set_icon(&mut self, icon: IconImage) {
        self.icon = Some(icon);
        self.dirty = true;
    }

    pub fn clear_icon(&mut self) {
        if self.icon.take().is_some() {
            self.dirty = true;
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            if !enabled {
                self.down = false;
                self.hot = false;
            }
            self.dirty = true;
        }
    }

    fn draw_icon(&self, win: &mut crate::wm::Window, icon: &IconImage, x: i32, y: i32) {
        if icon.width == 0 || icon.height == 0 {
            return;
        }

        let draw_w = (icon.width as i32).min(ICON_SIZE);
        let draw_h = (icon.height as i32).min(ICON_SIZE);
        let sx0 = ((icon.width as i32 - draw_w) / 2).max(0) as usize;
        let sy0 = ((icon.height as i32 - draw_h) / 2).max(0) as usize;
        let dx0 = x + (ICON_SIZE - draw_w) / 2;
        let dy0 = y + (ICON_SIZE - draw_h) / 2;
        let src_w = icon.width as usize;
        let pixels = icon.pixels();
        let mask = icon.mask();

        let iter = (0..draw_h as usize).flat_map(|dy| {
            (0..draw_w as usize).filter_map(move |dx| {
                let sx = sx0 + dx;
                let sy = sy0 + dy;
                let index = sy.checked_mul(src_w)?.checked_add(sx)?;
                if !mask_opaque(mask, index) {
                    return None;
                }
                let raw = *pixels.get(index)?;
                let mut c = rgb565_to_rgb888(raw);
                if !self.enabled {
                    let gray = ((c.r() as u16 + c.g() as u16 + c.b() as u16) / 3) as u8;
                    c = Rgb888::new(gray, gray, gray);
                }
                Some(Pixel(Point::new(dx0 + dx as i32, dy0 + dy as i32), c))
            })
        });
        let _ = win.draw_iter(iter);
    }
}

impl Widget for ToolbarButton {
    fn measure(&self, c: Constraints) -> TSize<f32> {
        let icon_w = if self.icon.is_some() { ICON_SIZE + if self.label.is_empty() { 0 } else { GAP } } else { 0 };
        let text_w = self.label.chars().count() as i32 * FONT_W;
        c.clamp(TSize {
            width: (icon_w + text_w + 10) as f32,
            height: 30.0,
        })
    }

    fn set_rect(&mut self, rect: Rect) {
        if self.rect != rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    fn rect(&self) -> Rect {
        self.rect
    }

    fn draw(&self, win: &mut crate::wm::Window) {
        let r = self.rect;
        if r.w == 0 || r.h == 0 {
            return;
        }

        let bg = if self.down && self.hot && self.enabled {
            BG_DOWN
        } else if self.hot && self.enabled {
            BG_HOT
        } else {
            BG
        };

        let rect = Rectangle::new(Point::new(r.x, r.y), Size::new(r.w, r.h));
        let _ = rect.into_styled(PrimitiveStyle::with_fill(bg)).draw(win);
        if (self.hot || self.down) && self.enabled {
            let _ = rect
                .into_styled(PrimitiveStyle::with_stroke(BORDER_HOT, 1))
                .draw(win);
        }

        let mut x = r.x + 5;
        if let Some(icon) = self.icon.as_ref() {
            let iy = r.y + (r.h as i32 - ICON_SIZE) / 2;
            self.draw_icon(win, icon, x, iy);
            x += ICON_SIZE;
            if !self.label.is_empty() {
                x += GAP;
            }
        }

        if !self.label.is_empty() {
            let style = MonoTextStyle::new(font(), if self.enabled { TEXT } else { TEXT_DISABLED });
            let y = r.y + (r.h as i32 - FONT_H) / 2;
            let _ = Text::with_baseline(
                self.label.as_str(),
                Point::new(x, y.max(r.y + 2)),
                style,
                Baseline::Top,
            )
            .draw(win);
        }
    }

    fn event(&mut self, ev: &UiEvent, _focused: bool) -> EventResult {
        if !self.enabled {
            return EventResult::Ignored;
        }

        match *ev {
            UiEvent::Down { x, y } if self.rect.contains(x, y) => {
                self.down = true;
                self.hot = true;
                self.dirty = true;
                EventResult::Consumed
            }
            UiEvent::Up { x, y } if self.down => {
                let click = self.rect.contains(x, y);
                self.down = false;
                self.hot = click;
                self.dirty = true;
                if click { EventResult::Clicked } else { EventResult::Consumed }
            }
            UiEvent::Move { x, y } => {
                let hot = self.rect.contains(x, y);
                if hot != self.hot {
                    self.hot = hot;
                    self.dirty = true;
                    EventResult::Consumed
                } else {
                    EventResult::Ignored
                }
            }
            _ => EventResult::Ignored,
        }
    }

    fn focusable(&self) -> bool {
        true
    }

    fn dirty(&self) -> bool {
        self.dirty
    }

    fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

fn mask_opaque(mask: Option<&[u8]>, index: usize) -> bool {
    let Some(mask) = mask else { return true; };
    let byte = index >> 3;
    if byte >= mask.len() {
        return false;
    }
    let bit = 7 - (index & 7);
    (mask[byte] & (1u8 << bit)) != 0
}

fn rgb565_to_rgb888(raw: u16) -> Rgb888 {
    let r5 = ((raw >> 11) & 0x1f) as u8;
    let g6 = ((raw >> 5) & 0x3f) as u8;
    let b5 = (raw & 0x1f) as u8;
    Rgb888::new(
        (r5 << 3) | (r5 >> 2),
        (g6 << 2) | (g6 >> 4),
        (b5 << 3) | (b5 >> 2),
    )
}
