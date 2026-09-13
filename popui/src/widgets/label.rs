use std::string::String;

use popugos::window::Window;

use crate::{draw, Constraints, EventResult, Point, Rect, Size, Theme, UiEvent, Widget};

const FONT_W: f32 = 9.0;
const FONT_H: f32 = 18.0;

pub struct Label {
    text: String,
    rect: Rect,
    dirty: bool,
}

impl Label {
    pub fn new(text: &str) -> Self { Self { text: String::from(text), rect: Rect::default(), dirty: true } }
    pub fn text(&self) -> &str { &self.text }
    pub fn set_text(&mut self, text: &str) {
        if self.text != text { self.text = String::from(text); self.dirty = true; }
    }
}

impl Widget for Label {
    fn measure(&self, constraints: Constraints) -> Size {
        constraints.clamp(Size::new(self.text.chars().count() as f32 * FONT_W, FONT_H))
    }
    fn set_rect(&mut self, rect: Rect) { if self.rect != rect { self.rect = rect; self.dirty = true; } }
    fn rect(&self) -> Rect { self.rect }
    fn draw(&self, window: &mut Window, theme: &Theme) {
        if self.rect.w == 0 || self.rect.h == 0 { return; }
        let y = self.rect.y + (self.rect.h as i32 - FONT_H as i32) / 2;
        draw::text(window, Point::new(self.rect.x, y.max(self.rect.y)), &self.text, theme.label);
    }
    fn event(&mut self, _event: &UiEvent, _focused: bool) -> EventResult { EventResult::Ignored }
    fn dirty(&self) -> bool { self.dirty }
    fn clear_dirty(&mut self) { self.dirty = false; }
    fn as_any(&self) -> &dyn core::any::Any { self }
    fn as_any_mut(&mut self) -> &mut dyn core::any::Any { self }
}
