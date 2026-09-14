use std::string::String;

use popugos::window::Window;

use crate::{draw, Color, Constraints, EventResult, Point, Rect, Size, Theme, UiEvent, Widget};

const FONT_W: i32 = 9;
const FONT_H: i32 = 18;
const SCAN_ESC: u8 = 0x01;
const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_ENTER: u8 = 0x1C;
const SCAN_HOME: u8 = 0x47;
const SCAN_LEFT: u8 = 0x4B;
const SCAN_RIGHT: u8 = 0x4D;
const SCAN_END: u8 = 0x4F;
const SCAN_DELETE: u8 = 0x53;
const PAD_X: i32 = 4;

pub struct TextInput {
    text: String,
    rect: Rect,
    dirty: bool,
    focused: bool,
    max_len: usize,
    cursor: usize,
    scroll_col: usize,
    selection_anchor: Option<usize>,
    dragging_selection: bool,
}

impl TextInput {
    pub fn new(text: &str) -> Self {
        Self {
            text: String::from(text),
            rect: Rect::default(),
            dirty: true,
            focused: false,
            max_len: 64,
            cursor: text.len(),
            scroll_col: 0,
            selection_anchor: None,
            dragging_selection: false,
        }
    }
    pub fn text(&self) -> &str { &self.text }
    pub fn cursor(&self) -> usize { self.cursor }
    pub fn set_text(&mut self, text: &str) {
        if self.text != text {
            self.text = String::from(text);
            self.cursor = self.text.len();
            self.selection_anchor = None;
            self.ensure_cursor_visible();
            self.dirty = true;
        }
    }
    pub fn set_max_len(&mut self, max_len: usize) { self.max_len = max_len; }

    fn visible_chars(&self) -> usize {
        ((self.rect.w as i32 - PAD_X * 2).max(FONT_W) / FONT_W).max(1) as usize
    }

    fn cursor_col(&self) -> usize { self.text[..self.cursor].chars().count() }

    fn byte_at_col(&self, col: usize) -> usize {
        self.text.char_indices().nth(col).map(|(index, _)| index).unwrap_or(self.text.len())
    }

    fn previous_char(&self) -> usize {
        if self.cursor == 0 { 0 } else { self.text[..self.cursor].char_indices().next_back().map(|(index, _)| index).unwrap_or(0) }
    }

    fn next_char(&self) -> usize {
        if self.cursor >= self.text.len() {
            self.text.len()
        } else {
            self.cursor + self.text[self.cursor..].chars().next().map(char::len_utf8).unwrap_or(0)
        }
    }

    fn ensure_cursor_visible(&mut self) {
        let col = self.cursor_col();
        let visible = self.visible_chars();
        if col < self.scroll_col {
            self.scroll_col = col;
        } else if col > self.scroll_col + visible {
            self.scroll_col = col - visible;
        }
        let max_scroll = self.text.chars().count().saturating_sub(visible);
        self.scroll_col = self.scroll_col.min(max_scroll);
    }

    fn move_cursor(&mut self, cursor: usize) {
        let cursor = cursor.min(self.text.len());
        if self.cursor != cursor {
            self.cursor = cursor;
            self.ensure_cursor_visible();
            self.dirty = true;
        }
    }

    fn place_cursor(&mut self, x: i32) {
        let local = (x - self.rect.x - PAD_X).max(0);
        let clicked_col = self.scroll_col + ((local + FONT_W / 2) / FONT_W) as usize;
        self.move_cursor(self.byte_at_col(clicked_col));
    }

    fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        (anchor != self.cursor).then_some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    fn clear_selection(&mut self) { self.selection_anchor = None; }

    fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else { return false; };
        self.text.replace_range(start..end, "");
        self.cursor = start;
        self.selection_anchor = None;
        self.ensure_cursor_visible();
        self.dirty = true;
        true
    }

    fn prepare_navigation(&mut self, shift: bool) {
        if shift {
            if self.selection_anchor.is_none() { self.selection_anchor = Some(self.cursor); }
        } else {
            self.clear_selection();
        }
    }
}

impl Widget for TextInput {
    fn measure(&self, constraints: Constraints) -> Size {
        constraints.clamp(Size::new(self.text.chars().count().max(8) as f32 * FONT_W as f32 + 12.0, 26.0))
    }
    fn set_rect(&mut self, rect: Rect) {
        if self.rect != rect {
            self.rect = rect;
            self.ensure_cursor_visible();
            self.dirty = true;
        }
    }
    fn rect(&self) -> Rect { self.rect }

