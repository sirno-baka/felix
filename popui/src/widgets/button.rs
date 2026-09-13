use std::string::String;

use popugos::window::Window;

use crate::{draw, Constraints, EventResult, Point, Rect, Size, Theme, UiEvent, Widget};

const FONT_W: f32 = 9.0;
const FONT_H: f32 = 18.0;

pub struct Button {
    label: String,
    rect: Rect,
    dirty: bool,
    hot: bool,
    down: bool,
}

impl Button {
    pub fn new(label: &str) -> Self {
        Self { label: String::from(label), rect: Rect::default(), dirty: true, hot: false, down: false }
    }
    pub fn label(&self) -> &str { &self.label }
    pub fn set_label(&mut self, label: &str) {
        if self.label != label { self.label = String::from(label); self.dirty = true; }
    }
}

impl Widget for Button {
    fn measure(&self, constraints: Constraints) -> Size {
        constraints.clamp(Size::new(self.label.chars().count() as f32 * FONT_W + 24.0, FONT_H + 10.0))
    }
    fn set_rect(&mut self, rect: Rect) { if self.rect != rect { self.rect = rect; self.dirty = true; } }
    fn rect(&self) -> Rect { self.rect }

    fn draw(&self, window: &mut Window, theme: &Theme) {
        if self.rect.w == 0 || self.rect.h == 0 { return; }
        let background = if self.down && self.hot { theme.button_down }
            else if self.hot { theme.button_hot } else { theme.button };
        draw::fill_rect(window, self.rect, background);
        draw::stroke_rect(window, self.rect, theme.button_border, 1);
        let text_w = self.label.chars().count() as i32 * FONT_W as i32;
        let x = self.rect.x + (self.rect.w as i32 - text_w) / 2;
        let y = self.rect.y + (self.rect.h as i32 - FONT_H as i32) / 2;
        draw::text(window, Point::new(x.max(self.rect.x + 4), y.max(self.rect.y + 2)), &self.label, theme.text);
    }

    fn event(&mut self, event: &UiEvent, _focused: bool) -> EventResult {
        match *event {
            UiEvent::Down { x, y } if self.rect.contains(x, y) => {
                self.down = true; self.hot = true; self.dirty = true; EventResult::Consumed
            }
            UiEvent::Up { x, y } if self.down => {
                let clicked = self.rect.contains(x, y);
                self.down = false; self.hot = clicked; self.dirty = true;
                if clicked { EventResult::Clicked } else { EventResult::Consumed }
            }
            UiEvent::Move { x, y } => {
                let hot = self.rect.contains(x, y);
                if hot != self.hot { self.hot = hot; self.dirty = true; EventResult::Changed } else { EventResult::Ignored }
            }
            UiEvent::Leave if self.hot => {
                self.hot = false;
                self.dirty = true;
                EventResult::Changed
            }
            _ => EventResult::Ignored,
        }
    }
    fn focusable(&self) -> bool { true }
    fn dirty(&self) -> bool { self.dirty }
    fn clear_dirty(&mut self) { self.dirty = false; }
    fn as_any(&self) -> &dyn core::any::Any { self }
    fn as_any_mut(&mut self) -> &mut dyn core::any::Any { self }
}
