//! Retained-mode UI with a flexbox layout tree.
//!
//! The UI is split into two trees:
//!   - a **layout tree** owned by [`taffy::tree::TaffyTree`] (one node per container *and*
//!     per leaf widget), and
//!   - a **widget tree** ([`Button`], [`Label`], [`TextInput`]) stored in a flat `Vec` indexed
//!     by [`WidgetId`].
//!
//! A [`WidgetId`] maps to a taffy [`NodeId`] (and back, via the taffy node context). Positions
//! and sizes are never set by hand: after [`Ui::process`] polls events we run
//! `compute_layout` over the tree, walk it, and write each widget's computed rect.
//!
//! Building is cursor-based: containers/leaves are attached to the *current container*
//! (`container`), and style helpers (`grow`, `padding`, …) act on the *last created* node
//! (`last`).
//!
//! ```ignore
//! let mut ui = Ui::new();
//!
//! // A vertical stack. Direct children stretch to the window width.
//! let root = ui.current_container();
//!
//! // Toolbar row.
//! let _toolbar = ui.row().gap(8).padding(8).justify_space_between();
//! let _open = ui.button("Open");
//! let _save = ui.button("Save");
//! ui.up();
//!
//! // Body.
//! let _body = ui.column().padding(12);
//! let _title = ui.label("Hello, Felix");
//! let _count = ui.label("count: 0");
//! let _input = ui.text_input();
//!
//! let mut count = 0u32;
//! ui.on_click(open, move |ui| {
//!     count += 1;
//!     ui.set_label(count_label, &alloc::format!("count: {count}"));
//! });
//!
//! loop {
//!     ui.process(&mut win);
//! }
//! ```

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use embedded_graphics_unicodefonts::mono_9x18_atlas;
use embedded_graphics::{
    mono_font::MonoTextStyle,
    pixelcolor::Rgb888,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle, RoundedRectangle},
    text::{Baseline, Text},
};
use taffy::compute_leaf_layout;
use taffy::geometry::{Rect as TRect, Size as TSize};
use taffy::prelude::{
    AvailableSpace, Dimension, LengthPercentage as LP, LengthPercentageAuto as LPA,
};
use taffy::tree::TaffyTree;

/// Taffy style/geometry types surfaced for building.
pub use taffy::prelude::{AlignContent, AlignItems, FlexDirection, JustifyContent, Position, Style};
/// A node in the taffy layout tree (containers and leaf widgets).
pub use taffy::tree::NodeId;

use crate::wm::{
    Window, WmEvent, EV_KEY_DOWN, EV_KEY_UP, EV_MOUSE_DOWN, EV_MOUSE_MOVE, EV_MOUSE_UP,
};

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

pub const BG: Rgb888 = Rgb888::new(0x10, 0x18, 0x20);
pub const BTN_BG: Rgb888 = Rgb888::new(0x2A, 0x3A, 0x4A);
pub const BTN_BG_HOT: Rgb888 = Rgb888::new(0x3A, 0x5A, 0x7A);
pub const BTN_BG_DOWN: Rgb888 = Rgb888::new(0x1A, 0x2A, 0x3A);
pub const BTN_BORDER: Rgb888 = Rgb888::new(0x50, 0x60, 0x70);
pub const TEXT: Rgb888 = Rgb888::new(0xF0, 0xF0, 0xF0);
pub const LABEL_FG: Rgb888 = Rgb888::new(0xC8, 0xD0, 0xD8);
pub const INPUT_BG: Rgb888 = Rgb888::new(0x18, 0x20, 0x28);
pub const INPUT_BORDER: Rgb888 = Rgb888::new(0x50, 0x60, 0x70);
pub const INPUT_BORDER_FOCUS: Rgb888 = Rgb888::new(0x3A, 0x7C, 0xA5);
pub const PANEL_BG: Rgb888 = Rgb888::new(0x18, 0x22, 0x2E);
pub const PANEL_BORDER: Rgb888 = Rgb888::new(0x40, 0x50, 0x60);

/// Mono font metrics (embedded-graphics `mono_9x18`).
const FONT_W: i32 = 9;
const FONT_H: f32 = 18.0;

const SCAN_ESC: u8 = 0x01;
const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_ENTER: u8 = 0x1C;

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiEvent {
    Down { x: i32, y: i32 },
    Move { x: i32, y: i32 },
    Up { x: i32, y: i32 },
    /// `ch` = 0 if no printable; `mods`: bit0=shift, bit1=ctrl
    KeyDown { scancode: u8, ch: u8, mods: u8 },
    KeyUp { scancode: u8, ch: u8, mods: u8 },
}

// ---------------------------------------------------------------------------
// Ids
// ---------------------------------------------------------------------------

/// Identifies a leaf widget (button / label / text input).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WidgetId(usize);

