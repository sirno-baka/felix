use std::string::String;

use popugos::window::Window;

use crate::{draw, Constraints, EventResult, Image, Point, Rect, Size, Theme, UiEvent, Widget};

const FONT_W: i32 = 9;
const FONT_H: i32 = 18;
const ICON_SIZE: i32 = 20;
const GAP: i32 = 5;

pub struct ToolbarButton {
    label: String,
    icon: Option<Image>,
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

    pub fn with_icon(label: &str, icon: Image) -> Self {
        let mut out = Self::new(label);
        out.icon = Some(icon);
        out
    }

    pub fn label(&self) -> &str { &self.label }
    pub fn set_label(&mut self, label: &str) {
        if self.label != label {
            self.label = String::from(label);
            self.dirty = true;
        }
    }

    pub fn enabled(&self) -> bool { self.enabled }
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            if !enabled {
                self.hot = false;
                self.down = false;
            }
            self.dirty = true;
        }
    }

    pub fn set_icon(&mut self, icon: Image) {
        self.icon = Some(icon);
        self.dirty = true;
    }

    pub fn clear_icon(&mut self) {
        if self.icon.take().is_some() { self.dirty = true; }
    }
}

impl Widget for ToolbarButton {
    fn measure(&self, constraints: Constraints) -> Size {
        let icon_w = if self.icon.is_some() { ICON_SIZE + if self.label.is_empty() { 0 } else { GAP } } else { 0 };
        let text_w = self.label.chars().count() as i32 * FONT_W;
        constraints.clamp(Size::new((icon_w + text_w + 10) as f32, 30.0))
    }

    fn set_rect(&mut self, rect: Rect) {
        if self.rect != rect { self.rect = rect; self.dirty = true; }
    }

    fn rect(&self) -> Rect { self.rect }

    fn draw(&self, window: &mut Window, theme: &Theme) {
        if self.rect.w == 0 || self.rect.h == 0 { return; }
        let bg = if self.down && self.hot && self.enabled {
            theme.button_down
        } else if self.hot && self.enabled {
            theme.button_hot
        } else {
            theme.panel_background
        };
        draw::fill_rect(window, self.rect, bg);
        if (self.hot || self.down) && self.enabled {
            draw::stroke_rect(window, self.rect, theme.button_border, 1);
        }

        let mut x = self.rect.x + 5;
        if let Some(icon) = self.icon.as_ref() {
            let y = self.rect.y + (self.rect.h as i32 - ICON_SIZE) / 2;
            draw::blit_rgb565_cropped(
                window,
                Point::new(x, y),
                icon.width,
                icon.height,
                icon.pixels(),
                icon.mask(),
                ICON_SIZE,
                ICON_SIZE,
            );
            x += ICON_SIZE + if self.label.is_empty() { 0 } else { GAP };
        }

        if !self.label.is_empty() {
            let y = self.rect.y + (self.rect.h as i32 - FONT_H) / 2;
            draw::text(window, Point::new(x, y.max(self.rect.y + 2)), &self.label, if self.enabled { theme.text } else { theme.label });
        }
    }

    fn event(&mut self, event: &UiEvent, _focused: bool) -> EventResult {
        if !self.enabled { return EventResult::Ignored; }
        match *event {
            UiEvent::Down { x, y } if self.rect.contains(x, y) => {
                self.down = true; self.hot = true; self.dirty = true; EventResult::Consumed
            }
            UiEvent::Up { x, y } if self.down => {
                let click = self.rect.contains(x, y);
                self.down = false; self.hot = click; self.dirty = true;
                if click { EventResult::Clicked } else { EventResult::Consumed }
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
