//! Retained-mode UI for Felix windows.
//!
//! Focus + event bubble:
//! 1. MouseDown hit-tests → sets focus on the widget under the cursor
//! 2. Event goes to the focused widget first (`process` → `true` = consumed)
//! 3. If not consumed → default handling (hover, unfocus on empty click, …)
//!
//! ```ignore
//! let mut ui = Ui::new();
//! let btn = ui.add_button(Button::new(20, 20, 120, 28, "click"));
//! let lbl = ui.add_label(Label::new(20, 60, "count: 0"));
//! let input = ui.add_text_input(TextInput::new(20, 100, 200, 24));
//!
//! let mut count = 0u32;
//! ui.on_click(btn, move |ui| {
//!     count += 1;
//!     ui.set_label(lbl, &alloc::format!("count: {count}"));
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

use crate::println;
use crate::wm::{
    mouse, Window, WmEvent, EV_KEY_DOWN, EV_KEY_UP, EV_MOUSE_DOWN, EV_MOUSE_MOVE, EV_MOUSE_UP,
    TITLE_H,
};
use embedded_graphics::{
    mono_font::MonoTextStyle,
    pixelcolor::Rgb888,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle, RoundedRectangle},
    text::{Baseline, Text},
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

const SCAN_ESC: u8 = 0x01;
const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_ENTER: u8 = 0x1C;

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiEvent {
    Down {
        x: i32,
        y: i32,
    },
    Move {
        x: i32,
        y: i32,
    },
    Up {
        x: i32,
        y: i32,
    },
    /// `ch` = 0 if no printable; `mods`: bit0=shift, bit1=ctrl
    KeyDown {
        scancode: u8,
        ch: u8,
        mods: u8,
    },
    KeyUp {
        scancode: u8,
        ch: u8,
        mods: u8,
    },
}

// ---------------------------------------------------------------------------
// Ids
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WidgetId(usize);

// ---------------------------------------------------------------------------
// Button
// ---------------------------------------------------------------------------

pub struct Button {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    label: String,
    dirty: bool,
    hot: bool,
    down: bool,
    /// Set when a click completes; consumed by callback dispatch.
    pending_click: bool,
}

impl Button {
    pub fn new(x: i32, y: i32, w: u32, h: u32, label: &str) -> Self {
        Self {
            x,
            y,
            w,
            h,
            label: String::from(label),
            dirty: true,
            hot: false,
            down: false,
            pending_click: false,
        }
    }

    pub fn set_label(&mut self, s: &str) {
        if self.label != s {
            self.label = String::from(s);
            self.dirty = true;
        }
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w as i32 && y < self.y + self.h as i32
    }

