//! Retained-mode UI built around Taffy.
//!
//! The library is intentionally split into three layers:
//! - [`layout`] owns the Taffy tree, containers, styles and constraints.
//! - [`widget`] defines the common leaf-widget contract.
//! - [`widgets`] contains concrete controls.
//!
//! Containers are layout nodes, not widgets. Only leaves implement [`Widget`].

use micromath::F32Ext;
mod geometry;
pub mod layout;
pub mod theme;
mod widget;
pub mod widgets;

use alloc::{boxed::Box, vec::Vec};
use embedded_graphics::{
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
};
use taffy::{
    geometry::Size as TSize,
    prelude::{AvailableSpace, Dimension, LengthPercentage as LP, LengthPercentageAuto as LPA},
    tree::TaffyTree,
    Overflow, TraversePartialTree,
};

use crate::wm::{
    Window, WmEvent, EV_KEY_DOWN, EV_KEY_UP, EV_MOUSE_DOWN, EV_MOUSE_MOVE, EV_MOUSE_UP,
};
pub use geometry::{Constraints, Rect};
pub use taffy::prelude::{
    AlignContent, AlignItems, FlexDirection, JustifyContent, Position, Style,
};
pub use taffy::tree::NodeId;
pub use theme::{
    BG, BTN_BG, BTN_BG_DOWN, BTN_BG_HOT, BTN_BORDER, INPUT_BG, INPUT_BORDER, INPUT_BORDER_FOCUS,
    LABEL_FG, PANEL_BG, PANEL_BORDER, SCROLLBAR_BG, SCROLLBAR_THUMB, TEXT,
};
pub use widget::{EventResult, Widget};
pub use widgets::{Button, Label, TextInput};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiEvent {
    Down { x: i32, y: i32 },
    Move { x: i32, y: i32 },
    Up { x: i32, y: i32 },
    KeyDown { scancode: u8, ch: u8, mods: u8 },
    KeyUp { scancode: u8, ch: u8, mods: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WidgetId(usize);
impl WidgetId {
    pub const fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScrollViewId(usize);
impl ScrollViewId {
    pub const fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy)]
enum NodeCtx {
    Widget(WidgetId),
    Panel,
    ScrollView(ScrollViewId),
    ScrollContent(ScrollViewId),
}

#[derive(Clone, Copy, Debug, Default)]
struct ScrollState {
    offset_y: f32,
    content_height: f32,
    viewport_height: f32,
    dragging: bool,
    drag_grab: f32,
}

impl ScrollState {
    fn max_offset(self) -> f32 {
        (self.content_height - self.viewport_height).max(0.0)
    }

    fn set_offset(&mut self, y: f32) {
        self.offset_y = y.max(0.0).min(self.max_offset());
    }
}

type ClickHandler = Box<dyn FnMut(&mut Ui)>;

pub struct Ui {
    taffy: TaffyTree<NodeCtx>,
    widgets: Vec<Box<dyn Widget>>,
    widget_nodes: Vec<NodeId>,
    widget_clips: Vec<Option<Rect>>,
    clicks: Vec<Option<ClickHandler>>,
    scrolls: Vec<ScrollState>,
    scroll_nodes: Vec<NodeId>,
    scroll_contents: Vec<NodeId>,
    scroll_rects: Vec<Rect>,
    root: NodeId,
    root_w: u32,
    root_h: u32,
    focus: Option<WidgetId>,
    dirty: bool,
    needs_layout: bool,
}

impl Ui {
    pub fn new() -> Self {
        Self::with_size(1, 1)
    }

    pub fn with_size(w: u32, h: u32) -> Self {
        let mut taffy = TaffyTree::new();
        let mut style = Style::default();
        style.flex_direction = FlexDirection::Column;
        style.size = TSize {
            width: Dimension::length(w.max(1) as f32),
            height: Dimension::length(h.max(1) as f32),
        };
        let root = taffy.new_leaf(style).expect("taffy: create root");
        Self {
            taffy,
            widgets: Vec::new(),
            widget_nodes: Vec::new(),
            widget_clips: Vec::new(),
            clicks: Vec::new(),
            scrolls: Vec::new(),
            scroll_nodes: Vec::new(),
            scroll_contents: Vec::new(),
            scroll_rects: Vec::new(),
            root,
            root_w: w.max(1),
            root_h: h.max(1),
            focus: None,
            dirty: true,
            needs_layout: true,
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn root_node(&self) -> NodeId {
        self.root
    }

    pub fn root_size(&self) -> (u32, u32) {
        (self.root_w, self.root_h)
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
        if self.taffy.set_style(node, style).is_ok() {
            self.needs_layout = true;
            true
        } else {
            false
        }
    }

    pub fn column(&mut self, parent: NodeId) -> NodeId {
        self.container(parent, FlexDirection::Column)
    }

    pub fn row(&mut self, parent: NodeId) -> NodeId {
        self.container(parent, FlexDirection::Row)
    }

    pub fn flex(&mut self, parent: NodeId, direction: FlexDirection) -> NodeId {
        self.container(parent, direction)
    }

    fn container(&mut self, parent: NodeId, direction: FlexDirection) -> NodeId {
        let mut s = Style::default();
        s.flex_direction = direction;
        let n = self.taffy.new_leaf(s).expect("taffy: create container");
        self.taffy
            .add_child(parent, n)
            .expect("taffy: add container");
        self.needs_layout = true;
        n
    }

    pub fn panel(&mut self, parent: NodeId) -> NodeId {
        let mut s = Style::default();
        s.flex_direction = FlexDirection::Column;
        s.padding = taffy::geometry::Rect {
            left: LP::length(8.0),
            right: LP::length(8.0),
            top: LP::length(8.0),
            bottom: LP::length(8.0),
        };
        let n = self
            .taffy
            .new_leaf_with_context(s, NodeCtx::Panel)
            .expect("taffy: create panel");
        self.taffy.add_child(parent, n).expect("taffy: add panel");
        self.needs_layout = true;
        n
    }

    pub fn spacer(&mut self, parent: NodeId) -> NodeId {
        let mut s = Style::default();
        s.flex_grow = 1.0;
        let n = self.taffy.new_leaf(s).expect("taffy: create spacer");
        self.taffy.add_child(parent, n).expect("taffy: add spacer");
        self.needs_layout = true;
        n
    }

    pub fn scroll_view(&mut self, parent: NodeId) -> ScrollViewId {
        let sid = ScrollViewId(self.scrolls.len());
        let mut viewport = Style::default();
        viewport.flex_direction = FlexDirection::Column;
        viewport.min_size.height = LPA::length(0.0);
        viewport.overflow = taffy::geometry::Point {
            x: Overflow::Hidden,
            y: Overflow::Hidden,
        };
        let node = self
            .taffy
            .new_leaf_with_context(viewport, NodeCtx::ScrollView(sid))
            .expect("taffy: create scroll view");
        let mut content = Style::default();
        content.flex_direction = FlexDirection::Column;
        content.flex_shrink = 0.0;
        content.min_size.height = LPA::length(0.0);
        content.size.width = Dimension::percent(1.0);
        let content_node = self
            .taffy
            .new_leaf_with_context(content, NodeCtx::ScrollContent(sid))
            .expect("taffy: create scroll content");
        self.taffy
            .add_child(node, content_node)
            .expect("taffy: add scroll content");
        self.taffy
            .add_child(parent, node)
            .expect("taffy: add scroll view");
        self.scrolls.push(ScrollState::default());
        self.scroll_nodes.push(node);
        self.scroll_contents.push(content_node);
        self.scroll_rects.push(Rect::default());
        self.needs_layout = true;
        sid
    }

    pub fn scroll_content(&self, scroll: ScrollViewId) -> Option<NodeId> {
        self.scroll_contents.get(scroll.0).copied()
    }

    pub fn scroll_offset(&self, scroll: ScrollViewId) -> Option<f32> {
        self.scrolls.get(scroll.0).map(|s| s.offset_y)
    }

    pub fn scroll_max_offset(&self, scroll: ScrollViewId) -> Option<f32> {
        self.scrolls.get(scroll.0).map(|s| s.max_offset())
    }

    pub fn scroll_by(&mut self, scroll: ScrollViewId, dy: f32) -> bool {
        let Some(s) = self.scrolls.get_mut(scroll.0) else {
            return false;
        };
        let old = s.offset_y;
        s.set_offset(old + dy);
        if old != s.offset_y {
            self.dirty = true;
            true
        } else {
            false
        }
    }

    pub fn scroll_to(&mut self, scroll: ScrollViewId, y: f32) -> bool {
        let Some(s) = self.scrolls.get_mut(scroll.0) else {
            return false;
        };
        let old = s.offset_y;
        s.set_offset(y);
        if old != s.offset_y {
            self.dirty = true;
            true
        } else {
            false
        }
    }

    pub fn scroll_to_top(&mut self, scroll: ScrollViewId) -> bool {
        self.scroll_to(scroll, 0.0)
    }

    pub fn scroll_to_bottom(&mut self, scroll: ScrollViewId) -> bool {
        self.scroll_max_offset(scroll)
            .map(|y| self.scroll_to(scroll, y))
            .unwrap_or(false)
    }

    pub fn button(&mut self, parent: NodeId, label: &str) -> WidgetId {
        let mut s = Style::default();
        s.justify_content = Some(JustifyContent::CENTER);
        s.align_items = Some(AlignItems::CENTER);
        s.padding = taffy::geometry::Rect {
            left: LP::length(12.0),
            right: LP::length(12.0),
            top: LP::length(5.0),
            bottom: LP::length(5.0),
        };
        self.add_widget(parent, Box::new(Button::new(label)), s)
    }

    pub fn label(&mut self, parent: NodeId, text: &str) -> WidgetId {
        self.add_widget(parent, Box::new(Label::new(text)), Style::default())
    }

    pub fn text_input(&mut self, parent: NodeId) -> WidgetId {
        self.text_input_with(parent, "")
    }

    pub fn text_input_with(&mut self, parent: NodeId, text: &str) -> WidgetId {
        let mut s = Style::default();
        s.align_items = Some(AlignItems::CENTER);
        s.min_size = TSize {
            width: LPA::auto(),
            height: LPA::length(26.0),
        };
        s.padding = taffy::geometry::Rect {
            left: LP::length(6.0),
            right: LP::length(6.0),
            top: LP::length(4.0),
            bottom: LP::length(4.0),
        };
        self.add_widget(parent, Box::new(TextInput::new(text)), s)
    }

    fn add_widget(&mut self, parent: NodeId, widget: Box<dyn Widget>, style: Style) -> WidgetId {
        let id = WidgetId(self.widgets.len());
        let node = self
            .taffy
            .new_leaf_with_context(style, NodeCtx::Widget(id))
            .expect("taffy: create widget");
        self.taffy
            .add_child(parent, node)
            .expect("taffy: add widget");
        self.widgets.push(widget);
        self.widget_nodes.push(node);
        self.widget_clips.push(None);
        self.clicks.push(None);
        self.needs_layout = true;
        id
    }

    pub fn button_mut(&mut self, id: WidgetId) -> Option<&mut Button> {
        self.widgets
            .get_mut(id.0)?
            .as_any_mut()
            .downcast_mut::<Button>()
    }

    pub fn label_mut(&mut self, id: WidgetId) -> Option<&mut Label> {
        self.widgets
            .get_mut(id.0)?
            .as_any_mut()
            .downcast_mut::<Label>()
    }

    pub fn text_input_mut(&mut self, id: WidgetId) -> Option<&mut TextInput> {
        self.widgets
            .get_mut(id.0)?
            .as_any_mut()
            .downcast_mut::<TextInput>()
    }

    pub fn text(&self, id: WidgetId) -> Option<&str> {
        let w = self.widgets.get(id.0)?;
        w.as_any()
            .downcast_ref::<TextInput>()
            .map(|v| v.text())
            .or_else(|| w.as_any().downcast_ref::<Label>().map(|v| v.text()))
    }

    pub fn set_label(&mut self, id: WidgetId, text: &str) {
        if let Some(v) = self.label_mut(id) {
            v.set_text(text);
            self.needs_layout = true;
        }
    }

    pub fn set_button_label(&mut self, id: WidgetId, text: &str) {
        if let Some(v) = self.button_mut(id) {
            v.set_label(text);
            self.needs_layout = true;
        }
    }

    pub fn set_text(&mut self, id: WidgetId, text: &str) {
        if let Some(v) = self.text_input_mut(id) {
            v.set_text(text);
            self.needs_layout = true;
        }
    }

    pub fn on_click<F: FnMut(&mut Ui) + 'static>(&mut self, id: WidgetId, f: F) {
        if let Some(slot) = self.clicks.get_mut(id.0) {
            *slot = Some(Box::new(f));
        }
    }

    pub fn rect(&self, id: WidgetId) -> Option<Rect> {
        self.widgets.get(id.0).map(|w| w.rect())
    }

    pub fn node_of(&self, id: WidgetId) -> Option<NodeId> {
        self.widget_nodes.get(id.0).copied()
    }

    pub fn focus(&self) -> Option<WidgetId> {
        self.focus
    }

    pub fn set_focus(&mut self, id: Option<WidgetId>) {
        if self.focus == id {
            return;
        }
        if let Some(old) = self.focus {
            if let Some(w) = self.widgets.get_mut(old.0) {
                w.set_focused(false);
            }
        }
        self.focus = id;
        if let Some(new) = id {
            if let Some(w) = self.widgets.get_mut(new.0) {
                w.set_focused(true);
            }
        }
        self.dirty = true;
    }

    pub fn request_full_redraw(&mut self) {
        self.dirty = true;
        self.needs_layout = true;
    }

    pub fn process(&mut self, win: &mut Window) {
        let mut buf = [WmEvent::default(); 256];
        let n = win.poll_events(&mut buf);
        let mut events = Vec::new();
        for e in &buf[..n] {
            match e.kind {
                EV_MOUSE_DOWN => events.push(UiEvent::Down { x: e.a, y: e.b }),
                EV_MOUSE_UP => events.push(UiEvent::Up { x: e.a, y: e.b }),
                EV_MOUSE_MOVE => events.push(UiEvent::Move { x: e.a, y: e.b }),
                EV_KEY_DOWN => events.push(UiEvent::KeyDown {
                    scancode: e.a as u8,
                    ch: e.b as u8,
                    mods: e.c as u8,
                }),
                EV_KEY_UP => events.push(UiEvent::KeyUp {
                    scancode: e.a as u8,
                    ch: e.b as u8,
                    mods: e.c as u8,
                }),
                _ => {}
            }
        }
        self.set_root_size(win.client_width().max(1), win.client_height().max(1));
        if self.needs_layout {
            self.compute();
            self.needs_layout = false;
        }
        let mut clicked = Vec::new();
        for ev in &events {
            if self.dispatch(ev, &mut clicked) {
                self.dirty = true;
            }
        }
        for id in clicked {
            let mut cb = self.clicks[id.0].take();
            if let Some(f) = cb.as_mut() {
                f(self);
            }
            self.clicks[id.0] = cb;
        }
        if self.needs_layout || self.widgets.iter().any(|w| w.dirty()) {
            self.compute();
            self.needs_layout = false;
            self.dirty = true;
        }
        if self.dirty {
            self.draw(win);
            let _ = win.flip();
            self.dirty = false;
            for w in &mut self.widgets {
                w.clear_dirty();
            }
        }
    }

    pub fn set_root_size(&mut self, w: u32, h: u32) {
        let w = w.max(1);
        let h = h.max(1);
        if w == self.root_w && h == self.root_h {
            return;
        }
        self.root_w = w;
        self.root_h = h;
        let _ = self.style(self.root, |s| {
            s.size = TSize {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
            }
        });
    }

    fn compute(&mut self) {
        let root = self.root;
        let rw = self.root_w;
        let rh = self.root_h;
        let widgets = &self.widgets;
        let taffy = &mut self.taffy;
        let space = TSize {
            width: AvailableSpace::Definite(rw as f32),
            height: AvailableSpace::Definite(rh as f32),
        };
        let _ = taffy.compute_layout_with_measure(
            root,
            space,
            |inputs, _node, node_context, style| {
                taffy::compute_leaf_layout(
                    inputs,
                    style,
                    |_value, _basis| 0.0,
                    |known, available| match node_context.as_deref() {
                        Some(NodeCtx::Widget(id)) => {
                            widgets[id.0].measure(constraints_from_taffy(known, available))
                        }
                        _ => TSize::ZERO,
                    },
                )
            },
        );
        self.update_scroll_metrics();
        self.apply_layout();
    }

    fn update_scroll_metrics(&mut self) {
        for i in 0..self.scrolls.len() {
            let v = self.taffy.layout(self.scroll_nodes[i]).ok().copied();
            let c = self.taffy.layout(self.scroll_contents[i]).ok().copied();
            if let (Some(v), Some(c)) = (v, c) {
                self.scrolls[i].viewport_height = v.size.height;
                self.scrolls[i].content_height = c.size.height;
                let off = self.scrolls[i].offset_y;
                self.scrolls[i].set_offset(off);
            }
        }
    }

    fn apply_layout(&mut self) {
        for clip in &mut self.widget_clips {
            *clip = None;
        }
        for rect in &mut self.scroll_rects {
            *rect = Rect::default();
        }
        self.apply_layout_node(self.root, 0.0, 0.0, None);
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
        let ctx = self.taffy.get_node_context(node).copied();
        let mut x = parent_x + layout.location.x;
        let mut y = parent_y + layout.location.y;

        let rect = Rect::new(
            x.round() as i32,
            y.round() as i32,
            layout.size.width.max(0.0).round() as u32,
            layout.size.height.max(0.0).round() as u32,
        );

        let mut clip = inherited_clip;
        if let Some(NodeCtx::ScrollView(sid)) = ctx {
            self.scroll_rects[sid.0] = rect;
            clip = match clip {
                Some(existing) => existing.intersect(rect),
                None => Some(rect),
            };
        }

        if let Some(NodeCtx::ScrollContent(sid)) = ctx {
            y -= self.scrolls[sid.0].offset_y;
        }

        if let Some(NodeCtx::Widget(id)) = ctx {
            let widget_rect = Rect::new(
                x.round() as i32,
                y.round() as i32,
                layout.size.width.max(0.0).round() as u32,
                layout.size.height.max(0.0).round() as u32,
            );
            self.widgets[id.0].set_rect(widget_rect);
            self.widget_clips[id.0] = clip;
        }

        let children: Vec<NodeId> = self.taffy.child_ids(node).collect();
        for child in children {
            self.apply_layout_node(child, x, y, clip);
        }
    }

    fn dispatch(&mut self, event: &UiEvent, clicked: &mut Vec<WidgetId>) -> bool {
        match event {
            UiEvent::Down { x, y } => {
                if let Some((sid, grab)) = self.scrollbar_at(*x, *y) {
                    if let Some(s) = self.scrolls.get_mut(sid.0) {
                        s.dragging = true;
                        s.drag_grab = grab;
                    }
                    return true;
                }
                let target = self.hit_test(*x, *y);
                if let Some(id) = target {
                    if self.widgets[id.0].focusable() {
                        self.set_focus(Some(id));
                    }
                    let r = self.widgets[id.0].event(event, self.focus == Some(id));
                    if r == EventResult::Clicked {
                        clicked.push(id);
                    }
                    return r != EventResult::Ignored;
                }
                false
            }
            UiEvent::Move { x, y } => {
                let mut changed = false;
                for i in 0..self.scrolls.len() {
                    if self.scrolls[i].dragging {
                        let sid = ScrollViewId(i);
                        let max = self.scrolls[i].max_offset();
                        let v = self.scroll_rects[i];
                        let track_h = v.h as f32;
                        let thumb_h = (track_h
                            * (self.scrolls[i].viewport_height
                                / self.scrolls[i]
                                    .content_height
                                    .max(self.scrolls[i].viewport_height)))
                        .max(12.0)
                        .min(track_h);
                        let rel = (*y as f32 - v.y as f32 - self.scrolls[i].drag_grab)
                            .max(0.0)
                            .min((track_h - thumb_h).max(0.0));
                        let off = if track_h <= thumb_h {
                            0.0
                        } else {
                            rel / (track_h - thumb_h) * max
                        };
                        changed |= self.scroll_to(sid, off);
                    }
                }
                if let Some(id) = self.hit_test(*x, *y) {
                    let r = self.widgets[id.0].event(event, self.focus == Some(id));
                    changed |= r != EventResult::Ignored;
                }
                changed
            }
            UiEvent::Up { x, y } => {
                let mut changed = false;
                for s in &mut self.scrolls {
                    changed |= s.dragging;
                    s.dragging = false;
                }
                if let Some(id) = self.hit_test(*x, *y) {
                    let r = self.widgets[id.0].event(event, self.focus == Some(id));
                    changed |= r != EventResult::Ignored;
                }
                changed
            }
            UiEvent::KeyDown { .. } | UiEvent::KeyUp { .. } => {
                if let Some(id) = self.focus {
                    let r = self.widgets[id.0].event(event, true);
                    if r == EventResult::Submitted {
                        if let Some(cb) = self.clicks.get_mut(id.0) {
                            if cb.is_some() {
                                clicked.push(id);
                            }
                        }
                    }
                    r != EventResult::Ignored
                } else {
                    false
                }
            }
        }
    }

    fn hit_test(&self, x: i32, y: i32) -> Option<WidgetId> {
        for i in (0..self.widgets.len()).rev() {
            let r = self.widgets[i].rect();
            if !r.contains(x, y) {
                continue;
            }
            if let Some(clip) = self.widget_clips[i] {
                if !clip.contains(x, y) {
                    continue;
                }
            }
            return Some(WidgetId(i));
        }
        None
    }

    fn scrollbar_at(&self, x: i32, y: i32) -> Option<(ScrollViewId, f32)> {
        for i in (0..self.scrolls.len()).rev() {
            let v = self.scroll_rects[i];
            if v.w == 0 || v.h == 0 {
                continue;
            }
            let track_w = 10;
            let xr = v.x + v.w as i32 - track_w;
            if x >= xr && x <= v.x + v.w as i32 && y >= v.y && y <= v.y + v.h as i32 {
                let track_h = v.h as f32;
                let thumb_h = (track_h
                    * (self.scrolls[i].viewport_height
                        / self.scrolls[i]
                            .content_height
                            .max(self.scrolls[i].viewport_height)))
                .max(12.0)
                .min(track_h);
                let rel = (y as f32 - v.y as f32).max(0.0).min(track_h);
                let grab = (rel - thumb_h * 0.5)
                    .max(0.0)
                    .min((track_h - thumb_h).max(0.0));
                return Some((ScrollViewId(i), grab));
            }
        }
        None
    }

    fn draw(&self, win: &mut Window) {
        let _ = Rectangle::new(Point::new(0, 0), Size::new(self.root_w, self.root_h))
            .into_styled(PrimitiveStyle::with_fill(BG))
            .draw(win);
        self.draw_node(self.root, win);
        self.draw_scrollbars(win);
    }

    fn draw_node(&self, node: NodeId, win: &mut Window) {
        if let Some(ctx) = self.taffy.get_node_context(node) {
            if let NodeCtx::Widget(id) = ctx {
                let r = self.widgets[id.0].rect();
                let visible = r.intersect(Rect::new(0, 0, self.root_w, self.root_h)).is_some()
                    && self.widget_clips[id.0]
                        .map(|clip| r.intersect(clip).is_some())
                        .unwrap_or(true);
                if visible {
                    self.widgets[id.0].draw(win);
                }
                return;
            }
        }
        let children: Vec<NodeId> = self.taffy.child_ids(node).collect();
        for child in children {
            self.draw_node(child, win);
        }
    }

    fn draw_scrollbars(&self, win: &mut Window) {
        for i in 0..self.scrolls.len() {
            if self.scrolls[i].content_height <= self.scrolls[i].viewport_height {
                continue;
            }
            let v = self.scroll_rects[i];
            if v.w == 0 || v.h == 0 {
                continue;
            }
            let x = v.x + v.w as i32 - 10;
            let y = v.y;
            let h = v.h as i32;
            let thumb_h = ((h as f32)
                * (self.scrolls[i].viewport_height / self.scrolls[i].content_height))
                .max(12.0) as i32;
            let travel = (h - thumb_h).max(0);
            let top = y + if self.scrolls[i].max_offset() <= 0.0 {
                0
            } else {
                ((self.scrolls[i].offset_y / self.scrolls[i].max_offset()) * travel as f32)
                    as i32
            };
            let _ = Rectangle::new(Point::new(x, y), Size::new(10, h as u32))
                .into_styled(PrimitiveStyle::with_fill(SCROLLBAR_BG))
                .draw(win);
            let _ = Rectangle::new(Point::new(x, top), Size::new(10, thumb_h as u32))
                .into_styled(PrimitiveStyle::with_fill(SCROLLBAR_THUMB))
                .draw(win);
        }
    }
}

fn constraints_from_taffy(
    known: TSize<Option<f32>>,
    available: TSize<AvailableSpace>,
) -> Constraints {
    fn max(v: AvailableSpace) -> f32 {
        match v {
            AvailableSpace::Definite(v) => v,
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

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

impl Rect {
    fn from_taffy(l: &taffy::tree::Layout) -> Self {
        Self::new(
            l.location.x.round() as i32,
            l.location.y.round() as i32,
            l.size.width.max(0.0).round() as u32,
            l.size.height.max(0.0).round() as u32,
        )
    }
}
