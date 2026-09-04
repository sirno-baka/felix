use crate::ui::theme::*;
use crate::ui::{Constraints, EventResult, Rect, UiEvent, Widget};
use alloc::string::String;
use embedded_graphics::{
    mono_font::MonoTextStyle,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle, RoundedRectangle},
    text::{Baseline, Text},
};
use taffy::geometry::Size as TSize;

const FONT_W: f32 = 9.0;
const FONT_H: f32 = 18.0;

pub struct Button {
    pub label: String,
    rect: Rect,
    dirty: bool,
    hot: bool,
    down: bool,
}
impl Button {
    pub fn new(label: &str) -> Self {
        Self {
            label: String::from(label),
            rect: Rect::default(),
            dirty: true,
            hot: false,
            down: false,
        }
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
}
impl Widget for Button {
    fn measure(&self, c: Constraints) -> TSize<f32> {
        // Content box only — Taffy adds Style padding (12/5).
        c.clamp(TSize {
            width: self.label.chars().count() as f32 * FONT_W,
            height: FONT_H,
        })
    }
    fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }
    fn rect(&self) -> Rect {
        self.rect
    }
    fn draw(&self, win: &mut crate::wm::Window) {
        let r = self.rect;
        if r.w == 0 || r.h == 0 {
            return;
        }
        let bg = if self.down && self.hot {
            BTN_BG_DOWN
        } else if self.hot {
            BTN_BG_HOT
        } else {
            BTN_BG
        };
        let rect = Rectangle::new(
            Point::new(r.x, r.y),
            embedded_graphics::geometry::Size::new(r.w, r.h),
        );
        let rr = RoundedRectangle::with_equal_corners(
            rect,
            embedded_graphics::geometry::Size::new(4, 4),
        );
        let _ = rr.into_styled(PrimitiveStyle::with_fill(bg)).draw(win);
        let _ = rr
            .into_styled(PrimitiveStyle::with_stroke(BTN_BORDER, 1))
            .draw(win);
        let style = MonoTextStyle::new(super::font(), TEXT);
        let tw = self.label.chars().count() as i32 * 9;
        let tx = r.x + (r.w as i32 - tw) / 2;
        let ty = r.y + (r.h as i32 - FONT_H as i32) / 2;
        let _ = Text::with_baseline(
            self.label.as_str(),
            Point::new(tx.max(r.x + 4), ty.max(r.y + 2)),
            style,
            Baseline::Top,
        )
        .draw(win);
    }
    fn event(&mut self, ev: &UiEvent, _focused: bool) -> EventResult {
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
                if click {
                    EventResult::Clicked
                } else {
                    EventResult::Consumed
                }
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
