use std::{boxed::Box, vec::Vec};

use popugos::window::{Window, WindowError};

use taffy::{
    geometry::Size as TSize,
    prelude::{AvailableSpace, Dimension, FlexDirection, LengthPercentageAuto as LPA, Style},
    tree::{NodeId, TaffyTree},
    TraversePartialTree,
};

use crate::{draw, layout, Constraints, EventResult, Rect, Size, Theme, UiEvent, Widget, DARK_THEME};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WidgetId(usize);

impl WidgetId {
    pub const fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy)]
enum NodeCtx {
    Widget(WidgetId),
    Panel,
}

type ClickHandler = Box<dyn FnMut(&mut Ui)>;
type ChangeHandler = Box<dyn FnMut(&mut Ui)>;
type ContextHandler = Box<dyn FnMut(&mut Ui, i32, i32)>;

/// Retained-mode PopugOS UI tree.
///
/// `Ui` owns synchronous widget/layout state and draws directly into a
/// `popugos::window::Window`. Tokio-driven waiting and application messages live
/// in [`crate::UiRuntime`].
pub struct Ui {
    taffy: TaffyTree<NodeCtx>,
    widgets: Vec<Box<dyn Widget>>,
    widget_nodes: Vec<NodeId>,
    widget_clips: Vec<Option<Rect>>,
    clicks: Vec<Option<ClickHandler>>,
    changes: Vec<Option<ChangeHandler>>,
    contexts: Vec<Option<ContextHandler>>,
    root: NodeId,
    root_w: u32,
    root_h: u32,
    focus: Option<WidgetId>,
    hovered: Option<WidgetId>,
    pressed: Option<WidgetId>,
    dirty: bool,
    dirty_rect: Option<Rect>,
    needs_layout: bool,
    theme: Theme,
}

impl Ui {
    pub fn new() -> Self {
        Self::with_size(1, 1)
    }