    fn draw(&self, window: &mut Window, theme: &Theme) {
        if self.rect.w == 0 || self.rect.h == 0 { return; }
        draw::fill_rect(window, self.rect, theme.input_background);
        draw::stroke_rect(window, self.rect, if self.focused { theme.input_border_focus } else { theme.input_border }, 1);

        let max_chars = self.visible_chars();
        let start = self.byte_at_col(self.scroll_col);
        let end = self.text[start..].char_indices().nth(max_chars).map(|(i, _)| start + i).unwrap_or(self.text.len());
        let shown = &self.text[start..end];
        let y = self.rect.y + (self.rect.h as i32 - FONT_H) / 2;
        if let Some((selection_start, selection_end)) = self.selection() {
            let first_col = self.text[..selection_start].chars().count();
            let last_col = self.text[..selection_end].chars().count();
            let visible_start = first_col.max(self.scroll_col);
            let visible_end = last_col.min(self.scroll_col + max_chars);
            if visible_start < visible_end {
                draw::fill_rect(window, Rect::new(
                    self.rect.x + PAD_X + (visible_start - self.scroll_col) as i32 * FONT_W,
                    y,
                    ((visible_end - visible_start) as i32 * FONT_W) as u32,
                    FONT_H as u32,
                ), Color::new(0x9f, 0xc5, 0xef));
            }
        }
        draw::text(window, Point::new(self.rect.x + PAD_X, y.max(self.rect.y + 2)), shown, theme.text);
        if self.focused {
            let screen_col = self.cursor_col().saturating_sub(self.scroll_col).min(max_chars);
            let x = self.rect.x + PAD_X + screen_col as i32 * FONT_W;
            draw::fill_rect(window, Rect::new(x, self.rect.y + 4, 1, self.rect.h.saturating_sub(8)), theme.text);
        }
    }

    fn event(&mut self, event: &UiEvent, focused: bool) -> EventResult {
        if !focused {
            return match *event {
                UiEvent::Down { x, y } if self.rect.contains(x, y) => EventResult::Consumed,
                _ => EventResult::Ignored,
            };
        }
        match *event {
            UiEvent::Down { x, y } if self.rect.contains(x, y) => {
                self.place_cursor(x);
                self.selection_anchor = Some(self.cursor);
                self.dragging_selection = true;
                EventResult::Consumed
            }
            UiEvent::Move { x, .. } if self.dragging_selection => {
                let left = self.rect.x + PAD_X;
                let right = self.rect.x + self.rect.w as i32 - PAD_X;
                if x < left {
                    self.scroll_col = self.scroll_col.saturating_sub(1);
                    self.move_cursor(self.byte_at_col(self.scroll_col));
                } else if x > right {
                    self.move_cursor(self.byte_at_col(self.scroll_col + self.visible_chars() + 1));
                } else {
                    self.place_cursor(x);
                }
                EventResult::Consumed
            }
            UiEvent::Up { .. } if self.dragging_selection => {
                self.dragging_selection = false;
                self.dirty = true;
                EventResult::Consumed
            }
            UiEvent::KeyDown { scancode, ch, mods } => {
                if scancode == SCAN_ESC { return EventResult::Ignored; }
                // A printable character is authoritative.  Navigation/editing
                // scancodes are only considered for non-printable key events.
                if ch >= 0x20 && ch < 0x7f && (self.text.len() < self.max_len || self.selection().is_some()) {
                    self.delete_selection();
                    if self.text.len() >= self.max_len { return EventResult::Consumed; }
                    self.text.insert(self.cursor, ch as char);
                    self.cursor += 1;
                    self.ensure_cursor_visible();
                    self.dirty = true;
                    return EventResult::Changed;
                }
                if scancode == SCAN_BACKSPACE {
                    if self.delete_selection() { return EventResult::Changed; }
                    if self.cursor > 0 {
                        let previous = self.previous_char();
                        self.text.replace_range(previous..self.cursor, "");
                        self.cursor = previous;
                        self.ensure_cursor_visible();
                        self.dirty = true;
                        return EventResult::Changed;
                    }
                    return EventResult::Consumed;
                }
                if scancode == SCAN_DELETE {
                    if self.delete_selection() { return EventResult::Changed; }
                    if self.cursor < self.text.len() {
                        let next = self.next_char();
                        self.text.replace_range(self.cursor..next, "");
                        self.ensure_cursor_visible();
                        self.dirty = true;
                        return EventResult::Changed;
                    }
                    return EventResult::Consumed;
                }
                if scancode == SCAN_ENTER { return EventResult::Submitted; }
                let shift = mods & 1 != 0;
                if scancode == SCAN_LEFT { self.prepare_navigation(shift); self.move_cursor(self.previous_char()); return EventResult::Consumed; }
                if scancode == SCAN_RIGHT { self.prepare_navigation(shift); self.move_cursor(self.next_char()); return EventResult::Consumed; }
                if scancode == SCAN_HOME { self.prepare_navigation(shift); self.move_cursor(0); return EventResult::Consumed; }
                if scancode == SCAN_END { self.prepare_navigation(shift); self.move_cursor(self.text.len()); return EventResult::Consumed; }
                EventResult::Consumed
            }
            UiEvent::KeyUp { .. } | UiEvent::Down { .. } | UiEvent::Up { .. } => EventResult::Consumed,
            UiEvent::Context { .. }
            | UiEvent::Move { .. }
            | UiEvent::Leave
            | UiEvent::Wheel { .. }
            | UiEvent::Resize { .. } => EventResult::Ignored,
        }
    }
    fn focusable(&self) -> bool { true }
    fn set_focused(&mut self, focused: bool) { if self.focused != focused { self.focused = focused; self.dirty = true; } }
    fn dirty(&self) -> bool { self.dirty }
    fn clear_dirty(&mut self) { self.dirty = false; }
    fn as_any(&self) -> &dyn core::any::Any { self }
    fn as_any_mut(&mut self) -> &mut dyn core::any::Any { self }
}