    /// Returns `true` if the event was consumed.
    fn process(&mut self, ev: &UiEvent, focused: bool) -> bool {
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
            UiEvent::KeyDown { .. } | UiEvent::KeyUp { .. } if focused => {
                // buttons ignore keys
                false
            }
            _ => false,
        }
    }

    fn draw(&self, win: &mut Window) {
        let bg = if self.down && self.hot {
            BTN_BG_DOWN
        } else if self.hot {
            BTN_BG_HOT
        } else {
            BTN_BG
        };

        let rect = Rectangle::new(Point::new(self.x, self.y), Size::new(self.w, self.h));
        let rr = RoundedRectangle::with_equal_corners(rect, Size::new(4, 4));
        let _ = rr.into_styled(PrimitiveStyle::with_fill(bg)).draw(win);
        let _ = rr
            .into_styled(PrimitiveStyle::with_stroke(BTN_BORDER, 1))
            .draw(win);
        let binding = mono_9x18_atlas();
        let style = MonoTextStyle::new(&binding, TEXT);
        let tw = (self.label.len() as i32) * 9;
        let tx = self.x + (self.w as i32 - tw) / 2;
        let ty = self.y + (self.h as i32 - 15) / 2;
        let _ = Text::with_baseline(
            self.label.as_str(),
            Point::new(tx.max(self.x + 4), ty.max(self.y + 2)),
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
    pub x: i32,
    pub y: i32,
    text: String,
    dirty: bool,
    fg: Rgb888,
    clear_w: u32,
}

impl Label {
    pub fn new(x: i32, y: i32, text: &str) -> Self {
        let clear_w = (text.len() as u32).saturating_mul(9).saturating_add(4);
        Self {
            x,
            y,
            text: String::from(text),
            dirty: true,
            fg: LABEL_FG,
            clear_w,
        }
    }

    pub fn set_text(&mut self, s: &str) {
        if self.text != s {
            let nw = (s.len() as u32).saturating_mul(9).saturating_add(4);
            self.clear_w = self.clear_w.max(nw);
            self.text = String::from(s);
            self.dirty = true;
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_color(&mut self, c: Rgb888) {
        self.fg = c;
        self.dirty = true;
    }

    fn draw(&self, win: &mut Window) {
        let _ = Rectangle::new(Point::new(self.x, self.y), Size::new(self.clear_w, 16))
            .into_styled(PrimitiveStyle::with_fill(BG))
            .draw(win);
        let binding = mono_9x18_atlas();

        let style = MonoTextStyle::new(&binding, self.fg);
        let _ = Text::with_baseline(
            self.text.as_str(),
            Point::new(self.x, self.y),
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
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    text: String,
    dirty: bool,
    focused: bool,
    max_len: usize,
    /// Set when Enter is pressed while focused; consumed by `take_submit`.
    pending_submit: bool,
}

impl TextInput {
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self {
            x,
            y,
            w,
            h: h.max(20),
            text: String::new(),
            dirty: true,
            focused: false,
            max_len: 64,
            pending_submit: false,
        }
    }

    pub fn with_text(mut self, s: &str) -> Self {
        self.text = String::from(s);
        self
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

    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w as i32 && y < self.y + self.h as i32
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
                    // bubble → default clears focus
                    return false;
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
                // unknown key while focused — still consume to avoid leaking to others
                true
            }
            UiEvent::KeyUp { .. } => true,
            UiEvent::Down { .. } => true, // already focused by hit-test
            UiEvent::Up { .. } => true,
            UiEvent::Move { .. } => false,
        }
    }

    fn draw(&self, win: &mut Window) {
        let border = if self.focused {
            INPUT_BORDER_FOCUS
        } else {
            INPUT_BORDER
        };
        let rect = Rectangle::new(Point::new(self.x, self.y), Size::new(self.w, self.h));
        let _ = rect
            .into_styled(PrimitiveStyle::with_fill(INPUT_BG))
            .draw(win);
        let _ = rect
            .into_styled(PrimitiveStyle::with_stroke(border, 1))
            .draw(win);
        let binding = mono_9x18_atlas();
        let style = MonoTextStyle::new(&binding, TEXT);
        let ty = self.y + (self.h as i32 - 15) / 2;
        let mut shown = self.text.as_str();
        // crude clip by pixel width
        let max_chars = (self.w as usize).saturating_sub(8) / 9;
        if shown.len() > max_chars {
            shown = &shown[shown.len() - max_chars..];
        }
        let _ = Text::with_baseline(
            shown,
            Point::new(self.x + 4, ty.max(self.y + 2)),
            style,
            Baseline::Top,
        )
        .draw(win);

        // caret when focused
        if self.focused {
            let cx = self.x + 4 + (shown.len() as i32) * 9;
            let _ = Rectangle::new(
                Point::new(cx, self.y + 4),
                Size::new(2, self.h.saturating_sub(8)),
            )
            .into_styled(PrimitiveStyle::with_fill(TEXT))
            .draw(win);
        }
    }
}

// ---------------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------------

enum Node {
    Button(Button),
    Label(Label),
    TextInput(TextInput),
}

impl Node {
    fn dirty(&self) -> bool {
        match self {
            Node::Button(b) => b.dirty,
            Node::Label(l) => l.dirty,
            Node::TextInput(t) => t.dirty,
        }
    }

    fn clear_dirty(&mut self) {
        match self {
            Node::Button(b) => b.dirty = false,
            Node::Label(l) => l.dirty = false,
            Node::TextInput(t) => t.dirty = false,
        }
    }

    fn mark_dirty(&mut self) {
        match self {
            Node::Button(b) => b.dirty = true,
            Node::Label(l) => l.dirty = true,
            Node::TextInput(t) => t.dirty = true,
        }
    }

    fn draw(&self, win: &mut Window) {
        match self {
            Node::Button(b) => b.draw(win),
            Node::Label(l) => l.draw(win),
            Node::TextInput(t) => t.draw(win),
        }
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        match self {
            Node::Button(b) => b.contains(x, y),
            Node::TextInput(t) => t.contains(x, y),
            Node::Label(_) => false,
        }
    }

    fn focusable(&self) -> bool {
        matches!(self, Node::Button(_) | Node::TextInput(_))
    }

    fn set_focused_visual(&mut self, f: bool) {
        if let Node::TextInput(t) = self {
            t.set_focused(f);
        }
    }

    fn process(&mut self, ev: &UiEvent, focused: bool) -> bool {
        match self {
            Node::Button(b) => b.process(ev, focused),
            Node::TextInput(t) => t.process(ev, focused),
            Node::Label(_) => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Ui
// ---------------------------------------------------------------------------

pub struct Ui {
    nodes: Vec<Node>,
    focus: Option<WidgetId>,
    /// on_click handlers (index = WidgetId.0); only for buttons.
    /// Signature `FnMut(&mut Ui)` — handler is taken out of the vec while called
    /// so it can safely touch other widgets via `&mut Ui`.
    clicks: Vec<Option<Box<dyn FnMut(&mut Ui)>>>,
    needs_full: bool,
    /// legacy: ids that completed a click this process tick
    clicked: Vec<WidgetId>,
}

impl Ui {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            focus: None,
            clicks: Vec::new(),
            needs_full: true,
            clicked: Vec::new(),
        }
    }

    pub fn add_button(&mut self, btn: Button) -> WidgetId {
        let id = WidgetId(self.nodes.len());
        self.nodes.push(Node::Button(btn));
        self.clicks.push(None);
        self.needs_full = true;
        id
    }

    pub fn add_label(&mut self, lbl: Label) -> WidgetId {
        let id = WidgetId(self.nodes.len());
        self.nodes.push(Node::Label(lbl));
        self.clicks.push(None);
        self.needs_full = true;
        id
    }

    pub fn add_text_input(&mut self, t: TextInput) -> WidgetId {
        let id = WidgetId(self.nodes.len());
        self.nodes.push(Node::TextInput(t));
        self.clicks.push(None);
        self.needs_full = true;
        id
    }

    /// Register a click handler for a button. Replaces any previous handler.
    ///
    /// The closure receives `&mut Ui` and may update other widgets (labels, inputs, …).
    /// Capture your own counters with `move |ui| { count += 1; ... }`.
    pub fn on_click<F: FnMut(&mut Ui) + 'static>(&mut self, id: WidgetId, f: F) {
        if id.0 < self.clicks.len() {
            self.clicks[id.0] = Some(Box::new(f));
        }
    }

    pub fn button_mut(&mut self, id: WidgetId) -> Option<&mut Button> {
        match self.nodes.get_mut(id.0)? {
            Node::Button(b) => Some(b),
            _ => None,
        }
    }

    pub fn label_mut(&mut self, id: WidgetId) -> Option<&mut Label> {
        match self.nodes.get_mut(id.0)? {
            Node::Label(l) => Some(l),
            _ => None,
        }
    }

    pub fn text_input_mut(&mut self, id: WidgetId) -> Option<&mut TextInput> {
        match self.nodes.get_mut(id.0)? {
            Node::TextInput(t) => Some(t),
            _ => None,
        }
    }

    pub fn text(&self, id: WidgetId) -> Option<&str> {
        match self.nodes.get(id.0)? {
            Node::TextInput(t) => Some(t.text()),
            Node::Label(l) => Some(l.text()),
            _ => None,
        }
    }

    pub fn set_label(&mut self, id: WidgetId, s: &str) {
        if let Some(l) = self.label_mut(id) {
            l.set_text(s);
        }
    }

    /// True if the given TextInput received Enter since last check (clears the flag).
    pub fn take_submit(&mut self, id: WidgetId) -> bool {
        if let Some(t) = self.text_input_mut(id) {
            t.take_submit()
        } else {
            false
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
            if let Some(n) = self.nodes.get_mut(old.0) {
                n.set_focused_visual(false);
            }
        }
        self.focus = id;
        if let Some(new) = id {
            if let Some(n) = self.nodes.get_mut(new.0) {
                n.set_focused_visual(true);
            }
        }
    }

    /// Poll window events, dispatch (focus → bubble → default), redraw if needed, flip.
    pub fn process(&mut self, win: &mut Window) {
        self.clicked.clear();
        let mut dirty = self.needs_full;

        let mut buf = [WmEvent::default(); 256];
        let n = win.poll_events(&mut buf);

        // Build UiEvent list (also fallback mouse edge if queue empty)
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

        if events.is_empty() {
            // optional soft fallback for demos without queue traffic
            if let Some(ev) = Self::fallback_mouse(win) {
                events.push(ev);
            }
        }

        for ev in events {
            if self.dispatch(&ev) {
                dirty = true;
            }
        }

        // Fire click callbacks: take handler out so it can borrow `&mut Ui`.
        let mut fired: Vec<usize> = Vec::new();
        for (i, node) in self.nodes.iter_mut().enumerate() {
            if let Node::Button(b) = node {
                if b.pending_click {
                    b.pending_click = false;
                    fired.push(i);
                    self.clicked.push(WidgetId(i));
                }
            }
        }
        for i in fired {
            let mut cb = self.clicks[i].take();
            if let Some(ref mut f) = cb {
                f(self);
            }
            self.clicks[i] = cb;
        }

        // Callbacks may have dirtied labels via exterior Rc — check flags
        if !dirty {
            dirty = self.nodes.iter().any(|n| n.dirty()) || self.needs_full;
        }

        if dirty {
            self.draw(win);
            let _ = win.flip();
        }
    }

    fn fallback_mouse(win: &Window) -> Option<UiEvent> {
        // Static-ish edge detect without storing on Ui: skip if events exist.
        // Keep simple — only used when poll is empty; callers usually get kernel events.
        let _ = (win, mouse);
        None
    }

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
            if fid.0 < self.nodes.len() {
                let consumed = self.nodes[fid.0].process(ev, true);
                if self.nodes[fid.0].dirty() {
                    any = true;
                }
                if consumed {
                    // default hover still useful for other buttons on Move
                    if matches!(ev, UiEvent::Move { .. }) {
                        any |= self.default_hover(ev);
                    }
                    return any;
                }
                // not consumed — e.g. Escape from TextInput
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

    fn hit_test(&self, x: i32, y: i32) -> Option<WidgetId> {
        for (i, n) in self.nodes.iter().enumerate().rev() {
            if n.focusable() && n.contains(x, y) {
                return Some(WidgetId(i));
            }
        }
        None
    }

    fn default_hover(&mut self, ev: &UiEvent) -> bool {
        let mut any = false;
        if let UiEvent::Move { x, y } = *ev {
            for n in self.nodes.iter_mut() {
                if let Node::Button(b) = n {
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

        // Deliver to non-focused widgets that care (buttons under cursor)
        match *ev {
            UiEvent::Down { x, y } | UiEvent::Up { x, y } => {
                for (i, n) in self.nodes.iter_mut().enumerate().rev() {
                    if self.focus == Some(WidgetId(i)) {
                        continue;
                    }
                    if n.process(ev, false) {
                        if n.dirty() {
                            any = true;
                        }
                        break;
                    }
                    let _ = (x, y);
                }
            }
            UiEvent::Move { .. } => {}
            UiEvent::KeyDown { .. } | UiEvent::KeyUp { .. } => {
                // no focus → keys ignored
            }
        }
        any
    }

    /// Legacy helper still works with `process` (filled each tick).
    pub fn clicked(&self, id: WidgetId) -> bool {
        self.clicked.contains(&id)
    }

    pub fn draw(&mut self, win: &mut Window) {
        if self.needs_full {
            let _ = win.clear(BG);
            for n in self.nodes.iter() {
                n.draw(win);
            }
            for n in self.nodes.iter_mut() {
                n.clear_dirty();
            }
            self.needs_full = false;
            return;
        }

        for n in self.nodes.iter() {
            if n.dirty() {
                n.draw(win);
            }
        }
        for n in self.nodes.iter_mut() {
            n.clear_dirty();
        }
    }

    pub fn request_full_redraw(&mut self) {
        self.needs_full = true;
        for n in self.nodes.iter_mut() {
            n.mark_dirty();
        }
    }
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Optional MouseTracker kept for apps that still drive handle() manually
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default)]
pub struct MouseTracker {
    prev_buttons: u8,
    last_x: i32,
    last_y: i32,
}

impl MouseTracker {
    pub const fn new() -> Self {
        Self {
            prev_buttons: 0,
            last_x: 0,
            last_y: 0,
        }
    }

    pub fn poll_ui(&mut self, win: &mut Window) -> Option<UiEvent> {
        let mut buf = [WmEvent::default(); 8];
        let n = win.poll_events(&mut buf);
        for e in &buf[..n] {
            match e.kind {
                EV_MOUSE_DOWN => {
                    self.last_x = e.a;
                    self.last_y = e.b;
                    self.prev_buttons |= 1;
                    return Some(UiEvent::Down { x: e.a, y: e.b });
                }
                EV_MOUSE_UP => {
                    self.last_x = e.a;
                    self.last_y = e.b;
                    self.prev_buttons &= !1;
                    return Some(UiEvent::Up { x: e.a, y: e.b });
                }
                EV_MOUSE_MOVE => {
                    if e.a != self.last_x || e.b != self.last_y {
                        self.last_x = e.a;
                        self.last_y = e.b;
                        return Some(UiEvent::Move { x: e.a, y: e.b });
                    }
                }
                EV_KEY_DOWN => {
                    return Some(UiEvent::KeyDown {
                        scancode: e.a as u8,
                        ch: e.b as u8,
                        mods: e.c as u8,
                    });
                }
                EV_KEY_UP => {
                    return Some(UiEvent::KeyUp {
                        scancode: e.a as u8,
                        ch: e.b as u8,
                        mods: e.c as u8,
                    });
                }
                _ => {}
            }
        }

        let m = mouse();
        let info = win.info();
        let x = m.x - info.x;
        let y = m.y - (info.y + TITLE_H);
        let left = (m.buttons & 1) != 0;
        let prev = (self.prev_buttons & 1) != 0;
        self.prev_buttons = m.buttons;

        if !prev && left {
            self.last_x = x;
            self.last_y = y;
            return Some(UiEvent::Down { x, y });
        }
        if prev && !left {
            self.last_x = x;
            self.last_y = y;
            return Some(UiEvent::Up { x, y });
        }
        if x != self.last_x || y != self.last_y {
            self.last_x = x;
            self.last_y = y;
            return Some(UiEvent::Move { x, y });
        }
        None
    }
}