    pub fn with_size(width: u32, height: u32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let mut taffy = TaffyTree::new();
        let mut style = Style::default();
        style.flex_direction = FlexDirection::Column;
        style.size = TSize {
            width: Dimension::length(width as f32),
            height: Dimension::length(height as f32),
        };
        let root = taffy.new_leaf(style).expect("popui: create root");
        Self {
            taffy,
            widgets: Vec::new(),
            widget_nodes: Vec::new(),
            widget_clips: Vec::new(),
            clicks: Vec::new(),
            changes: Vec::new(),
            contexts: Vec::new(),
            root,
            root_w: width,
            root_h: height,
            focus: None,
            hovered: None,
            pressed: None,
            dirty: true,
            dirty_rect: None,
            needs_layout: true,
            theme: DARK_THEME,
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn root_size(&self) -> (u32, u32) {
        (self.root_w, self.root_h)
    }

    pub fn theme(&self) -> Theme {
        self.theme
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.request_full_redraw();
    }

    pub fn style(&mut self, node: NodeId, update: impl FnOnce(&mut Style)) -> bool {
        let Some(mut style) = self.taffy.style(node).ok().cloned() else {
            return false;
        };
        update(&mut style);
        if self.taffy.set_style(node, style).is_ok() {
            self.needs_layout = true;
            true
        } else {
            false
        }
    }

    pub fn set_style(&mut self, node: NodeId, style: Style) -> bool {
        self.style(node, |current| layout::merge_style(current, &style))
    }

    pub fn column(&mut self, parent: NodeId) -> NodeId {
        self.container(parent, FlexDirection::Column)
    }

    pub fn row(&mut self, parent: NodeId) -> NodeId {
        self.container(parent, FlexDirection::Row)
    }

    pub fn toolbar(&mut self, parent: NodeId) -> NodeId {
        let node = self.container(parent, FlexDirection::Row);
        let _ = self.style(node, |style| {
            style.size.height = Dimension::length(36.0);
            style.flex_shrink = 0.0;
            style.align_items = Some(taffy::prelude::AlignItems::CENTER);
        });
        node
    }

    pub fn menu(&mut self, parent: NodeId, menu: crate::Menu) -> WidgetId {
        let mut style = Style::default();
        style.size.height = Dimension::length(26.0);
        style.flex_shrink = 0.0;
        self.add_widget(parent, menu, style)
    }

    pub fn flex(&mut self, parent: NodeId, direction: FlexDirection) -> NodeId {
        self.container(parent, direction)
    }

    fn container(&mut self, parent: NodeId, direction: FlexDirection) -> NodeId {
        let mut style = Style::default();
        style.flex_direction = direction;
        style.min_size = TSize {
            width: LPA::length(0.0),
            height: LPA::length(0.0),
        };
        let node = self.taffy.new_leaf(style).expect("popui: create container");
        self.taffy.add_child(parent, node).expect("popui: add container");
        self.needs_layout = true;
        node
    }

    pub fn panel(&mut self, parent: NodeId) -> NodeId {
        let mut style = Style::default();
        style.flex_direction = FlexDirection::Column;
        style.flex_grow = 1.0;
        style.min_size = TSize {
            width: LPA::length(0.0),
            height: LPA::length(0.0),
        };
        let node = self
            .taffy
            .new_leaf_with_context(style, NodeCtx::Panel)
            .expect("popui: create panel");
        self.taffy.add_child(parent, node).expect("popui: add panel");
        self.needs_layout = true;
        node
    }

    pub fn spacer(&mut self, parent: NodeId) -> NodeId {
        let mut style = Style::default();
        style.flex_grow = 1.0;
        let node = self.taffy.new_leaf(style).expect("popui: create spacer");
        self.taffy.add_child(parent, node).expect("popui: add spacer");
        self.needs_layout = true;
        node
    }

    pub fn button(&mut self, parent: NodeId, label: &str) -> WidgetId {
        self.add_widget(parent, crate::Button::new(label), Style::default())
    }

    pub fn toolbar_button(&mut self, parent: NodeId, label: &str) -> WidgetId {
        let mut style = Style::default();
        style.flex_shrink = 0.0;
        self.add_widget(parent, crate::ToolbarButton::new(label), style)
    }

    pub fn label(&mut self, parent: NodeId, text: &str) -> WidgetId {
        self.add_widget(parent, crate::Label::new(text), Style::default())
    }

    pub fn text_input(&mut self, parent: NodeId) -> WidgetId {
        self.text_input_with(parent, "")
    }

    pub fn text_input_with(&mut self, parent: NodeId, text: &str) -> WidgetId {
        self.add_widget(parent, crate::TextInput::new(text), Style::default())
    }

    pub fn text_area(&mut self, parent: NodeId) -> WidgetId {
        let mut style = Style::default();
        style.flex_grow = 1.0;
        style.flex_shrink = 1.0;
        self.add_widget(parent, crate::TextArea::new(), style)
    }

    pub fn text_area_with(&mut self, parent: NodeId, text: &str) -> WidgetId {
        let mut style = Style::default();
        style.flex_grow = 1.0;
        style.flex_shrink = 1.0;
        self.add_widget(parent, crate::TextArea::with_text(text), style)
    }

    pub fn icon(&mut self, parent: NodeId, image: crate::Image) -> WidgetId {
        self.add_widget(parent, crate::Icon::new(image), Style::default())
    }

    pub fn file_view(&mut self, parent: NodeId) -> WidgetId {
        let mut style = Style::default();
        style.flex_grow = 1.0;
        style.flex_shrink = 1.0;
        self.add_widget(parent, crate::FileView::new(), style)
    }

    pub fn tree_view(&mut self, parent: NodeId) -> WidgetId {
        let mut style = Style::default();
        style.flex_grow = 1.0;
        style.flex_shrink = 1.0;
        self.add_widget(parent, crate::TreeView::new(), style)
    }

    pub fn add_widget<W: Widget + 'static>(&mut self, parent: NodeId, widget: W, style: Style) -> WidgetId {
        self.add_boxed_widget(parent, Box::new(widget), style)
    }

    pub fn add_boxed_widget(
        &mut self,
        parent: NodeId,
        widget: Box<dyn Widget>,
        style: Style,
    ) -> WidgetId {
        let id = WidgetId(self.widgets.len());
        let node = self
            .taffy
            .new_leaf_with_context(style, NodeCtx::Widget(id))
            .expect("popui: create widget");
        self.taffy.add_child(parent, node).expect("popui: add widget");
        self.widgets.push(widget);
        self.widget_nodes.push(node);
        self.widget_clips.push(None);
        self.clicks.push(None);
        self.changes.push(None);
        self.contexts.push(None);
        self.needs_layout = true;
        id
    }

    pub fn widget<T: 'static>(&self, id: WidgetId) -> Option<&T> {
        self.widgets.get(id.0)?.as_any().downcast_ref::<T>()
    }

