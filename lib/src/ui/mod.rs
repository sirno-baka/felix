//! Retained-mode UI built around Taffy.
//!
//! The library is intentionally split into three layers:
//! - [`layout`] owns the Taffy tree, containers, styles and constraints.
//! - [`widget`] defines the common leaf-widget contract.
//! - [`widgets`] contains concrete controls.
//!
//! Containers are layout nodes, not widgets. Only leaves implement [`Widget`].

mod geometry;
mod widget;
pub mod layout;
pub mod theme;
pub mod widgets;

use alloc::{boxed::Box, vec::Vec};
use embedded_graphics::{pixelcolor::Rgb888, prelude::*, primitives::{PrimitiveStyle, Rectangle}};
use taffy::{geometry::Size as TSize, prelude::{AvailableSpace, Dimension, LengthPercentage as LP, LengthPercentageAuto as LPA}, tree::TaffyTree, Overflow};

use crate::wm::{Window, WmEvent, EV_KEY_DOWN, EV_KEY_UP, EV_MOUSE_DOWN, EV_MOUSE_MOVE, EV_MOUSE_UP};
pub use geometry::{Constraints, Rect};
pub use widget::{EventResult, Widget};
pub use widgets::{Button, Label, TextInput};
pub use taffy::prelude::{AlignContent, AlignItems, FlexDirection, JustifyContent, Position, Style};
pub use taffy::tree::NodeId;
pub use theme::{BG, BTN_BG, BTN_BG_DOWN, BTN_BG_HOT, BTN_BORDER, INPUT_BG, INPUT_BORDER, INPUT_BORDER_FOCUS, LABEL_FG, PANEL_BG, PANEL_BORDER, SCROLLBAR_BG, SCROLLBAR_THUMB, TEXT};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiEvent { Down { x: i32, y: i32 }, Move { x: i32, y: i32 }, Up { x: i32, y: i32 }, KeyDown { scancode: u8, ch: u8, mods: u8 }, KeyUp { scancode: u8, ch: u8, mods: u8 } }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WidgetId(usize);
impl WidgetId { pub const fn index(self) -> usize { self.0 } }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScrollViewId(usize);
impl ScrollViewId { pub const fn index(self) -> usize { self.0 } }

#[derive(Clone, Copy)]
enum NodeCtx { Widget(WidgetId), Panel, ScrollView(ScrollViewId), ScrollContent(ScrollViewId) }
#[derive(Clone, Copy, Debug, Default)]
struct ScrollState { offset_y: f32, content_height: f32, viewport_height: f32, dragging: bool, drag_grab: f32 }
impl ScrollState { fn max_offset(self) -> f32 { (self.content_height - self.viewport_height).max(0.0) } fn set_offset(&mut self, y: f32) { self.offset_y = y.max(0.0).min(self.max_offset()); } }
type ClickHandler = Box<dyn FnMut(&mut Ui)>;

pub struct Ui {
    taffy: TaffyTree<NodeCtx>, widgets: Vec<Box<dyn Widget>>, widget_nodes: Vec<NodeId>, clicks: Vec<Option<ClickHandler>>,
    scrolls: Vec<ScrollState>, scroll_nodes: Vec<NodeId>, scroll_contents: Vec<NodeId>, root: NodeId,
    root_w: u32, root_h: u32, focus: Option<WidgetId>, dirty: bool, needs_layout: bool,
}

