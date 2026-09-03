use crate::ui::{ScrollViewId, Ui};
use taffy::{geometry::{Point, Rect, Size}, prelude::{AlignItems, Dimension, FlexDirection, JustifyContent, LengthPercentage as LP, LengthPercentageAuto as LPA, Position, Style}, tree::NodeId, Overflow};

pub trait LayoutApi {
    fn column(&mut self, parent: NodeId) -> NodeId;
    fn row(&mut self, parent: NodeId) -> NodeId;
    fn flex(&mut self, parent: NodeId, direction: FlexDirection) -> NodeId;
    fn panel(&mut self, parent: NodeId) -> NodeId;
    fn spacer(&mut self, parent: NodeId) -> NodeId;
    fn scroll_view(&mut self, parent: NodeId) -> ScrollViewId;
    fn scroll_content(&self, scroll: ScrollViewId) -> Option<NodeId>;
    fn style(&mut self, node: NodeId, update: impl FnOnce(&mut Style)) -> bool;
    fn set_style(&mut self, node: NodeId, style: Style) -> bool;
}
impl LayoutApi for Ui {
    fn column(&mut self, parent: NodeId) -> NodeId { Ui::column(self, parent) }
    fn row(&mut self, parent: NodeId) -> NodeId { Ui::row(self, parent) }
    fn flex(&mut self, parent: NodeId, direction: FlexDirection) -> NodeId { Ui::flex(self, parent, direction) }
    fn panel(&mut self, parent: NodeId) -> NodeId { Ui::panel(self, parent) }
    fn spacer(&mut self, parent: NodeId) -> NodeId { Ui::spacer(self, parent) }
    fn scroll_view(&mut self, parent: NodeId) -> ScrollViewId { Ui::scroll_view(self, parent) }
    fn scroll_content(&self, scroll: ScrollViewId) -> Option<NodeId> { Ui::scroll_content(self, scroll) }
    fn style(&mut self, node: NodeId, update: impl FnOnce(&mut Style)) -> bool { Ui::style(self, node, update) }
    fn set_style(&mut self, node: NodeId, style: Style) -> bool { Ui::set_style(self, node, style) }
}
pub fn column() -> Style { let mut s = Style::default(); s.flex_direction = FlexDirection::Column; s }
pub fn row() -> Style { let mut s = Style::default(); s.flex_direction = FlexDirection::Row; s }
pub fn fill() -> Style { let mut s = Style::default(); s.flex_grow = 1.0; s.min_size = Size { width: LPA::length(0.0), height: LPA::length(0.0) }; s }
pub fn padding(v: f32) -> Style { let p = LP::length(v); let mut s = Style::default(); s.padding = Rect { left: p, right: p, top: p, bottom: p }; s }
pub fn gap(v: f32) -> Style { let g = LP::length(v); let mut s = Style::default(); s.gap = Size { width: g, height: g }; s }
pub fn size(width: f32, height: f32) -> Style { let mut s = Style::default(); s.size = Size { width: Dimension::length(width), height: Dimension::length(height) }; s }
pub fn centered() -> Style { let mut s = Style::default(); s.justify_content = Some(JustifyContent::CENTER); s.align_items = Some(AlignItems::CENTER); s }
pub fn absolute() -> Style { let mut s = Style::default(); s.position = Position::Absolute; s }
pub fn clipped() -> Style { let mut s = Style::default(); s.overflow = Point { x: Overflow::Hidden, y: Overflow::Hidden }; s }
