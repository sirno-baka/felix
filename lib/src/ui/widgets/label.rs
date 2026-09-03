use alloc::string::String;
use embedded_graphics::{mono_font::MonoTextStyle, pixelcolor::Rgb888, prelude::*, text::{Baseline, Text}};
use embedded_graphics_unicodefonts::mono_9x18_atlas;
use taffy::geometry::Size;
use crate::ui::{Constraints, EventResult, Rect, UiEvent, Widget};
use crate::ui::theme::LABEL_FG;

const FONT_W: f32 = 9.0;
const FONT_H: f32 = 18.0;

pub struct Label {
    pub text: String,
    rect: Rect,
    dirty: bool,
    fg: Rgb888,
}
impl Label {
    pub fn new(text: &str) -> Self { Self { text: String::from(text), rect: Rect::default(), dirty: true, fg: LABEL_FG } }
    pub fn text(&self) -> &str { &self.text }
    pub fn set_text(&mut self, text: &str) { if self.text != text { self.text = String::from(text); self.dirty = true; } }
    pub fn set_color(&mut self, color: Rgb888) { self.fg = color; self.dirty = true; }
}
impl Widget for Label {
    fn measure(&self, c: Constraints) -> Size<f32> { c.clamp(Size { width: self.text.chars().count() as f32 * FONT_W, height: FONT_H }) }
    fn set_rect(&mut self, rect: Rect) { self.rect = rect; }
    fn rect(&self) -> Rect { self.rect }
    fn draw(&self, win: &mut crate::wm::Window) {
        let r = self.rect; if r.w == 0 || r.h == 0 { return; }
        let binding = mono_9x18_atlas();
        let style = MonoTextStyle::new(&binding, self.fg);
        let ty = r.y + (r.h as i32 - FONT_H as i32) / 2;
        let _ = Text::with_baseline(self.text.as_str(), Point::new(r.x, ty.max(r.y)), style, Baseline::Top).draw(win);
    }
    fn event(&mut self, _ev: &UiEvent, _focused: bool) -> EventResult { EventResult::Ignored }
    fn dirty(&self) -> bool { self.dirty }
    fn clear_dirty(&mut self) { self.dirty = false; }
    fn as_any(&self) -> &dyn core::any::Any { self }
    fn as_any_mut(&mut self) -> &mut dyn core::any::Any { self }
}
