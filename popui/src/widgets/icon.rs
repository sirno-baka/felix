use popugos::window::Window;

use crate::{draw, Constraints, EventResult, Image, Point, Rect, Size, Theme, UiEvent, Widget};

pub struct Icon {
    image: Image,
    rect: Rect,
    dirty: bool,
}

impl Icon {
    pub fn new(image: Image) -> Self { Self { image, rect: Rect::default(), dirty: true } }
    pub fn image(&self) -> &Image { &self.image }
    pub fn set_image(&mut self, image: Image) { self.image = image; self.dirty = true; }
}

impl Widget for Icon {
    fn measure(&self, constraints: Constraints) -> Size {
        constraints.clamp(Size::new(self.image.width as f32, self.image.height as f32))
    }
    fn set_rect(&mut self, rect: Rect) { if self.rect != rect { self.rect = rect; self.dirty = true; } }
    fn rect(&self) -> Rect { self.rect }
    fn draw(&self, window: &mut Window, _theme: &Theme) {
        if self.rect.w == 0 || self.rect.h == 0 || self.image.width == 0 || self.image.height == 0 { return; }
        let x = self.rect.x + (self.rect.w as i32 - self.image.width as i32) / 2;
        let y = self.rect.y + (self.rect.h as i32 - self.image.height as i32) / 2;
        draw::blit_rgb565(window, Point::new(x, y), self.image.width, self.image.height, self.image.pixels(), self.image.mask());
    }
    fn event(&mut self, _event: &UiEvent, _focused: bool) -> EventResult { EventResult::Ignored }
    fn dirty(&self) -> bool { self.dirty }
    fn clear_dirty(&mut self) { self.dirty = false; }
    fn as_any(&self) -> &dyn core::any::Any { self }
    fn as_any_mut(&mut self) -> &mut dyn core::any::Any { self }
}
