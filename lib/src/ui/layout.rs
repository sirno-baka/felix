use crate::ui::{ScrollViewId, Ui};
use taffy::{
    geometry::{Point, Rect, Size},
    prelude::{
        AlignItems, Dimension, FlexDirection, JustifyContent, LengthPercentage as LP,
        LengthPercentageAuto as LPA, Position, Style,
    },
    tree::NodeId,
    Overflow,
};
use taffy::prelude::{TaffyAuto, TaffyZero};

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
    fn column(&mut self, parent: NodeId) -> NodeId {
        Ui::column(self, parent)
    }
    fn row(&mut self, parent: NodeId) -> NodeId {
        Ui::row(self, parent)
    }
    fn flex(&mut self, parent: NodeId, direction: FlexDirection) -> NodeId {
        Ui::flex(self, parent, direction)
    }
    fn panel(&mut self, parent: NodeId) -> NodeId {
        Ui::panel(self, parent)
    }
    fn spacer(&mut self, parent: NodeId) -> NodeId {
        Ui::spacer(self, parent)
    }
    fn scroll_view(&mut self, parent: NodeId) -> ScrollViewId {
        Ui::scroll_view(self, parent)
    }
    fn scroll_content(&self, scroll: ScrollViewId) -> Option<NodeId> {
        Ui::scroll_content(self, scroll)
    }
    fn style(&mut self, node: NodeId, update: impl FnOnce(&mut Style)) -> bool {
        Ui::style(self, node, update)
    }
    fn set_style(&mut self, node: NodeId, style: Style) -> bool {
        Ui::set_style(self, node, style)
    }
}
pub fn column() -> Style {
    let mut s = Style::default();
    s.flex_direction = FlexDirection::Column;
    s
}
pub fn row() -> Style {
    let mut s = Style::default();
    s.flex_direction = FlexDirection::Row;
    s
}
pub fn fill() -> Style {
    let mut s = Style::default();
    s.flex_grow = 1.0;
    s.min_size = Size {
        width: LPA::length(0.0),
        height: LPA::length(0.0),
    };
    s
}
pub fn padding(v: f32) -> Style {
    let p = LP::length(v);
    let mut s = Style::default();
    s.padding = Rect {
        left: p,
        right: p,
        top: p,
        bottom: p,
    };
    s
}
pub fn gap(v: f32) -> Style {
    let g = LP::length(v);
    let mut s = Style::default();
    s.gap = Size {
        width: g,
        height: g,
    };
    s
}
pub fn size(width: f32, height: f32) -> Style {
    let mut s = Style::default();
    s.size = Size {
        width: Dimension::length(width),
        height: Dimension::length(height),
    };
    s
}
pub fn centered() -> Style {
    let mut s = Style::default();
    s.justify_content = Some(JustifyContent::CENTER);
    s.align_items = Some(AlignItems::CENTER);
    s
}
pub fn absolute() -> Style {
    let mut s = Style::default();
    s.position = Position::Absolute;
    s
}
pub fn clipped() -> Style {
    let mut s = Style::default();
    s.overflow = Point {
        x: Overflow::Hidden,
        y: Overflow::Hidden,
    };
    s
}

fn is_default_dimension(d: Dimension) -> bool {
    matches!(d, Dimension::AUTO)
}

fn is_default_lpa(v: LPA) -> bool {
    matches!(v, LPA::AUTO)
}

fn is_zero_lp(v: LP) -> bool {
    matches!(v, LP::ZERO)
}

/// Copy only fields that differ from `Style::default()` onto `dst`.
pub fn merge_style(dst: &mut Style, src: &Style) {
    let def = Style::<alloc::string::String>::DEFAULT;
    if src.flex_direction != def.flex_direction {
        dst.flex_direction = src.flex_direction;
    }
    if src.flex_grow != def.flex_grow {
        dst.flex_grow = src.flex_grow;
    }
    if src.flex_shrink != def.flex_shrink {
        dst.flex_shrink = src.flex_shrink;
    }
    if src.position != def.position {
        dst.position = src.position;
    }
    if src.justify_content != def.justify_content {
        dst.justify_content = src.justify_content;
    }
    if src.align_items != def.align_items {
        dst.align_items = src.align_items;
    }
    if src.align_content != def.align_content {
        dst.align_content = src.align_content;
    }
    if !is_default_dimension(src.size.width) {
        dst.size.width = src.size.width;
    }
    if !is_default_dimension(src.size.height) {
        dst.size.height = src.size.height;
    }
    if !is_default_lpa(src.min_size.width) {
        dst.min_size.width = src.min_size.width;
    }
    if !is_default_lpa(src.min_size.height) {
        dst.min_size.height = src.min_size.height;
    }
    if !is_default_lpa(src.max_size.width) {
        dst.max_size.width = src.max_size.width;
    }
    if !is_default_lpa(src.max_size.height) {
        dst.max_size.height = src.max_size.height;
    }
    if !is_zero_lp(src.padding.left) {
        dst.padding.left = src.padding.left;
    }
    if !is_zero_lp(src.padding.right) {
        dst.padding.right = src.padding.right;
    }
    if !is_zero_lp(src.padding.top) {
        dst.padding.top = src.padding.top;
    }
    if !is_zero_lp(src.padding.bottom) {
        dst.padding.bottom = src.padding.bottom;
    }
    if !is_zero_lp(src.gap.width) {
        dst.gap.width = src.gap.width;
    }
    if !is_zero_lp(src.gap.height) {
        dst.gap.height = src.gap.height;
    }
    if src.overflow != def.overflow {
        dst.overflow = src.overflow;
    }
    if src.inset != def.inset {
        dst.inset = src.inset;
    }
}
