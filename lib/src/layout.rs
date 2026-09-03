//! Explicit-parent layout helpers for `libfelix`.
//!
//! This module is intentionally a thin layer over [`crate::ui::Ui`].  It keeps
//! the layout tree explicit: every container and widget is created under a
//! `NodeId` returned by its parent.  This avoids relying on `Ui::up()` or on
//! whichever node happened to be created last.
//!
//! The underlying Taffy style remains available through `Ui::style`.

use crate::ui::{AlignContent, AlignItems, FlexDirection, JustifyContent, NodeId, Position, Style, Ui, WidgetId};

/// Explicit-parent layout API for [`Ui`].
///
/// The existing cursor API remains available for compatibility. New code can
/// import this trait and build layouts using `*_in(parent, ...)` methods.
pub trait UiLayoutExt {
    /// Create a flex container under `parent`.
    fn flex_in(&mut self, parent: NodeId, direction: FlexDirection) -> NodeId;

    /// Create a vertical flex container under `parent`.
    fn column_in(&mut self, parent: NodeId) -> NodeId;

    /// Create a horizontal flex container under `parent`.
    fn row_in(&mut self, parent: NodeId) -> NodeId;

    /// Create a filled/bordered panel under `parent`.
    fn panel_in(&mut self, parent: NodeId) -> NodeId;

    /// Create a flexible spacer under `parent`.
    fn spacer_in(&mut self, parent: NodeId) -> NodeId;

    /// Create a button under `parent`.
    fn button_in(&mut self, parent: NodeId, label: &str) -> WidgetId;

    /// Create a label under `parent`.
    fn label_in(&mut self, parent: NodeId, text: &str) -> WidgetId;

    /// Create a text input under `parent`.
    fn text_input_in(&mut self, parent: NodeId) -> WidgetId;

    /// Create a text input with initial text under `parent`.
    fn text_input_with_in(&mut self, parent: NodeId, text: &str) -> WidgetId;

    /// Apply a complete Taffy style to an explicit node.
    fn style_node(&mut self, node: NodeId, style: Style) -> bool;

    /// Mutate an explicit Taffy style without changing the build parent.
    fn update_style(&mut self, node: NodeId, update: impl FnOnce(&mut Style)) -> bool;
}

impl UiLayoutExt for Ui {
    fn flex_in(&mut self, parent: NodeId, direction: FlexDirection) -> NodeId {
        self.with_container(parent, |ui| {
            let node = ui.flex();
            ui.style(node, |s| s.flex_direction = direction);
            node
        })
    }

    fn column_in(&mut self, parent: NodeId) -> NodeId {
        self.with_container(parent, |ui| ui.column())
    }

    fn row_in(&mut self, parent: NodeId) -> NodeId {
        self.with_container(parent, |ui| ui.row())
    }

    fn panel_in(&mut self, parent: NodeId) -> NodeId {
        self.with_container(parent, |ui| ui.panel())
    }

    fn spacer_in(&mut self, parent: NodeId) -> NodeId {
        self.with_container(parent, |ui| ui.spacer())
    }

    fn button_in(&mut self, parent: NodeId, label: &str) -> WidgetId {
        self.with_container(parent, |ui| ui.button(label))
    }

    fn label_in(&mut self, parent: NodeId, text: &str) -> WidgetId {
        self.with_container(parent, |ui| ui.label(text))
    }

    fn text_input_in(&mut self, parent: NodeId) -> WidgetId {
        self.with_container(parent, |ui| ui.text_input())
    }

    fn text_input_with_in(&mut self, parent: NodeId, text: &str) -> WidgetId {
        self.with_container(parent, |ui| ui.text_input_with(text))
    }

    fn style_node(&mut self, node: NodeId, style: Style) -> bool {
        self.style(node, |dst| *dst = style)
    }

    fn update_style(&mut self, node: NodeId, update: impl FnOnce(&mut Style)) -> bool {
        self.style(node, update)
    }
}

/// Small style presets for common application layouts.
pub mod presets {
    use super::*;
    use taffy::geometry::{Rect, Size};
    use taffy::prelude::{Dimension, LengthPercentage as LP, LengthPercentageAuto as LPA};

    /// A full-height body region that consumes remaining vertical space.
    pub fn fill_column() -> Style {
        let mut s = Style::default();
        s.flex_direction = FlexDirection::Column;
        s.flex_grow = 1.0;
        s.min_size.height = LPA::length(0.0);
        s
    }

    /// A row that stretches to its parent's width and has a fixed gap.
    pub fn row_gap(gap: f32) -> Style {
        let mut s = Style::default();
        s.flex_direction = FlexDirection::Row;
        s.gap = Size {
            width: LP::length(gap),
            height: LP::length(gap),
        };
        s
    }

    /// Padding on all four sides.
    pub fn padding(v: f32) -> Style {
        let mut s = Style::default();
        let p = LP::length(v);
        s.padding = Rect {
            left: p,
            right: p,
            top: p,
            bottom: p,
        };
        s
    }

    /// A fixed-size style.
    pub fn fixed_size(width: f32, height: f32) -> Style {
        let mut s = Style::default();
        s.size = Size {
            width: Dimension::length(width),
            height: Dimension::length(height),
        };
        s
    }

    /// Center children on both axes.
    pub fn centered() -> Style {
        let mut s = Style::default();
        s.justify_content = Some(JustifyContent::CENTER);
        s.align_items = Some(AlignItems::CENTER);
        s
    }

    /// Stretch children across the cross axis and start them on the main axis.
    pub fn start() -> Style {
        let mut s = Style::default();
        s.justify_content = Some(JustifyContent::START);
        s.align_items = Some(AlignItems::STRETCH);
        s.align_content = Some(AlignContent::FLEX_START);
        s
    }

    /// Absolute-positioned node style.
    pub fn absolute() -> Style {
        let mut s = Style::default();
        s.position = Position::ABSOLUTE;
        s
    }
}
