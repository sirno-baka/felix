use core::any::Any;

use popugos::window::Window;

use crate::{Constraints, Rect, Size, Theme, UiEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventResult {
    Ignored,
    Consumed,
    Clicked,
    Changed,
    Submitted,
    /// The widget accepted a secondary click and wants its registered context callback.
    ContextRequested,
}

/// PopUI control. Drawing targets PopugOS `Window` directly; there is no
/// native/libfelix backend layer to keep in sync.
pub trait Widget {
    fn measure(&self, constraints: Constraints) -> Size;
    fn set_rect(&mut self, rect: Rect);
    fn rect(&self) -> Rect;
    fn draw(&self, window: &mut Window, theme: &Theme);
    fn event(&mut self, event: &UiEvent, focused: bool) -> EventResult;

    fn focusable(&self) -> bool { false }
    fn dirty(&self) -> bool { false }
    /// Optional widget-local damage. `None` means the whole widget rectangle.
    fn dirty_region(&self) -> Option<Rect> { None }
    fn clear_dirty(&mut self) {}
    fn set_focused(&mut self, _focused: bool) {}

    /// Extra hit area drawn above the normal layout tree (dropdowns/context menus).
    fn overlay_contains(&self, _x: i32, _y: i32) -> bool { false }
    fn overlay_active(&self) -> bool { false }
    fn draw_overlay(&self, _window: &mut Window, _theme: &Theme) {}
    fn dismiss_overlay(&mut self) {}
    /// Overlay widgets need a complete repaint when opening/closing so stale popup
    /// pixels are erased by the normal UI tree.
    fn full_redraw_when_dirty(&self) -> bool { false }

    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
