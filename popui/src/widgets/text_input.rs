use std::string::String;

use popugos::window::Window;

use crate::{draw, Constraints, EventResult, Point, Rect, Size, Theme, UiEvent, Widget};

const FONT_W: i32 = 9;
const FONT_H: i32 = 18;
const SCAN_ESC: u8 = 0x01;
const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_ENTER: u8 = 0x1C;

pub struct TextInput {
    text: String,
    rect: Rect,
    dirty: bool,
    focused: bool,
    max_len: usize,
}

impl TextInput {
    pub fn new(text: &str) -> Self {
        Self { text: String::from(text), rect: Rect::default(), dirty: true, focused: false, max_len: 64 }
    }
    pub fn text(&self) -> &str { &self.text }
    pub fn set_text(&mut self, text: &str) {
        if self.text != text { self.text = String::from(text); self.dirty = true; }
    }
    pub fn set_max_len(&mut self, max_len: usize) { self.max_len = max_len; }
}

impl Widget for TextInput {
    fn measure(&self, constraints: Constraints) -> Size {
        constraints.clamp(Size::new(self.text.chars().count().max(8) as f32 * FONT_W as f32 + 12.0, 26.0))
    }
    fn set_rect(&mut self, rect: Rect) { if self.rect != rect { self.rect = rect; self.dirty = true; } }
    fn rect(&self) -> Rect { self.rect }

    fn draw(&self, window: &mut Window, theme: &Theme) {
        if self.rect.w == 0 || self.rect.h == 0 { return; }
        draw::fill_rect(window, self.rect, theme.input_background);
        draw::stroke_rect(window, self.rect, if self.focused { theme.input_border_focus } else { theme.input_border }, 1);

        let max_chars = (self.rect.w as usize).saturating_sub(8) / FONT_W as usize;
        let skip = self.text.chars().count().saturating_sub(max_chars);
        let start = self.text.char_indices().nth(skip).map(|(i, _)| i).unwrap_or(0);
        let shown = &self.text[start..];
        let y = self.rect.y + (self.rect.h as i32 - FONT_H) / 2;
        draw::text(window, Point::new(self.rect.x + 4, y.max(self.rect.y + 2)), shown, theme.text);
        if self.focused {
            let x = self.rect.x + 4 + shown.chars().count() as i32 * FONT_W;
            draw::fill_rect(window, Rect::new(x, self.rect.y + 4, 1, self.rect.h.saturating_sub(8)), theme.text);
        }
    }

    fn event(&mut self, event: &UiEvent, focused: bool) -> EventResult {
        if !focused {
            return match *event {
                UiEvent::Down { x, y } if self.rect.contains(x, y) => EventResult::Consumed,
                _ => EventResult::Ignored,
            };
        }
        match *event {
            UiEvent::KeyDown { scancode, ch, .. } => {
                if scancode == SCAN_ESC { return EventResult::Ignored; }
                if scancode == SCAN_BACKSPACE {
                    if self.text.pop().is_some() { self.dirty = true; return EventResult::Changed; }
                    return EventResult::Consumed;
                }
                if scancode == SCAN_ENTER { return EventResult::Submitted; }
                if ch >= 0x20 && ch < 0x7f && self.text.len() < self.max_len {
                    self.text.push(ch as char); self.dirty = true; return EventResult::Changed;
                }
                EventResult::Consumed
            }
            UiEvent::KeyUp { .. } | UiEvent::Down { .. } | UiEvent::Up { .. } => EventResult::Consumed,
            UiEvent::Context { .. }
            | UiEvent::Move { .. }
            | UiEvent::Leave
            | UiEvent::Wheel { .. }
            | UiEvent::Resize { .. } => EventResult::Ignored,
        }
    }
    fn focusable(&self) -> bool { true }
    fn set_focused(&mut self, focused: bool) { if self.focused != focused { self.focused = focused; self.dirty = true; } }
    fn dirty(&self) -> bool { self.dirty }
    fn clear_dirty(&mut self) { self.dirty = false; }
    fn as_any(&self) -> &dyn core::any::Any { self }
    fn as_any_mut(&mut self) -> &mut dyn core::any::Any { self }
}
