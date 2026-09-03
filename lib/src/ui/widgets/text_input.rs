use crate::ui::theme::{INPUT_BG, INPUT_BORDER, INPUT_BORDER_FOCUS, TEXT};
use crate::ui::{Constraints, EventResult, Rect, UiEvent, Widget};
use alloc::string::String;
use embedded_graphics::{
    mono_font::MonoTextStyle,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use embedded_graphics_unicodefonts::mono_9x18_atlas;
use taffy::geometry::Size as TSize;

const FONT_W: f32 = 9.0;
const FONT_H: f32 = 18.0;
const SCAN_ESC: u8 = 0x01;
const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_ENTER: u8 = 0x1C;

pub struct TextInput {
    pub text: String,
    rect: Rect,
    dirty: bool,
    focused: bool,
    max_len: usize,
}
impl TextInput {
    pub fn new(text: &str) -> Self {
        Self {
            text: String::from(text),
            rect: Rect::default(),
            dirty: true,
            focused: false,
            max_len: 64,
        }
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn set_text(&mut self, text: &str) {
        if self.text != text {
            self.text = String::from(text);
            self.dirty = true;
        }
    }
    pub fn set_max_len(&mut self, max_len: usize) {
        self.max_len = max_len;
    }
}
impl Widget for TextInput {
    fn measure(&self, c: Constraints) -> TSize<f32> {
        c.clamp(TSize {
            width: self.text.chars().count().max(8) as f32 * FONT_W + 12.0,
            height: 26.0,
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
        let border = if self.focused {
            INPUT_BORDER_FOCUS
        } else {
            INPUT_BORDER
        };
        let rect = Rectangle::new(
            Point::new(r.x, r.y),
            embedded_graphics::geometry::Size::new(r.w, r.h),
        );
        let _ = rect
            .into_styled(PrimitiveStyle::with_fill(INPUT_BG))
            .draw(win);
        let _ = rect
            .into_styled(PrimitiveStyle::with_stroke(border, 1))
            .draw(win);
        let binding = mono_9x18_atlas();
        let style = MonoTextStyle::new(&binding, TEXT);
        let max_chars = (r.w as usize).saturating_sub(8) / FONT_W as usize;
        let shown = if self.text.len() > max_chars {
            &self.text[self.text.len() - max_chars..]
        } else {
            self.text.as_str()
        };
        let ty = r.y + (r.h as i32 - FONT_H as i32) / 2;
        let _ = Text::with_baseline(
            shown,
            Point::new(r.x + 4, ty.max(r.y + 2)),
            style,
            Baseline::Top,
        )
        .draw(win);
        if self.focused {
            let cx = r.x + 4 + shown.len() as i32 * FONT_W as i32;
            let _ = Rectangle::new(
                Point::new(cx, r.y + 4),
                embedded_graphics::geometry::Size::new(2, r.h.saturating_sub(8)),
            )
            .into_styled(PrimitiveStyle::with_fill(TEXT))
            .draw(win);
        }
    }
    fn event(&mut self, ev: &UiEvent, focused: bool) -> EventResult {
        if !focused {
            return match *ev {
                UiEvent::Down { x, y } if self.rect.contains(x, y) => EventResult::Consumed,
                _ => EventResult::Ignored,
            };
        }
        match *ev {
            UiEvent::KeyDown { scancode, ch, .. } => {
                if scancode == SCAN_ESC {
                    return EventResult::Ignored;
                }
                if scancode == SCAN_BACKSPACE {
                    if self.text.pop().is_some() {
                        return EventResult::Changed;
                    }
                    return EventResult::Consumed;
                }
                if scancode == SCAN_ENTER {
                    return EventResult::Submitted;
                }
                if ch >= 0x20 && ch < 0x7f && self.text.len() < self.max_len {
                    self.text.push(ch as char);
                    return EventResult::Changed;
                }
                EventResult::Consumed
            }
            UiEvent::KeyUp { .. } | UiEvent::Down { .. } | UiEvent::Up { .. } => {
                EventResult::Consumed
            }
            UiEvent::Move { .. } => EventResult::Ignored,
        }
    }
    fn focusable(&self) -> bool {
        true
    }
    fn set_focused(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.dirty = true;
        }
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
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
