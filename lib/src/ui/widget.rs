use crate::ui::{Constraints, Rect, UiEvent};
use crate::wm::Window;
use core::any::Any;
use taffy::geometry::Size;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventResult {
    Ignored,
    Consumed,
    Clicked,
    Changed,
    Submitted,
}

pub trait Widget {
    fn measure(&self, constraints: Constraints) -> Size<f32>;
    fn set_rect(&mut self, rect: Rect);
    fn rect(&self) -> Rect;
    fn draw(&self, win: &mut Window);
    fn event(&mut self, event: &UiEvent, focused: bool) -> EventResult;
    fn focusable(&self) -> bool {
        false
    }
    fn dirty(&self) -> bool {
        false
    }
    fn clear_dirty(&mut self) {}
    fn set_focused(&mut self, _focused: bool) {}
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