impl Ui {
    pub fn new() -> Self { Self::with_size(1, 1) }
    pub fn with_size(w: u32, h: u32) -> Self {
        let mut taffy = TaffyTree::new(); let mut style = Style::default(); style.flex_direction = FlexDirection::Column;
        style.size = TSize { width: Dimension::length(w.max(1) as f32), height: Dimension::length(h.max(1) as f32) };
        let root = taffy.new_leaf(style).expect("taffy: create root");
        Self { taffy, widgets: Vec::new(), widget_nodes: Vec::new(), clicks: Vec::new(), scrolls: Vec::new(), scroll_nodes: Vec::new(), scroll_contents: Vec::new(), root, root_w: w.max(1), root_h: h.max(1), focus: None, dirty: true, needs_layout: true }
    }
    pub fn root(&self) -> NodeId { self.root }
    pub fn root_node(&self) -> NodeId { self.root }
    pub fn root_size(&self) -> (u32, u32) { (self.root_w, self.root_h) }
    pub fn style(&mut self, node: NodeId, update: impl FnOnce(&mut Style)) -> bool { let Some(mut style) = self.taffy.style(node).ok().cloned() else { return false }; update(&mut style); if self.taffy.set_style(node, style).is_ok() { self.needs_layout = true; true } else { false } }
    pub fn set_style(&mut self, node: NodeId, style: Style) -> bool { if self.taffy.set_style(node, style).is_ok() { self.needs_layout = true; true } else { false } }
    pub fn column(&mut self, parent: NodeId) -> NodeId { self.container(parent, FlexDirection::Column) }
    pub fn row(&mut self, parent: NodeId) -> NodeId { self.container(parent, FlexDirection::Row) }
    pub fn flex(&mut self, parent: NodeId, direction: FlexDirection) -> NodeId { self.container(parent, direction) }
    fn container(&mut self, parent: NodeId, direction: FlexDirection) -> NodeId { let mut s = Style::default(); s.flex_direction = direction; let n = self.taffy.new_leaf(s).expect("taffy: create container"); self.taffy.add_child(parent, n).expect("taffy: add container"); self.needs_layout = true; n }
    pub fn panel(&mut self, parent: NodeId) -> NodeId { let mut s = Style::default(); s.flex_direction = FlexDirection::Column; s.padding = taffy::geometry::Rect { left: LP::length(8.0), right: LP::length(8.0), top: LP::length(8.0), bottom: LP::length(8.0) }; let n = self.taffy.new_leaf_with_context(s, NodeCtx::Panel).expect("taffy: create panel"); self.taffy.add_child(parent, n).expect("taffy: add panel"); self.needs_layout = true; n }
    pub fn spacer(&mut self, parent: NodeId) -> NodeId { let mut s = Style::default(); s.flex_grow = 1.0; let n = self.taffy.new_leaf(s).expect("taffy: create spacer"); self.taffy.add_child(parent, n).expect("taffy: add spacer"); self.needs_layout = true; n }
    pub fn scroll_view(&mut self, parent: NodeId) -> ScrollViewId {
        let sid = ScrollViewId(self.scrolls.len()); let mut viewport = Style::default(); viewport.flex_direction = FlexDirection::Column; viewport.min_size.height = LPA::length(0.0); viewport.overflow = taffy::geometry::Point { x: Overflow::Hidden, y: Overflow::Hidden };
        let node = self.taffy.new_leaf_with_context(viewport, NodeCtx::ScrollView(sid)).expect("taffy: create scroll view");
        let mut content = Style::default(); content.flex_direction = FlexDirection::Column; content.flex_shrink = 0.0; content.min_size.height = LPA::length(0.0); content.size.width = Dimension::percent(1.0);
        let content_node = self.taffy.new_leaf_with_context(content, NodeCtx::ScrollContent(sid)).expect("taffy: create scroll content");
        self.taffy.add_child(node, content_node).expect("taffy: add scroll content"); self.taffy.add_child(parent, node).expect("taffy: add scroll view"); self.scrolls.push(ScrollState::default()); self.scroll_nodes.push(node); self.scroll_contents.push(content_node); self.needs_layout = true; sid
    }
    pub fn scroll_content(&self, scroll: ScrollViewId) -> Option<NodeId> { self.scroll_contents.get(scroll.0).copied() }
    pub fn scroll_offset(&self, scroll: ScrollViewId) -> Option<f32> { self.scrolls.get(scroll.0).map(|s| s.offset_y) }
    pub fn scroll_max_offset(&self, scroll: ScrollViewId) -> Option<f32> { self.scrolls.get(scroll.0).map(|s| s.max_offset()) }
    pub fn scroll_by(&mut self, scroll: ScrollViewId, dy: f32) -> bool { let Some(s) = self.scrolls.get_mut(scroll.0) else { return false }; let old = s.offset_y; s.set_offset(old + dy); if old != s.offset_y { self.dirty = true; true } else { false } }
    pub fn scroll_to(&mut self, scroll: ScrollViewId, y: f32) -> bool { let Some(s) = self.scrolls.get_mut(scroll.0) else { return false }; let old = s.offset_y; s.set_offset(y); if old != s.offset_y { self.dirty = true; true } else { false } }
    pub fn scroll_to_top(&mut self, scroll: ScrollViewId) -> bool { self.scroll_to(scroll, 0.0) }
    pub fn scroll_to_bottom(&mut self, scroll: ScrollViewId) -> bool { self.scroll_max_offset(scroll).map(|y| self.scroll_to(scroll, y)).unwrap_or(false) }
    pub fn button(&mut self, parent: NodeId, label: &str) -> WidgetId { let mut s = Style::default(); s.justify_content = Some(JustifyContent::CENTER); s.align_items = Some(AlignItems::CENTER); s.padding = taffy::geometry::Rect { left: LP::length(12.0), right: LP::length(12.0), top: LP::length(5.0), bottom: LP::length(5.0) }; self.add_widget(parent, Box::new(Button::new(label)), s) }
    pub fn label(&mut self, parent: NodeId, text: &str) -> WidgetId { self.add_widget(parent, Box::new(Label::new(text)), Style::default()) }
    pub fn text_input(&mut self, parent: NodeId) -> WidgetId { self.text_input_with(parent, "") }
    pub fn text_input_with(&mut self, parent: NodeId, text: &str) -> WidgetId { let mut s = Style::default(); s.align_items = Some(AlignItems::CENTER); s.min_size = TSize { width: LPA::auto(), height: LPA::length(26.0) }; s.padding = taffy::geometry::Rect { left: LP::length(6.0), right: LP::length(6.0), top: LP::length(4.0), bottom: LP::length(4.0) }; self.add_widget(parent, Box::new(TextInput::new(text)), s) }
    fn add_widget(&mut self, parent: NodeId, widget: Box<dyn Widget>, style: Style) -> WidgetId { let id = WidgetId(self.widgets.len()); let node = self.taffy.new_leaf_with_context(style, NodeCtx::Widget(id)).expect("taffy: create widget"); self.taffy.add_child(parent, node).expect("taffy: add widget"); self.widgets.push(widget); self.widget_nodes.push(node); self.clicks.push(None); self.needs_layout = true; id }
    pub fn button_mut(&mut self, id: WidgetId) -> Option<&mut Button> { self.widgets.get_mut(id.0)?.as_any_mut().downcast_mut::<Button>() }
    pub fn label_mut(&mut self, id: WidgetId) -> Option<&mut Label> { self.widgets.get_mut(id.0)?.as_any_mut().downcast_mut::<Label>() }
    pub fn text_input_mut(&mut self, id: WidgetId) -> Option<&mut TextInput> { self.widgets.get_mut(id.0)?.as_any_mut().downcast_mut::<TextInput>() }
    pub fn text(&self, id: WidgetId) -> Option<&str> { let w = self.widgets.get(id.0)?; w.as_any().downcast_ref::<TextInput>().map(|v| v.text()).or_else(|| w.as_any().downcast_ref::<Label>().map(|v| v.text())) }
    pub fn set_label(&mut self, id: WidgetId, text: &str) { if let Some(v) = self.label_mut(id) { v.set_text(text); self.needs_layout = true; } }
    pub fn set_button_label(&mut self, id: WidgetId, text: &str) { if let Some(v) = self.button_mut(id) { v.set_label(text); self.needs_layout = true; } }
    pub fn set_text(&mut self, id: WidgetId, text: &str) { if let Some(v) = self.text_input_mut(id) { v.set_text(text); self.needs_layout = true; } }
    pub fn on_click<F: FnMut(&mut Ui) + 'static>(&mut self, id: WidgetId, f: F) { if let Some(slot) = self.clicks.get_mut(id.0) { *slot = Some(Box::new(f)); } }
    pub fn rect(&self, id: WidgetId) -> Option<Rect> { self.widgets.get(id.0).map(|w| w.rect()) }
    pub fn node_of(&self, id: WidgetId) -> Option<NodeId> { self.widget_nodes.get(id.0).copied() }
    pub fn focus(&self) -> Option<WidgetId> { self.focus }
    pub fn set_focus(&mut self, id: Option<WidgetId>) { if self.focus == id { return; } if let Some(old) = self.focus { if let Some(w) = self.widgets.get_mut(old.0) { w.set_focused(false); } } self.focus = id; if let Some(new) = id { if let Some(w) = self.widgets.get_mut(new.0) { w.set_focused(true); } } self.dirty = true; }
    pub fn request_full_redraw(&mut self) { self.dirty = true; self.needs_layout = true; }
    pub fn process(&mut self, win: &mut Window) {
        let mut buf = [WmEvent::default(); 256]; let n = win.poll_events(&mut buf); let mut events = Vec::new();
        for e in &buf[..n] { match e.kind { EV_MOUSE_DOWN => events.push(UiEvent::Down { x: e.a, y: e.b }), EV_MOUSE_UP => events.push(UiEvent::Up { x: e.a, y: e.b }), EV_MOUSE_MOVE => events.push(UiEvent::Move { x: e.a, y: e.b }), EV_KEY_DOWN => events.push(UiEvent::KeyDown { scancode: e.a as u8, ch: e.b as u8, mods: e.c as u8 }), EV_KEY_UP => events.push(UiEvent::KeyUp { scancode: e.a as u8, ch: e.b as u8, mods: e.c as u8 }), _ => {} } }
        self.set_root_size(win.client_width().max(1), win.client_height().max(1)); let mut clicked = Vec::new(); for ev in &events { if self.dispatch(ev, &mut clicked) { self.dirty = true; } }
        for id in clicked { let mut cb = self.clicks[id.0].take(); if let Some(f) = cb.as_mut() { f(self); } self.clicks[id.0] = cb; }
        if self.needs_layout || self.widgets.iter().any(|w| w.dirty()) { self.compute(); self.needs_layout = false; self.dirty = true; }
        if self.dirty { self.draw(win); let _ = win.flip(); self.dirty = false; for w in &mut self.widgets { w.clear_dirty(); } }
    }
    pub fn set_root_size(&mut self, w: u32, h: u32) { let w = w.max(1); let h = h.max(1); if w == self.root_w && h == self.root_h { return; } self.root_w = w; self.root_h = h; let _ = self.style(self.root, |s| s.size = TSize { width: Dimension::length(w as f32), height: Dimension::length(h as f32) }); }
    fn compute(&mut self) {
        let root = self.root; let rw = self.root_w; let rh = self.root_h; let widgets = &self.widgets; let taffy = &mut self.taffy; let space = TSize { width: AvailableSpace::Definite(rw as f32), height: AvailableSpace::Definite(rh as f32) };
        let _ = taffy.compute_layout_with_measure(root, space, |known, available, _node, ctx| { match ctx { Some(NodeCtx::Widget(id)) => widgets[id.0].measure(constraints_from_taffy(known, available)), _ => TSize::ZERO } });
        self.update_scroll_metrics(); self.apply_layout();
    }
    fn update_scroll_metrics(&mut self) { for i in 0..self.scrolls.len() { let v = self.taffy.layout(self.scroll_nodes[i]).ok().copied(); let c = self.taffy.layout(self.scroll_contents[i]).ok().copied(); if let (Some(v), Some(c)) = (v, c) { self.scrolls[i].viewport_height = v.size.height.max(0.0); self.scrolls[i].content_height = c.size.height.max(0.0); let max = self.scrolls[i].max_offset(); if self.scrolls[i].offset_y > max { self.scrolls[i].offset_y = max; } } } }
    fn apply_layout(&mut self) { self.apply_node(self.root, 0, 0, None); }
    fn apply_node(&mut self, node: NodeId, ox: i32, oy: i32, clip: Option<Rect>) { let l = match self.taffy.layout(node) { Ok(v) => *v, Err(_) => return }; let x = ox + l.location.x as i32; let y = oy + l.location.y as i32; let ctx = self.taffy.get_node_context(node).copied(); let mut child_y = y; let mut child_clip = clip; if let Some(NodeCtx::ScrollView(sid)) = ctx { let viewport = Rect::new(x, y, l.size.width.max(0.0) as u32, l.size.height.max(0.0) as u32); child_clip = child_clip.and_then(|c| c.intersect(viewport)).or(Some(viewport)); child_y -= self.scrolls[sid.0].offset_y as i32; } if let Some(NodeCtx::Widget(id)) = ctx { self.widgets[id.0].set_rect(Rect::new(x, y, l.size.width.max(0.0) as u32, l.size.height.max(0.0) as u32)); } if let Ok(children) = self.taffy.children(node) { for child in children { self.apply_node(child, x, child_y, child_clip); } } }
    fn dispatch(&mut self, ev: &UiEvent, clicked: &mut Vec<WidgetId>) -> bool {
        let mut changed = false;
        if let UiEvent::Down { x, y } = *ev { if let Some(sid) = self.scrollbar_at(x, y) { let l = match self.taffy.layout(self.scroll_nodes[sid.0]) { Ok(v) => *v, Err(_) => return false }; let r = Rect::new(l.location.x as i32, l.location.y as i32, l.size.width as u32, l.size.height as u32); let state = self.scrolls[sid.0]; let thumb_h = (r.h as f32 * r.h as f32 / state.content_height).max(16.0).min(r.h as f32) as u32; let travel = r.h.saturating_sub(thumb_h).max(1) as f32; let thumb_y = r.y + (travel * state.offset_y / state.max_offset().max(1.0)) as i32; self.scrolls[sid.0].dragging = true; self.scrolls[sid.0].drag_grab = (y - thumb_y) as f32; return true; } let hit = self.hit_test(x, y); if hit != self.focus { self.set_focus(hit); changed = true; } }
        if let UiEvent::Move { y, .. } = *ev { for i in 0..self.scrolls.len() { if !self.scrolls[i].dragging { continue; } let l = match self.taffy.layout(self.scroll_nodes[i]) { Ok(v) => *v, Err(_) => continue }; let r = Rect::new(l.location.x as i32, l.location.y as i32, l.size.width as u32, l.size.height as u32); let state = self.scrolls[i]; if state.content_height <= state.viewport_height { continue; } let th = (r.h as f32 * r.h as f32 / state.content_height).max(16.0).min(r.h as f32) as u32; let travel = r.h.saturating_sub(th).max(1) as f32; let t = (((y as f32) - r.y as f32 - state.drag_grab) / travel).max(0.0).min(1.0); self.scrolls[i].set_offset(t * state.max_offset()); changed = true; } }
        if let Some(fid) = self.focus { if fid.0 < self.widgets.len() { let result = self.widgets[fid.0].event(ev, true); if self.widgets[fid.0].dirty() { changed = true; } match result { EventResult::Clicked => { clicked.push(fid); return true; }, EventResult::Ignored if matches!(ev, UiEvent::KeyDown { scancode: 0x01, .. }) => { self.set_focus(None); return true; }, EventResult::Ignored => {}, EventResult::Consumed | EventResult::Changed | EventResult::Submitted => return true } } }
        if matches!(ev, UiEvent::Move { .. }) { for w in &mut self.widgets { if w.event(ev, false) != EventResult::Ignored { changed = true; } } } else { for i in (0..self.widgets.len()).rev() { if self.focus == Some(WidgetId(i)) { continue; } let result = self.widgets[i].event(ev, false); if self.widgets[i].dirty() { changed = true; } if result != EventResult::Ignored { if result == EventResult::Clicked { clicked.push(WidgetId(i)); } break; } } }
        if let UiEvent::Up { .. } = *ev { for state in &mut self.scrolls { state.dragging = false; } }
        if let UiEvent::KeyDown { scancode, .. } = *ev { const PAGE_UP: u8 = 0x49; const PAGE_DOWN: u8 = 0x51; if scancode == PAGE_UP || scancode == PAGE_DOWN { if let Some(fid) = self.focus { if let Some(sid) = self.scroll_for_widget(fid) { let delta = self.scrolls[sid.0].viewport_height * if scancode == PAGE_UP { -0.9 } else { 0.9 }; changed |= self.scroll_by(sid, delta); } } } }
        changed
    }
    fn scroll_for_widget(&self, id: WidgetId) -> Option<ScrollViewId> { let node = *self.widget_nodes.get(id.0)?; for i in 0..self.scroll_contents.len() { if self.is_descendant(node, self.scroll_contents[i]) { return Some(ScrollViewId(i)); } } None }
    fn is_descendant(&self, node: NodeId, ancestor: NodeId) -> bool { let mut cur = node; while let Some(parent) = self.taffy.parent(cur) { if parent == ancestor { return true; } cur = parent; } false }
    fn scrollbar_at(&self, x: i32, y: i32) -> Option<ScrollViewId> { for i in (0..self.scrolls.len()).rev() { let l = self.taffy.layout(self.scroll_nodes[i]).ok().copied()?; let r = Rect::new(l.location.x as i32, l.location.y as i32, l.size.width as u32, l.size.height as u32); if self.scrolls[i].content_height > self.scrolls[i].viewport_height && x >= r.x + r.w as i32 - 10 && y >= r.y && y < r.y + r.h as i32 { return Some(ScrollViewId(i)); } } None }
    fn hit_test(&self, x: i32, y: i32) -> Option<WidgetId> { if x < 0 || y < 0 || x >= self.root_w as i32 || y >= self.root_h as i32 { return None; } for i in (0..self.widgets.len()).rev() { let w = &self.widgets[i]; if w.focusable() && w.rect().contains(x, y) && self.point_visible_in_scroll(self.widget_nodes[i], x, y) { return Some(WidgetId(i)); } } None }
    fn point_visible_in_scroll(&self, node: NodeId, x: i32, y: i32) -> bool { let mut cur = node; while let Some(parent) = self.taffy.parent(cur) { if let Some(NodeCtx::ScrollView(_)) = self.taffy.get_node_context(parent) { let l = match self.taffy.layout(parent) { Ok(v) => *v, Err(_) => return false }; if !Rect::new(l.location.x as i32, l.location.y as i32, l.size.width as u32, l.size.height as u32).contains(x, y) { return false; } } if parent == self.root { break; } cur = parent; } true }
    fn draw(&mut self, win: &mut Window) { let _ = win.clear(BG); self.draw_node(self.root, 0, 0, None, win); self.draw_scrollbars(self.root, 0, 0, win); }
    fn draw_node(&self, node: NodeId, ox: i32, oy: i32, clip: Option<Rect>, win: &mut Window) { let l = match self.taffy.layout(node) { Ok(v) => *v, Err(_) => return }; let x = ox + l.location.x as i32; let y = oy + l.location.y as i32; let ctx = self.taffy.get_node_context(node).copied(); let mut child_clip = clip; let mut child_y = y; if let Some(NodeCtx::ScrollView(sid)) = ctx { let viewport = Rect::new(x, y, l.size.width as u32, l.size.height as u32); child_clip = child_clip.and_then(|c| c.intersect(viewport)).or(Some(viewport)); child_y -= self.scrolls[sid.0].offset_y as i32; } if let Some(NodeCtx::Panel) = ctx { draw_rect_clipped(win, Rect::new(x, y, l.size.width as u32, l.size.height as u32), clip, theme::PANEL_BG, true); } if let Some(NodeCtx::Widget(id)) = ctx { if child_clip.map_or(true, |c| self.widgets[id.0].rect().intersect(c).is_some()) { self.widgets[id.0].draw(win); } } if let Ok(children) = self.taffy.children(node) { for child in children { self.draw_node(child, x, child_y, child_clip, win); } } }
    fn draw_scrollbars(&self, node: NodeId, ox: i32, oy: i32, win: &mut Window) { let l = match self.taffy.layout(node) { Ok(v) => *v, Err(_) => return }; let x = ox + l.location.x as i32; let y = oy + l.location.y as i32; if let Some(NodeCtx::ScrollView(sid)) = self.taffy.get_node_context(node).copied() { let r = Rect::new(x, y, l.size.width as u32, l.size.height as u32); let state = self.scrolls[sid.0]; if state.content_height > state.viewport_height && r.h > 4 { let bw = 6u32; let bx = r.x + r.w as i32 - bw as i32; let _ = Rectangle::new(Point::new(bx, r.y), Size::new(bw, r.h)).into_styled(PrimitiveStyle::with_fill(theme::SCROLLBAR_BG)).draw(win); let th = (r.h as f32 * r.h as f32 / state.content_height).max(16.0).min(r.h as f32) as u32; let travel = r.h.saturating_sub(th); let ty = r.y + (travel as f32 * state.offset_y / state.max_offset().max(1.0)) as i32; let _ = Rectangle::new(Point::new(bx, ty), Size::new(bw, th)).into_styled(PrimitiveStyle::with_fill(theme::SCROLLBAR_THUMB)).draw(win); } } if let Ok(children) = self.taffy.children(node) { for child in children { self.draw_scrollbars(child, x, y, win); } } }
}

fn constraints_from_taffy(known: taffy::geometry::Size<Option<f32>>, available: taffy::geometry::Size<AvailableSpace>) -> Constraints { fn max(v: AvailableSpace) -> f32 { match v { AvailableSpace::Definite(v) => v, AvailableSpace::MinContent => 0.0, AvailableSpace::MaxContent => f32::INFINITY } } Constraints { min_width: known.width.unwrap_or(0.0), max_width: max(available.width), min_height: known.height.unwrap_or(0.0), max_height: max(available.height) } }
fn draw_rect_clipped(win: &mut Window, rect: Rect, clip: Option<Rect>, fill: Rgb888, border: bool) { let Some(r) = clip.map_or(Some(rect), |c| rect.intersect(c)) else { return }; let _ = Rectangle::new(Point::new(r.x, r.y), Size::new(r.w, r.h)).into_styled(PrimitiveStyle::with_fill(fill)).draw(win); if border { let _ = Rectangle::new(Point::new(rect.x, rect.y), Size::new(rect.w, rect.h)).into_styled(PrimitiveStyle::with_stroke(theme::PANEL_BORDER, 1)).draw(win); } }
impl Default for Ui { fn default() -> Self { Self::new() } }