    pub fn widget_mut<T: 'static>(&mut self, id: WidgetId) -> Option<&mut T> {
        self.widgets.get_mut(id.0)?.as_any_mut().downcast_mut::<T>()
    }

    pub fn node_of(&self, id: WidgetId) -> Option<NodeId> {
        self.widget_nodes.get(id.0).copied()
    }

    pub fn rect(&self, id: WidgetId) -> Option<Rect> {
        self.widgets.get(id.0).map(|widget| widget.rect())
    }

    pub fn focus(&self) -> Option<WidgetId> {
        self.focus
    }

    pub fn set_focus(&mut self, id: Option<WidgetId>) {
        if self.focus == id {
            return;
        }
        let old = self.focus;
        if let Some(old) = old {
            if let Some(widget) = self.widgets.get_mut(old.0) {
                widget.set_focused(false);
            }
        }
        self.focus = id;
        if let Some(new) = id {
            if let Some(widget) = self.widgets.get_mut(new.0) {
                widget.set_focused(true);
            }
        }
        if let Some(old) = old.and_then(|id| self.rect(id)) {
            self.mark_dirty_rect(old);
        }
        if let Some(new) = id.and_then(|id| self.rect(id)) {
            self.mark_dirty_rect(new);
        }
    }

    pub fn on_click<F: FnMut(&mut Ui) + 'static>(&mut self, id: WidgetId, callback: F) {
        if let Some(slot) = self.clicks.get_mut(id.0) {
            *slot = Some(Box::new(callback));
        }
    }

    /// Register a callback for edits or other value changes reported by a widget.
    pub fn on_change<F: FnMut(&mut Ui) + 'static>(&mut self, id: WidgetId, callback: F) {
        if let Some(slot) = self.changes.get_mut(id.0) {
            *slot = Some(Box::new(callback));
        }
    }

    /// Register a secondary-click callback for a widget. The widget must return
    /// `EventResult::ContextRequested` for `UiEvent::Context`.
    pub fn on_context<F: FnMut(&mut Ui, i32, i32) + 'static>(&mut self, id: WidgetId, callback: F) {
        if let Some(slot) = self.contexts.get_mut(id.0) {
            *slot = Some(Box::new(callback));
        }
    }

    pub fn set_root_size(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if (width, height) == (self.root_w, self.root_h) {
            return;
        }
        self.root_w = width;
        self.root_h = height;
        let root = self.root;
        let _ = self.style(root, |style| {
            style.size = TSize {
                width: Dimension::length(width as f32),
                height: Dimension::length(height as f32),
            };
        });
        self.request_full_redraw();
    }

    pub fn request_full_redraw(&mut self) {
        self.dirty = true;
        self.dirty_rect = None;
        self.needs_layout = true;
    }

    pub fn dispatch(&mut self, event: &UiEvent) -> bool {
        if let UiEvent::Resize { width, height } = *event {
            self.set_root_size(width, height);
            return true;
        }

        if self.needs_layout {
            self.compute();
        }

        match *event {
            UiEvent::Down { x, y } | UiEvent::Context { x, y } => {
                self.dismiss_overlays_outside(x, y);
            }
            _ => {}
        }

        if let UiEvent::Context { x, y } = *event {
            let Some(id) = self.hit_test(x, y) else { return false; };
            let result = self.widgets[id.0].event(event, self.focus == Some(id));
            if result != EventResult::Ignored || self.widgets[id.0].dirty() {
                self.mark_dirty_rect(self.widget_dirty_rect(id));
            }
            if result == EventResult::ContextRequested {
                let mut callback = self.contexts[id.0].take();
                if let Some(callback) = callback.as_mut() {
                    callback(self, x, y);
                }
                self.contexts[id.0] = callback;
                return true;
            }
            return result != EventResult::Ignored;
        }

        let mut clicked = None;
        let mut changed = None;
        let handled = self.dispatch_inner(event, &mut clicked, &mut changed);
        if let Some(id) = clicked {
            let mut callback = self.clicks[id.0].take();
            if let Some(callback) = callback.as_mut() {
                callback(self);
            }
            self.clicks[id.0] = callback;
        }
        if let Some(id) = changed {
            let mut callback = self.changes[id.0].take();
            if let Some(callback) = callback.as_mut() {
                callback(self);
            }
            self.changes[id.0] = callback;
        }
        handled
    }

    /// Render one dirty frame directly into a PopugOS window.
    /// Returns `true` when pixels were presented.
    pub fn frame(&mut self, window: &mut Window) -> Result<bool, WindowError> {
        self.set_root_size(window.client_width(), window.client_height());

        if self.needs_layout {
            self.compute();
        }

        let mut needs_full_redraw = false;
        for i in 0..self.widgets.len() {
            if self.widgets[i].dirty() {
                if self.widgets[i].full_redraw_when_dirty() {
                    needs_full_redraw = true;
                } else {
                    self.mark_dirty_rect(self.widget_dirty_rect(WidgetId(i)));
                }
            }
        }
        if needs_full_redraw {
            self.dirty = true;
            self.dirty_rect = None;
        }

        if !self.dirty {
            return Ok(false);
        }

        let screen = Rect::new(0, 0, self.root_w, self.root_h);
        let dirty = self.dirty_rect.unwrap_or(screen).intersect(screen).unwrap_or(screen);
        draw::set_clip(window, Some(dirty));
        draw::fill_rect(window, dirty, self.theme.background);
        self.draw_node(self.root, window, dirty, 0, 0);
        for widget in &self.widgets {
            if widget.overlay_active() {
                widget.draw_overlay(window, &self.theme);
            }
        }
        draw::set_clip(window, None);
        window.present_rect(draw::to_rectangle(dirty))?;

        self.dirty = false;
        self.dirty_rect = None;
        for widget in &mut self.widgets {
            widget.clear_dirty();
        }
        Ok(true)
    }

    fn mark_dirty_rect(&mut self, rect: Rect) {
        self.dirty = true;
        self.dirty_rect = match self.dirty_rect {
            None => Some(rect),
            Some(old) => {
                let x0 = old.x.min(rect.x);
                let y0 = old.y.min(rect.y);
                let x1 = (old.x + old.w as i32).max(rect.x + rect.w as i32);
                let y1 = (old.y + old.h as i32).max(rect.y + rect.h as i32);
                Some(Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
            }
        };
    }

    fn widget_dirty_rect(&self, id: WidgetId) -> Rect {
        self.widgets[id.0]
            .dirty_region()
            .unwrap_or_else(|| self.widgets[id.0].rect())
    }

    fn compute(&mut self) {
        let root = self.root;
        let widgets = &self.widgets;
        let taffy = &mut self.taffy;
        let available = TSize {
            width: AvailableSpace::Definite(self.root_w as f32),
            height: AvailableSpace::Definite(self.root_h as f32),
        };

        let _ = taffy.compute_layout_with_measure(root, available, |inputs, _node, context, style| {
            taffy::compute_leaf_layout(
                inputs,
                style,
                |_value, _basis| 0.0,
                |known, available| match context.as_deref() {
                    Some(NodeCtx::Widget(id)) => {
                        let measured = widgets[id.0].measure(constraints_from_taffy(known, available));
                        TSize { width: measured.width, height: measured.height }
                    }
                    Some(NodeCtx::Panel) => TSize {
                        width: known.width.unwrap_or(0.0),
                        height: known.height.unwrap_or(0.0),
                    },
                    None => TSize::ZERO,
                },
            )
        });

        for clip in &mut self.widget_clips {
            *clip = None;
        }
        self.apply_layout_node(self.root, 0.0, 0.0, None);
        self.needs_layout = false;
        self.dirty = true;
        self.dirty_rect = None;
    }

    fn apply_layout_node(
        &mut self,
        node: NodeId,
        parent_x: f32,
        parent_y: f32,
        inherited_clip: Option<Rect>,
    ) {
        let Ok(layout) = self.taffy.layout(node).copied() else {
            return;
        };
        let context = self.taffy.get_node_context(node).copied();
        let x = parent_x + layout.location.x;
        let y = parent_y + layout.location.y;
        let rect = Rect::new(
            x.round() as i32,
            y.round() as i32,
            layout.size.width.max(0.0).round() as u32,
            layout.size.height.max(0.0).round() as u32,
        );

        if let Some(NodeCtx::Widget(id)) = context {
            self.widgets[id.0].set_rect(rect);
            self.widget_clips[id.0] = inherited_clip;
        }

        let child_count = self.taffy.child_count(node);
        for index in 0..child_count {
            let Ok(child) = self.taffy.child_at_index(node, index) else {
                break;
            };
            self.apply_layout_node(child, rect.x as f32, rect.y as f32, inherited_clip);
        }
    }

    fn dispatch_inner(&mut self, event: &UiEvent, clicked: &mut Option<WidgetId>, changed: &mut Option<WidgetId>) -> bool {
        match *event {
            UiEvent::Down { x, y } => {
                let target = self.hit_test(x, y);
                self.hovered = target;
                self.pressed = target;
                if let Some(id) = target {
                    if self.widgets[id.0].focusable() {
                        self.set_focus(Some(id));
                    }
                    let result = self.widgets[id.0].event(event, self.focus == Some(id));
                    if result != EventResult::Ignored || self.widgets[id.0].dirty() {
                        self.mark_dirty_rect(self.widget_dirty_rect(id));
                    }
                    if result == EventResult::Clicked {
                        *clicked = Some(id);
                    }
                    if result == EventResult::Changed { *changed = Some(id); }
                    true
                } else {
                    false
                }
            }
            UiEvent::Move { x, y } => {
                let target = self.hit_test(x, y);
                let old = self.hovered;
                self.hovered = target;
                let receiver = self.pressed.or(target);
                let mut handled = old != target;
                if old != target {
                    if let Some(id) = old {
                        let leave = UiEvent::Leave;
                        let result = self.widgets[id.0].event(&leave, self.focus == Some(id));
                        if result != EventResult::Ignored || self.widgets[id.0].dirty() {
                            self.mark_dirty_rect(self.widget_dirty_rect(id));
                            handled = true;
                        }
                    }
                }
                if let Some(id) = receiver {
                    let result = self.widgets[id.0].event(event, self.focus == Some(id));
                    if result == EventResult::Changed { *changed = Some(id); }
                    if result != EventResult::Ignored || self.widgets[id.0].dirty() {
                        self.mark_dirty_rect(self.widget_dirty_rect(id));
                        handled = true;
                    }
                }
                handled
            }
            UiEvent::Leave => {
                let old = self.hovered.take();
                if let Some(id) = old {
                    let result = self.widgets[id.0].event(event, self.focus == Some(id));
                    if result != EventResult::Ignored || self.widgets[id.0].dirty() {
                        self.mark_dirty_rect(self.widget_dirty_rect(id));
                    }
                    true
                } else {
                    false
                }
            }
            UiEvent::Wheel { x, y, .. } => {
                let Some(id) = self.hit_test(x, y) else { return false; };
                let result = self.widgets[id.0].event(event, self.focus == Some(id));
                if result == EventResult::Changed { *changed = Some(id); }
                if result != EventResult::Ignored || self.widgets[id.0].dirty() {
                    self.mark_dirty_rect(self.widget_dirty_rect(id));
                }
                result != EventResult::Ignored
            }
            UiEvent::Up { x, y } => {
                let id = self.pressed.take().or_else(|| self.hit_test(x, y));
                self.hovered = self.hit_test(x, y);
                if let Some(id) = id {
                    let result = self.widgets[id.0].event(event, self.focus == Some(id));
                    if result == EventResult::Clicked {
                        *clicked = Some(id);
                    }
                    if result == EventResult::Changed { *changed = Some(id); }
                    if result != EventResult::Ignored || self.widgets[id.0].dirty() {
                        self.mark_dirty_rect(self.widget_dirty_rect(id));
                    }
                    true
                } else {
                    false
                }
            }
            UiEvent::KeyDown { .. } | UiEvent::KeyUp { .. } => {
                if let Some(id) = self.focus {
                    let result = self.widgets[id.0].event(event, true);
                    if result == EventResult::Submitted
                        && self.clicks.get(id.0).map(|entry| entry.is_some()).unwrap_or(false)
                    {
                        *clicked = Some(id);
                    }
                    if result == EventResult::Changed { *changed = Some(id); }
                    if result != EventResult::Ignored || self.widgets[id.0].dirty() {
                        self.mark_dirty_rect(self.widget_dirty_rect(id));
                    }
                    result != EventResult::Ignored
                } else {
                    false
                }
            }
            UiEvent::Context { .. } => false,
            UiEvent::Resize { .. } => true,
        }
    }

    fn dismiss_overlays_outside(&mut self, x: i32, y: i32) {
        let mut changed = false;
        for widget in &mut self.widgets {
            if widget.overlay_active()
                && !widget.overlay_contains(x, y)
                && !widget.rect().contains(x, y)
            {
                widget.dismiss_overlay();
                changed = true;
            }
        }
        if changed {
            self.dirty = true;
            self.dirty_rect = None;
        }
    }

    fn hit_test(&self, x: i32, y: i32) -> Option<WidgetId> {
        // Active popup/menu overlays always sit above the normal Taffy tree.
        for index in (0..self.widgets.len()).rev() {
            if self.widgets[index].overlay_active() && self.widgets[index].overlay_contains(x, y) {
                return Some(WidgetId(index));
            }
        }

        for index in (0..self.widgets.len()).rev() {
            let rect = self.widgets[index].rect();
            if !rect.contains(x, y) {
                continue;
            }
            if let Some(clip) = self.widget_clips[index] {
                if !clip.contains(x, y) {
                    continue;
                }
            }
            return Some(WidgetId(index));
        }
        None
    }

    fn draw_node(
        &self,
        node: NodeId,
        window: &mut Window,
        dirty: Rect,
        parent_x: i32,
        parent_y: i32,
    ) {
        let Ok(layout) = self.taffy.layout(node) else {
            return;
        };
        let context = self.taffy.get_node_context(node).copied();
        let node_rect = Rect::new(
            parent_x + layout.location.x.round() as i32,
            parent_y + layout.location.y.round() as i32,
            layout.size.width.max(0.0).round() as u32,
            layout.size.height.max(0.0).round() as u32,
        );

        if let Some(NodeCtx::Panel) = context {
            if node_rect.intersect(dirty).is_some() {
                draw::fill_rect(window, node_rect, self.theme.panel_background);
            }
        }

        if let Some(NodeCtx::Widget(id)) = context {
            let rect = self.widgets[id.0].rect();
            if rect.intersect(dirty).is_none() {
                return;
            }
            let clip = self.widget_clips[id.0]
                .and_then(|clip| clip.intersect(dirty))
                .or(Some(dirty));
            draw::set_clip(window, clip);
            self.widgets[id.0].draw(window, &self.theme);
            draw::set_clip(window, Some(dirty));
            return;
        }

        for child in self.taffy.child_ids(node) {
            self.draw_node(child, window, dirty, node_rect.x, node_rect.y);
        }
    }
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

fn constraints_from_taffy(
    known: TSize<Option<f32>>,
    available: TSize<AvailableSpace>,
) -> Constraints {
    fn max(space: AvailableSpace) -> f32 {
        match space {
            AvailableSpace::Definite(value) => value,
            AvailableSpace::MinContent => 0.0,
            AvailableSpace::MaxContent => f32::INFINITY,
        }
    }

    Constraints {
        min_width: known.width.unwrap_or(0.0),
        max_width: max(available.width),
        min_height: known.height.unwrap_or(0.0),
        max_height: max(available.height),
    }
}