/// An axis-aligned rectangle in client (window) coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl LayoutRect {
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x + self.w as i32
            && y < self.y + self.h as i32
    }
}

// ---------------------------------------------------------------------------
// Button
// ---------------------------------------------------------------------------

pub struct Button {
    pub label: String,
    /// Computed layout rectangle (set by the layout pass, never by hand).
    pub rect: LayoutRect,
    pub dirty: bool,
    hot: bool,
    down: bool,
    /// Set when a click completes; consumed by callback dispatch.
    pending_click: bool,
}

impl Button {
    pub fn new(label: &str) -> Self {
        Self {
            label: String::from(label),
            rect: LayoutRect::default(),
            dirty: true,
            hot: false,
            down: false,
            pending_click: false,
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn set_label(&mut self, s: &str) {
        if self.label != s {
            self.label = String::from(s);
            self.dirty = true;
        }
    }

    fn set_rect(&mut self, x: i32, y: i32, w: u32, h: u32) {
        self.rect = LayoutRect::new(x, y, w, h);
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        self.rect.contains(x, y)
    }

    /// Returns `true` if the event was consumed.
    fn process(&mut self, ev: &UiEvent) -> bool {
        match *ev {
            UiEvent::Down { x, y } if self.contains(x, y) => {
                self.down = true;
                self.hot = true;
                self.dirty = true;
                true
            }
            UiEvent::Up { x, y } if self.down => {
                let click = self.contains(x, y);
                self.down = false;
                self.dirty = true;
                if click {
                    self.pending_click = true;
                }
                true
            }
            UiEvent::Move { x, y } => {
                let hot = self.contains(x, y);
                if hot != self.hot {
                    self.hot = hot;
                    self.dirty = true;
                }
                // hover is non-exclusive — do not consume
                false
            }
            _ => false,
        }
    }

    fn draw(&self, win: &mut Window) {
        let r = self.rect;
        if r.w == 0 || r.h == 0 {
            return;
        }
        let bg = if self.down && self.hot {
            BTN_BG_DOWN
        } else if self.hot {
            BTN_BG_HOT
        } else {
            BTN_BG
        };

        let rect = Rectangle::new(Point::new(r.x, r.y), Size::new(r.w, r.h));
        let rr = RoundedRectangle::with_equal_corners(rect, Size::new(4, 4));
        let _ = rr.into_styled(PrimitiveStyle::with_fill(bg)).draw(win);
        let _ = rr
            .into_styled(PrimitiveStyle::with_stroke(BTN_BORDER, 1))
            .draw(win);

        let binding = mono_9x18_atlas();
        let style = MonoTextStyle::new(&binding, TEXT);
        let tw = self.label.chars().count() as i32 * FONT_W;
        let tx = r.x + (r.w as i32 - tw) / 2;
        let ty = r.y + (r.h as i32 - FONT_H as i32) / 2;
        let _ = Text::with_baseline(
            self.label.as_str(),
            Point::new(tx.max(r.x + 4), ty.max(r.y + 2)),
            style,
            Baseline::Top,
        )
        .draw(win);
    }
}

// ---------------------------------------------------------------------------
// Label
// ---------------------------------------------------------------------------

pub struct Label {
    pub text: String,
    /// Computed layout rectangle.
    pub rect: LayoutRect,
    pub dirty: bool,
    fg: Rgb888,
}

impl Label {
    pub fn new(text: &str) -> Self {
        Self {
            text: String::from(text),
            rect: LayoutRect::default(),
            dirty: true,
            fg: LABEL_FG,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, s: &str) {
        if self.text != s {
            self.text = String::from(s);
            self.dirty = true;
        }
    }

    pub fn set_color(&mut self, c: Rgb888) {
        self.fg = c;
        self.dirty = true;
    }

    fn set_rect(&mut self, x: i32, y: i32, w: u32, h: u32) {
        self.rect = LayoutRect::new(x, y, w, h);
    }

    fn draw(&self, win: &mut Window) {
        let r = self.rect;
        if r.w == 0 || r.h == 0 {
            return;
        }
        let binding = mono_9x18_atlas();
        let style = MonoTextStyle::new(&binding, self.fg);
        let ty = r.y + (r.h as i32 - FONT_H as i32) / 2;
        let _ = Text::with_baseline(
            self.text.as_str(),
            Point::new(r.x, ty.max(r.y)),
            style,
            Baseline::Top,
        )
        .draw(win);
    }
}

// ---------------------------------------------------------------------------
// TextInput
// ---------------------------------------------------------------------------

pub struct TextInput {
    pub text: String,
    /// Computed layout rectangle.
    pub rect: LayoutRect,
    pub dirty: bool,
    focused: bool,
    max_len: usize,
    /// Set when Enter is pressed while focused; consumed by `take_submit`.
    pending_submit: bool,
}

impl TextInput {
    pub fn new(text: &str) -> Self {
        Self {
            text: String::from(text),
            rect: LayoutRect::default(),
            dirty: true,
            focused: false,
            max_len: 64,
            pending_submit: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, s: &str) {
        if self.text != s {
            self.text = String::from(s);
            self.dirty = true;
        }
    }

    /// Returns true and clears the flag if Enter was pressed since last call.
    pub fn take_submit(&mut self) -> bool {
        let v = self.pending_submit;
        self.pending_submit = false;
        v
    }

    fn set_rect(&mut self, x: i32, y: i32, w: u32, h: u32) {
        self.rect = LayoutRect::new(x, y, w, h);
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        self.rect.contains(x, y)
    }

    fn set_focused(&mut self, f: bool) {
        if self.focused != f {
            self.focused = f;
            self.dirty = true;
        }
    }

    /// `true` = consumed. Escape returns `false` so default can unfocus.
    fn process(&mut self, ev: &UiEvent, focused: bool) -> bool {
        if !focused {
            return false;
        }
        match *ev {
            UiEvent::KeyDown {
                scancode,
                ch,
                mods: _,
            } => {
                if scancode == SCAN_ESC {
                    return false; // bubble → default clears focus
                }
                if scancode == SCAN_BACKSPACE {
                    if self.text.pop().is_some() {
                        self.dirty = true;
                    }
                    return true;
                }
                if scancode == SCAN_ENTER {
                    self.pending_submit = true;
                    return true;
                }
                if ch >= 0x20 && ch < 0x7f && self.text.len() < self.max_len {
                    self.text.push(ch as char);
                    self.dirty = true;
                    return true;
                }
                true // unknown key while focused — consume to avoid leaking to others
            }
            UiEvent::KeyUp { .. } => true,
            UiEvent::Down { .. } => true,
            UiEvent::Up { .. } => true,
            UiEvent::Move { .. } => false,
        }
    }

    fn draw(&self, win: &mut Window) {
        let r = self.rect;
        if r.w == 0 || r.h == 0 {
            return;
        }
        let border = if self.focused {
            INPUT_BORDER_FOCUS
        } else {
            INPUT_BORDER
        };
        let rect = Rectangle::new(Point::new(r.x, r.y), Size::new(r.w, r.h));
        let _ = rect
            .into_styled(PrimitiveStyle::with_fill(INPUT_BG))
            .draw(win);
        let _ = rect
            .into_styled(PrimitiveStyle::with_stroke(border, 1))
            .draw(win);

        let binding = mono_9x18_atlas();
        let style = MonoTextStyle::new(&binding, TEXT);
        let ty = r.y + (r.h as i32 - FONT_H as i32) / 2;
        let mut shown = self.text.as_str();
        let max_chars = (r.w as usize).saturating_sub(8) / FONT_W as usize;
        if shown.len() > max_chars {
            shown = &shown[shown.len() - max_chars..];
        }
        let _ = Text::with_baseline(
            shown,
            Point::new(r.x + 4, ty.max(r.y + 2)),
            style,
            Baseline::Top,
        )
        .draw(win);

        if self.focused {
            let cx = r.x + 4 + (shown.len() as i32) * FONT_W;
            let _ = Rectangle::new(
                Point::new(cx, r.y + 4),
                Size::new(2, r.h.saturating_sub(8)),
            )
            .into_styled(PrimitiveStyle::with_fill(TEXT))
            .draw(win);
        }
    }
}

// ---------------------------------------------------------------------------
// Widget (leaf data) + taffy node context
// ---------------------------------------------------------------------------

enum Widget {
    Button(Button),
    Label(Label),
    TextInput(TextInput),
}

impl Widget {
    fn dirty(&self) -> bool {
        match self {
            Widget::Button(b) => b.dirty,
            Widget::Label(l) => l.dirty,
            Widget::TextInput(t) => t.dirty,
        }
    }

    fn clear_dirty(&mut self) {
        match self {
            Widget::Button(b) => b.dirty = false,
            Widget::Label(l) => l.dirty = false,
            Widget::TextInput(t) => t.dirty = false,
        }
    }

    fn rect(&self) -> LayoutRect {
        match self {
            Widget::Button(b) => b.rect,
            Widget::Label(l) => l.rect,
            Widget::TextInput(t) => t.rect,
        }
    }

    fn set_rect(&mut self, x: i32, y: i32, w: u32, h: u32) {
        match self {
            Widget::Button(b) => b.set_rect(x, y, w, h),
            Widget::Label(l) => l.set_rect(x, y, w, h),
            Widget::TextInput(t) => t.set_rect(x, y, w, h),
        }
    }

    /// Intrinsic content size (text only, excluding padding/margin). Taffy adds the box model.
    fn content_size(&self) -> TSize<f32> {
        let (w, h) = match self {
            Widget::Button(b) => (b.label.chars().count() as f32 * FONT_W as f32, FONT_H),
            Widget::Label(l) => (l.text.chars().count() as f32 * FONT_W as f32, FONT_H),
            Widget::TextInput(t) => (
                t.text.chars().count().max(1) as f32 * FONT_W as f32,
                FONT_H,
            ),
        };
        TSize {
            width: w.max(0.0),
            height: h,
        }
    }

    fn draw(&self, win: &mut Window) {
        match self {
            Widget::Button(b) => b.draw(win),
            Widget::Label(l) => l.draw(win),
            Widget::TextInput(t) => t.draw(win),
        }
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        match self {
            Widget::Button(b) => b.contains(x, y),
            Widget::TextInput(t) => t.contains(x, y),
            Widget::Label(_) => false,
        }
    }

    fn focusable(&self) -> bool {
        matches!(self, Widget::Button(_) | Widget::TextInput(_))
    }

    fn set_focused_visual(&mut self, f: bool) {
        if let Widget::TextInput(t) = self {
            t.set_focused(f);
        }
    }

    fn process(&mut self, ev: &UiEvent, focused: bool) -> bool {
        match self {
            Widget::Button(b) => b.process(ev),
            Widget::TextInput(t) => t.process(ev, focused),
            Widget::Label(_) => false,
        }
    }
}

/// Per-node user data held in the taffy tree. Leaf widgets carry their [`WidgetId`];
/// panels carry their background/border colour. Plain containers carry `None`.
#[derive(Clone, Copy)]
enum NodeCtx {
    Widget(WidgetId),
    Panel { bg: Rgb888, border: Rgb888 },
}

// ---------------------------------------------------------------------------
// Ui
// ---------------------------------------------------------------------------

pub struct Ui {
    /// Layout tree: one node per container and per leaf widget.
    taffy: TaffyTree<NodeCtx>,
    /// Leaf widget data, indexed by [`WidgetId`].
    widgets: Vec<Widget>,
    /// `widget index -> taffy node`.
    node_of: Vec<NodeId>,
    /// `on_click` handlers, parallel to `widgets` (buttons only).
    clicks: Vec<Option<Box<dyn FnMut(&mut Ui)>>>,

    root: NodeId,
    root_w: u32,
    root_h: u32,

    /// New children (containers *and* widgets) are attached here.
    container: NodeId,
    /// Style helpers (`grow`, `padding`, …) act on the most recently created node.
    last: NodeId,

    focus: Option<WidgetId>,
    needs_layout: bool,
    dirty: bool,
    /// ids that completed a click this `process` tick (legacy helper).
    clicked: Vec<WidgetId>,
}

impl Ui {
    /// Create an empty UI. The root is a full-window column; its size is synced to the
    /// window on the first [`Ui::process`] (or via [`Ui::with_size`]).
    pub fn new() -> Self {
        Self::with_size(1, 1)
    }

    /// Create a UI with an explicit initial root size.
    pub fn with_size(w: u32, h: u32) -> Self {
        let mut taffy: TaffyTree<NodeCtx> = TaffyTree::new();
        let mut root_style = Style::default();
        root_style.flex_direction = FlexDirection::Column;
        root_style.size = TSize {
            width: Dimension::length(w.max(1) as f32),
            height: Dimension::length(h.max(1) as f32),
        };
        let root = taffy.new_leaf(root_style).expect("taffy: create root");
        Self {
            taffy,
            widgets: Vec::new(),
            node_of: Vec::new(),
            clicks: Vec::new(),
            root,
            root_w: w.max(1),
            root_h: h.max(1),
            container: root,
            last: root,
            focus: None,
            needs_layout: true,
            dirty: true,
            clicked: Vec::new(),
        }
    }

    // -- accessors -----------------------------------------------------------

    /// The node new children attach to.
    pub fn current_container(&self) -> NodeId {
        self.container
    }

    /// The taffy node backing a widget.
    pub fn node_of(&self, id: WidgetId) -> Option<NodeId> {
        self.node_of.get(id.0).copied()
    }

    /// Computed client-space rectangle of a widget.
    pub fn rect(&self, id: WidgetId) -> Option<LayoutRect> {
        self.widgets.get(id.0).map(Widget::rect)
    }

    pub fn root_node(&self) -> NodeId {
        self.root
    }

    pub fn root_size(&self) -> (u32, u32) {
        (self.root_w, self.root_h)
    }

    // -- containers ----------------------------------------------------------

    /// A vertical flex container (children stacked top→bottom).
    pub fn column(&mut self) -> NodeId {
        self.container_node(FlexDirection::Column)
    }

    /// A horizontal flex container (children laid out left→right) — e.g. a toolbar.
    pub fn row(&mut self) -> NodeId {
        self.container_node(FlexDirection::Row)
    }

    /// A generic flex container (defaults to a row; configure with `flex_direction`).
    pub fn flex(&mut self) -> NodeId {
        self.container_node(FlexDirection::Row)
    }

    /// A bordered, filled box (column by default) that draws a background.
    pub fn panel(&mut self) -> NodeId {
        let mut s = Style::default();
        s.flex_direction = FlexDirection::Column;
        s.padding = TRect {
            left: LP::length(8.0),
            right: LP::length(8.0),
            top: LP::length(8.0),
            bottom: LP::length(8.0),
        };
        let node = self.taffy.new_leaf(s).expect("taffy: node");
        let _ = self.taffy.set_node_context(node, Some(NodeCtx::Panel { bg: PANEL_BG, border: PANEL_BORDER }));
        self.taffy.add_child(self.container, node).expect("taffy: add_child");
        self.container = node;
        self.last = node;
        self.needs_layout = true;
        node
    }

    /// A zero-size child that grows to absorb leftover space.
    pub fn spacer(&mut self) -> NodeId {
        let mut s = Style::default();
        s.flex_grow = 1.0;
        let node = self.taffy.new_leaf(s).expect("taffy: node");
        self.taffy.add_child(self.container, node).expect("taffy: add_child");
        self.last = node;
        self.needs_layout = true;
        node
    }

    /// Move the current container back up one level.
    pub fn up(&mut self) {
        if let Some(p) = self.taffy.parent(self.container) {
            self.container = p;
            self.last = p;
        }
    }

    fn container_node(&mut self, dir: FlexDirection) -> NodeId {
        let mut s = Style::default();
        s.flex_direction = dir;
        let node = self.taffy.new_leaf(s).expect("taffy: node");
        self.taffy.add_child(self.container, node).expect("taffy: add_child");
        self.container = node;
        self.last = node;
        self.needs_layout = true;
        node
    }

    // -- leaf widgets --------------------------------------------------------

    /// Add a button to the current container.
    pub fn button(&mut self, label: &str) -> WidgetId {
        let wid = WidgetId(self.widgets.len());
        self.widgets.push(Widget::Button(Button::new(label)));
        self.clicks.push(None);

        let mut s = Style::default();
        s.justify_content = Some(JustifyContent::CENTER);
        s.align_items = Some(AlignItems::CENTER);
        s.padding = TRect {
            left: LP::length(12.0),
            right: LP::length(12.0),
            top: LP::length(5.0),
            bottom: LP::length(5.0),
        };
        let node = self.taffy.new_leaf_with_context(s, NodeCtx::Widget(wid)).expect("taffy: node");
        self.taffy.add_child(self.container, node).expect("taffy: add_child");
        self.node_of.push(node);
        self.last = node;
        self.needs_layout = true;
        wid
    }

    /// Add a text label to the current container.
    pub fn label(&mut self, text: &str) -> WidgetId {
        let wid = WidgetId(self.widgets.len());
        self.widgets.push(Widget::Label(Label::new(text)));
        self.clicks.push(None);

        let s = Style::default();
        let node = self.taffy.new_leaf_with_context(s, NodeCtx::Widget(wid)).expect("taffy: node");
        self.taffy.add_child(self.container, node).expect("taffy: add_child");
        self.node_of.push(node);
        self.last = node;
        self.needs_layout = true;
        wid
    }

    /// Add an empty text input to the current container.
    pub fn text_input(&mut self) -> WidgetId {
        self.text_input_with("")
    }

    /// Add a text input (with initial text) to the current container.
    pub fn text_input_with(&mut self, text: &str) -> WidgetId {
        let wid = WidgetId(self.widgets.len());
        self.widgets.push(Widget::TextInput(TextInput::new(text)));
        self.clicks.push(None);

        let mut s = Style::default();
        s.align_items = Some(AlignItems::CENTER);
        s.min_size = TSize {
            width: LPA::auto(),
            height: LPA::length(24.0),
        };
        s.padding = TRect {
            left: LP::length(6.0),
            right: LP::length(6.0),
            top: LP::length(4.0),
            bottom: LP::length(4.0),
        };
        let node = self.taffy.new_leaf_with_context(s, NodeCtx::Widget(wid)).expect("taffy: node");
        self.taffy.add_child(self.container, node).expect("taffy: add_child");
        self.node_of.push(node);
        self.last = node;
        self.needs_layout = true;
        wid
    }

    // -- style helpers (act on the last created node) ------------------------

    fn with_style(&mut self, f: impl FnOnce(&mut Style)) {
        let mut s = self.taffy.style(self.last).expect("taffy: style").clone();
        f(&mut s);
        let _ = self.taffy.set_style(self.last, s);
        self.needs_layout = true;
    }

    pub fn grow(&mut self, factor: f32) {
        self.with_style(|s| s.flex_grow = factor);
    }

    pub fn shrink(&mut self, factor: f32) {
        self.with_style(|s| s.flex_shrink = factor);
    }

    pub fn padding(&mut self, v: u32) {
        self.with_style(|s| {
            let p = LP::length(v as f32);
            s.padding = TRect { left: p, right: p, top: p, bottom: p };
        });
    }

    pub fn padding_xy(&mut self, x: u32, y: u32) {
        self.with_style(|s| {
            s.padding = TRect {
                left: LP::length(x as f32),
                right: LP::length(x as f32),
                top: LP::length(y as f32),
                bottom: LP::length(y as f32),
            };
        });
    }

    pub fn margin(&mut self, v: u32) {
        self.with_style(|s| {
            let m = LPA::length(v as f32);
            s.margin = TRect { left: m, right: m, top: m, bottom: m };
        });
    }

    pub fn gap(&mut self, v: u32) {
        self.with_style(|s| {
            let g = LP::length(v as f32);
            s.gap = TSize { width: g, height: g };
        });
    }

    pub fn width(&mut self, v: u32) {
        self.with_style(|s| s.size.width = Dimension::length(v as f32));
    }

    pub fn height(&mut self, v: u32) {
        self.with_style(|s| s.size.height = Dimension::length(v as f32));
    }

    pub fn size(&mut self, w: u32, h: u32) {
        self.with_style(|s| {
            s.size = TSize {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
            };
        });
    }

    pub fn min_width(&mut self, v: u32) {
        self.with_style(|s| s.min_size.width = LPA::length(v as f32));
    }

    pub fn max_width(&mut self, v: u32) {
        self.with_style(|s| s.max_size.width = LPA::length(v as f32));
    }

    pub fn min_height(&mut self, v: u32) {
        self.with_style(|s| s.min_size.height = LPA::length(v as f32));
    }

    pub fn max_height(&mut self, v: u32) {
        self.with_style(|s| s.max_size.height = LPA::length(v as f32));
    }

    pub fn flex_direction(&mut self, d: FlexDirection) {
        self.with_style(|s| s.flex_direction = d);
    }

    pub fn justify(&mut self, jc: JustifyContent) {
        self.with_style(|s| s.justify_content = Some(jc));
    }

    pub fn align_items(&mut self, ai: AlignItems) {
        self.with_style(|s| s.align_items = Some(ai));
    }

    pub fn align_content(&mut self, ac: AlignContent) {
        self.with_style(|s| s.align_content = Some(ac));
    }

    pub fn position(&mut self, p: Position) {
        self.with_style(|s| s.position = p);
    }

    // convenience alignment / distribution helpers
    pub fn justify_center(&mut self) {
        self.justify(JustifyContent::CENTER);
    }
    pub fn justify_start(&mut self) {
        self.justify(JustifyContent::START);
    }
    pub fn justify_end(&mut self) {
        self.justify(JustifyContent::END);
    }
    pub fn justify_space_between(&mut self) {
        self.justify(JustifyContent::SPACE_BETWEEN);
    }
    pub fn justify_space_around(&mut self) {
        self.justify(JustifyContent::SPACE_AROUND);
    }
    pub fn justify_space_evenly(&mut self) {
        self.justify(JustifyContent::SPACE_EVENLY);
    }
    pub fn align_center(&mut self) {
        self.align_items(AlignItems::CENTER);
    }
    pub fn align_start(&mut self) {
        self.align_items(AlignItems::FLEX_START);
    }
    pub fn align_end(&mut self) {
        self.align_items(AlignItems::FLEX_END);
    }
    pub fn align_stretch(&mut self) {
        self.align_items(AlignItems::STRETCH);
    }

    // -- widget accessors ----------------------------------------------------

    pub fn button_mut(&mut self, id: WidgetId) -> Option<&mut Button> {
        match self.widgets.get_mut(id.0)? {
            Widget::Button(b) => Some(b),
            _ => None,
        }
    }

    pub fn label_mut(&mut self, id: WidgetId) -> Option<&mut Label> {
        match self.widgets.get_mut(id.0)? {
            Widget::Label(l) => Some(l),
            _ => None,
        }
    }

    pub fn text_input_mut(&mut self, id: WidgetId) -> Option<&mut TextInput> {
        match self.widgets.get_mut(id.0)? {
            Widget::TextInput(t) => Some(t),
            _ => None,
        }
    }

    pub fn text(&self, id: WidgetId) -> Option<&str> {
        match self.widgets.get(id.0)? {
            Widget::TextInput(t) => Some(t.text()),
            Widget::Label(l) => Some(l.text()),
            _ => None,
        }
    }

    pub fn set_label(&mut self, id: WidgetId, s: &str) {
        if let Some(l) = self.label_mut(id) {
            l.set_text(s);
        }
        self.needs_layout = true;
    }

    pub fn set_button_label(&mut self, id: WidgetId, s: &str) {
        if let Some(b) = self.button_mut(id) {
            b.set_label(s);
        }
        self.needs_layout = true;
    }

    pub fn set_text(&mut self, id: WidgetId, s: &str) {
        if let Some(t) = self.text_input_mut(id) {
            t.set_text(s);
        }
        self.needs_layout = true;
    }

    /// True if the given TextInput received Enter since last check (clears the flag).
    pub fn take_submit(&mut self, id: WidgetId) -> bool {
        if let Some(t) = self.text_input_mut(id) {
            t.take_submit()
        } else {
            false
        }
    }

    /// Register a click handler for a button. Replaces any previous handler.
    pub fn on_click<F: FnMut(&mut Ui) + 'static>(&mut self, id: WidgetId, f: F) {
        if id.0 < self.clicks.len() {
            self.clicks[id.0] = Some(Box::new(f));
        }
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
                w.set_focused_visual(false);
            }
        }
        self.focus = id;
        if let Some(new) = id {
            if let Some(w) = self.widgets.get_mut(new.0) {
                w.set_focused_visual(true);
            }
        }
        self.dirty = true;
    }

    /// Legacy helper (filled each `process` tick).
    pub fn clicked(&self, id: WidgetId) -> bool {
        self.clicked.contains(&id)
    }

    pub fn request_full_redraw(&mut self) {
        self.needs_layout = true;
        self.dirty = true;
    }

    // -- main loop -----------------------------------------------------------

    /// Poll window events, dispatch, recompute layout, redraw if needed, flip.
    pub fn process(&mut self, win: &mut Window) {
        self.clicked.clear();

        // 1. Poll events. `Window::poll_events` rebuilds the pixel buffer on EV_RESIZE.
        let mut buf = [WmEvent::default(); 256];
        let n = win.poll_events(&mut buf);
        let mut events: Vec<UiEvent> = Vec::new();
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

        // 2. Sync the root to the window's client size (covers init + EV_RESIZE).
        let cw = win.client_width().max(1);
        let ch = win.client_height().max(1);
        if cw != self.root_w || ch != self.root_h {
            self.set_root_size(cw, ch);
        }

        // 3. Dispatch events (hit-test uses the previously computed rects).
        for ev in &events {
            if self.dispatch(ev) {
                self.dirty = true;
            }
        }

        // 4. Fire click callbacks (taken out so they can borrow &mut Ui).
        let mut fired: Vec<usize> = Vec::new();
        for (i, w) in self.widgets.iter_mut().enumerate() {
            if let Widget::Button(b) = w {
                if b.pending_click {
                    b.pending_click = false;
                    fired.push(i);
                    self.clicked.push(WidgetId(i));
                }
            }
        }
        for i in fired {
            let mut cb = self.clicks[i].take();
            if let Some(f) = cb.as_mut() {
                f(self);
            }
            self.clicks[i] = cb;
        }

        // 5. Recompute if the tree, root size, or any content changed.
        if self.needs_layout || self.widgets.iter().any(Widget::dirty) {
            self.compute();
            self.needs_layout = false;
            self.dirty = true;
        }

        // 6. Draw + flip.
        if self.dirty {
            self.draw(win);
            let _ = win.flip();
            self.dirty = false;
            for w in self.widgets.iter_mut() {
                w.clear_dirty();
            }
        }
    }

    /// Resize the root and mark the layout dirty.
    pub fn set_root_size(&mut self, w: u32, h: u32) {
        let w = w.max(1);
        let h = h.max(1);
        if w == self.root_w && h == self.root_h {
            return;
        }
        self.root_w = w;
        self.root_h = h;
        let mut s = self.taffy.style(self.root).expect("taffy: style").clone();
        s.size = TSize {
            width: Dimension::length(w as f32),
            height: Dimension::length(h as f32),
        };
        let _ = self.taffy.set_style(self.root, s);
        self.needs_layout = true;
    }

    // -- layout --------------------------------------------------------------

    /// Run taffy over the tree, then copy each node's computed rect onto its widget.
    fn compute(&mut self) {
        let root = self.root;
        let root_w = self.root_w;
        let root_h = self.root_h;
        {
            let widgets = &self.widgets;
            let taffy = &mut self.taffy;
            let space = TSize {
                width: AvailableSpace::Definite(root_w as f32),
                height: AvailableSpace::Definite(root_h as f32),
            };
            let _ = taffy.compute_layout_with_measure(root, space, |inputs, _node, ctx, style| {
                let content = match ctx {
                    Some(&mut NodeCtx::Widget(wid)) => widgets[wid.0].content_size(),
                    _ => TSize::ZERO,
                };
                compute_leaf_layout(inputs, style, |_, _| 0.0, |known, _avail| {
                    TSize {
                        width: known.width.unwrap_or(content.width),
                        height: known.height.unwrap_or(content.height),
                    }
                })
            });
        }
        self.apply_layout();
    }

    fn apply_layout(&mut self) {
        self.apply_node(self.root, 0, 0);
    }

    /// Walk the tree, accumulating absolute offsets, and store each widget's client rect.
    fn apply_node(&mut self, node: NodeId, ox: i32, oy: i32) {
        let l = self.taffy.layout(node).expect("taffy: layout");
        let x = ox + l.location.x as i32;
        let y = oy + l.location.y as i32;
        let w = l.size.width as u32;
        let h = l.size.height as u32;

        if let Some(&NodeCtx::Widget(wid)) = self.taffy.get_node_context(node) {
            self.widgets[wid.0].set_rect(x, y, w, h);
        }
        if let Ok(children) = self.taffy.children(node) {
            for c in children {
                self.apply_node(c, x, y);
            }
        }
    }

    // -- events --------------------------------------------------------------

    /// Dispatch one event. Returns true if something visual changed.
    fn dispatch(&mut self, ev: &UiEvent) -> bool {
        let mut any = false;

        // 1) MouseDown: hit-test → focus
        if let UiEvent::Down { x, y } = *ev {
            let hit = self.hit_test(x, y);
            if hit != self.focus {
                self.set_focus(hit);
                any = true;
            }
        }

        // 2) Focused widget first
        if let Some(fid) = self.focus {
            if fid.0 < self.widgets.len() {
                let consumed = self.widgets[fid.0].process(ev, true);
                if self.widgets[fid.0].dirty() {
                    any = true;
                }
                if consumed {
                    if matches!(ev, UiEvent::Move { .. }) {
                        any |= self.default_hover(ev);
                    }
                    return any;
                }
                if matches!(
                    ev,
                    UiEvent::KeyDown {
                        scancode: SCAN_ESC,
                        ..
                    }
                ) {
                    self.set_focus(None);
                    any = true;
                    return any;
                }
            }
        }

        // 3) Default path
        any |= self.default_handle(ev);
        any
    }

    /// Topmost focusable widget whose computed rect contains `(x, y)`.
    /// Points outside the client area are clipped (no hit).
    fn hit_test(&self, x: i32, y: i32) -> Option<WidgetId> {
        if x < 0 || y < 0 || x >= self.root_w as i32 || y >= self.root_h as i32 {
            return None;
        }
        for i in (0..self.widgets.len()).rev() {
            if self.widgets[i].focusable() && self.widgets[i].contains(x, y) {
                return Some(WidgetId(i));
            }
        }
        None
    }

    fn default_hover(&mut self, ev: &UiEvent) -> bool {
        let mut any = false;
        if let UiEvent::Move { x, y } = *ev {
            for w in self.widgets.iter_mut() {
                if let Widget::Button(b) = w {
                    let hot = b.contains(x, y);
                    if hot != b.hot {
                        b.hot = hot;
                        b.dirty = true;
                        any = true;
                    }
                }
            }
        }
        any
    }

    fn default_handle(&mut self, ev: &UiEvent) -> bool {
        let mut any = self.default_hover(ev);
        match *ev {
            UiEvent::Down { .. } | UiEvent::Up { .. } => {
                for i in (0..self.widgets.len()).rev() {
                    if self.focus == Some(WidgetId(i)) {
                        continue;
                    }
                    if self.widgets[i].process(ev, false) {
                        if self.widgets[i].dirty() {
                            any = true;
                        }
                        break;
                    }
                }
            }
            UiEvent::Move { .. } => {}
            UiEvent::KeyDown { .. } | UiEvent::KeyUp { .. } => {}
        }
        any
    }

    // -- drawing -------------------------------------------------------------

    /// Full redraw: clear to the background, then draw the tree (panels, then widgets).
    pub fn draw(&mut self, win: &mut Window) {
        let _ = win.clear(BG);
        self.draw_node(self.root, 0, 0, win);
    }

    fn draw_node(&mut self, node: NodeId, ax: i32, ay: i32, win: &mut Window) {
        let l = self.taffy.layout(node).expect("taffy: layout");
        let x = ax + l.location.x as i32;
        let y = ay + l.location.y as i32;
        let w = l.size.width as u32;
        let h = l.size.height as u32;

        let ctx = self.taffy.get_node_context(node).copied();
        if let Some(NodeCtx::Panel { bg, border }) = ctx {
            let rect = Rectangle::new(Point::new(x, y), Size::new(w, h));
            let _ = rect
                .into_styled(PrimitiveStyle::with_fill(bg))
                .draw(win);
            let _ = rect
                .into_styled(PrimitiveStyle::with_stroke(border, 1))
                .draw(win);
        }
        if let Some(NodeCtx::Widget(wid)) = ctx {
            self.widgets[wid.0].draw(win);
        }

        if let Ok(children) = self.taffy.children(node) {
            for c in children {
                self.draw_node(c, x, y, win);
            }
        }
    }
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}
