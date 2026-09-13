use std::string::String;

use popugos::window::Window;

use crate::{draw, Color, Constraints, EventResult, Point, Rect, Size, Theme, UiEvent, Widget};

const FONT_W: i32 = 9;
const FONT_H: i32 = 18;
const LINE_H: i32 = 20;
const PAD: i32 = 4;
const SCROLLBAR_W: i32 = 10;

const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_TAB: u8 = 0x0F;
const SCAN_ENTER: u8 = 0x1C;
const SCAN_HOME: u8 = 0x47;
const SCAN_UP: u8 = 0x48;
const SCAN_PAGE_UP: u8 = 0x49;
const SCAN_LEFT: u8 = 0x4B;
const SCAN_RIGHT: u8 = 0x4D;
const SCAN_END: u8 = 0x4F;
const SCAN_DOWN: u8 = 0x50;
const SCAN_PAGE_DOWN: u8 = 0x51;
const SCAN_DELETE: u8 = 0x53;

pub struct TextArea {
    text: String,
    rect: Rect,
    cursor: usize,
    scroll_line: usize,
    scroll_col: usize,
    max_len: usize,
    modified: bool,
    focused: bool,
    dragging_scrollbar: bool,
    scrollbar_grab_y: i32,
    dirty: bool,
}

impl TextArea {
    pub fn new() -> Self { Self::with_text("") }

    pub fn with_text(text: &str) -> Self {
        Self {
            text: String::from(text),
            rect: Rect::default(),
            cursor: text.len(),
            scroll_line: 0,
            scroll_col: 0,
            max_len: 256 * 1024,
            modified: false,
            focused: false,
            dragging_scrollbar: false,
            scrollbar_grab_y: 0,
            dirty: true,
        }
    }

    pub fn text(&self) -> &str { &self.text }
    pub fn is_modified(&self) -> bool { self.modified }
    pub fn mark_saved(&mut self) { self.modified = false; }
    pub fn cursor(&self) -> usize { self.cursor }

    pub fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.cursor = self.text.len();
        self.scroll_line = 0;
        self.scroll_col = 0;
        self.modified = false;
        self.dirty = true;
    }

    pub fn set_owned_text(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.len();
        self.scroll_line = 0;
        self.scroll_col = 0;
        self.modified = false;
        self.dirty = true;
    }

    pub fn clear(&mut self) { self.set_text(""); }
    pub fn set_max_len(&mut self, max_len: usize) { self.max_len = max_len; }

    pub fn cursor_line_col(&self) -> (usize, usize) {
        let mut line = 0;
        let mut col = 0;
        for ch in self.text[..self.cursor].chars() {
            if ch == '\n' { line += 1; col = 0; } else { col += 1; }
        }
        (line, col)
    }

    fn line_count(&self) -> usize { self.text.as_bytes().iter().filter(|&&b| b == b'\n').count() + 1 }
    fn visible_rows(&self) -> usize { ((self.rect.h as i32 - PAD * 2).max(LINE_H) / LINE_H).max(1) as usize }
    fn viewport_cols(&self) -> usize {
        let sb = if self.max_scroll_line() > 0 { SCROLLBAR_W } else { 0 };
        ((self.rect.w as i32 - PAD * 2 - sb).max(FONT_W) / FONT_W).max(1) as usize
    }
    fn max_scroll_line(&self) -> usize { self.line_count().saturating_sub(self.visible_rows()) }

    fn line_bounds(&self, wanted: usize) -> Option<(usize, usize)> {
        let mut line = 0;
        let mut start = 0;
        for (i, &b) in self.text.as_bytes().iter().enumerate() {
            if b == b'\n' {
                if line == wanted { return Some((start, i)); }
                line += 1;
                start = i + 1;
            }
        }
        (line == wanted).then_some((start, self.text.len()))
    }

    fn byte_at_line_col(&self, line: usize, col: usize) -> usize {
        let Some((start, end)) = self.line_bounds(line) else { return self.text.len(); };
        self.text[start..end].char_indices().nth(col).map(|(i, _)| start + i).unwrap_or(end)
    }

    fn prev_char(&self, at: usize) -> usize {
        if at == 0 { 0 } else { self.text[..at].char_indices().next_back().map(|(i, _)| i).unwrap_or(0) }
    }
    fn next_char(&self, at: usize) -> usize {
        if at >= self.text.len() { self.text.len() } else { at + self.text[at..].chars().next().map(char::len_utf8).unwrap_or(0) }
    }

    fn ensure_cursor_visible(&mut self) {
        let (line, col) = self.cursor_line_col();
        let rows = self.visible_rows().max(1);
        if line < self.scroll_line { self.scroll_line = line; }
        else if line >= self.scroll_line + rows { self.scroll_line = line + 1 - rows; }
        self.scroll_line = self.scroll_line.min(self.max_scroll_line());
        let cols = self.viewport_cols().max(1);
        if col < self.scroll_col { self.scroll_col = col; }
        else if col >= self.scroll_col + cols { self.scroll_col = col + 1 - cols; }
    }

    fn insert_char(&mut self, ch: char) {
        if self.text.len().saturating_add(ch.len_utf8()) > self.max_len { return; }
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.modified = true; self.ensure_cursor_visible(); self.dirty = true;
    }

    fn insert_str(&mut self, value: &str) {
        if self.text.len().saturating_add(value.len()) > self.max_len { return; }
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
        self.modified = true; self.ensure_cursor_visible(); self.dirty = true;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 { return; }
        let prev = self.prev_char(self.cursor);
        self.text.replace_range(prev..self.cursor, "");
        self.cursor = prev;
        self.modified = true; self.ensure_cursor_visible(); self.dirty = true;
    }

    fn delete(&mut self) {
        if self.cursor >= self.text.len() { return; }
        let next = self.next_char(self.cursor);
        self.text.replace_range(self.cursor..next, "");
        self.modified = true; self.ensure_cursor_visible(); self.dirty = true;
    }

    fn move_vertical(&mut self, delta: isize) {
        let (line, col) = self.cursor_line_col();
        let last = self.line_count().saturating_sub(1);
        let next = if delta < 0 { line.saturating_sub(delta.unsigned_abs()) } else { line.saturating_add(delta as usize).min(last) };
        self.cursor = self.byte_at_line_col(next, col);
        self.ensure_cursor_visible(); self.dirty = true;
    }

    fn click_to_cursor(&mut self, x: i32, y: i32) {
        let row = ((y - self.rect.y - PAD).max(0) / LINE_H) as usize;
        let line = (self.scroll_line + row).min(self.line_count().saturating_sub(1));
        let col = self.scroll_col + ((x - self.rect.x - PAD).max(0) / FONT_W) as usize;
        self.cursor = self.byte_at_line_col(line, col);
        self.ensure_cursor_visible(); self.dirty = true;
    }

    fn scrollbar_rect(&self) -> Option<Rect> {
        (self.max_scroll_line() > 0 && self.rect.w >= SCROLLBAR_W as u32).then_some(Rect::new(
            self.rect.x + self.rect.w as i32 - SCROLLBAR_W,
            self.rect.y,
            SCROLLBAR_W as u32,
            self.rect.h,
        ))
    }

    fn scrollbar_thumb(&self) -> Option<Rect> {
        let bar = self.scrollbar_rect()?;
        let rows = self.visible_rows().max(1);
        let total = self.line_count().max(1);
        let h = bar.h as i32;
        let thumb_h = ((h as i64 * rows as i64) / total as i64).max(14).min(h as i64) as i32;
        let travel = (h - thumb_h).max(0);
        let max = self.max_scroll_line();
        let top = if max == 0 { 0 } else { (self.scroll_line as i64 * travel as i64 / max as i64) as i32 };
        Some(Rect::new(bar.x, bar.y + top, bar.w, thumb_h as u32))
    }

    fn scroll_from_thumb_top(&mut self, top: i32) {
        let Some(bar) = self.scrollbar_rect() else { return; };
        let Some(thumb) = self.scrollbar_thumb() else { return; };
        let travel = (bar.h as i32 - thumb.h as i32).max(0);
        if travel == 0 { self.scroll_line = 0; return; }
        let local = (top - bar.y).clamp(0, travel);
        self.scroll_line = (local as i64 * self.max_scroll_line() as i64 / travel as i64) as usize;
        self.dirty = true;
    }
}

impl Default for TextArea { fn default() -> Self { Self::new() } }

impl Widget for TextArea {
    fn measure(&self, constraints: Constraints) -> Size { constraints.clamp(Size::new(320.0, 220.0)) }

    fn set_rect(&mut self, rect: Rect) {
        if self.rect != rect { self.rect = rect; self.scroll_line = self.scroll_line.min(self.max_scroll_line()); self.ensure_cursor_visible(); self.dirty = true; }
    }
    fn rect(&self) -> Rect { self.rect }

    fn draw(&self, window: &mut Window, theme: &Theme) {
        if self.rect.w == 0 || self.rect.h == 0 { return; }
        let bg = Color::new(0xff, 0xff, 0xff);
        let fg = Color::new(0x18, 0x18, 0x18);
        draw::fill_rect(window, self.rect, bg);
        draw::stroke_rect(window, self.rect, if self.focused { theme.input_border_focus } else { theme.input_border }, 1);

        let rows = self.visible_rows();
        for (line_no, line) in self.text.split('\n').enumerate().skip(self.scroll_line).take(rows) {
            let start = line.char_indices().nth(self.scroll_col).map(|(i, _)| i).unwrap_or(line.len());
            let rest = &line[start..];
            let end = rest.char_indices().nth(self.viewport_cols()).map(|(i, _)| i).unwrap_or(rest.len());
            draw::text(window, Point::new(self.rect.x + PAD, self.rect.y + PAD + (line_no - self.scroll_line) as i32 * LINE_H), &rest[..end], fg);
        }

        if self.focused {
            let (line, col) = self.cursor_line_col();
            if line >= self.scroll_line && line < self.scroll_line + rows {
                let screen_col = col.saturating_sub(self.scroll_col);
                if screen_col <= self.viewport_cols() {
                    draw::fill_rect(window, Rect::new(
                        self.rect.x + PAD + screen_col as i32 * FONT_W,
                        self.rect.y + PAD + (line - self.scroll_line) as i32 * LINE_H,
                        1,
                        FONT_H as u32,
                    ), fg);
                }
            }
        }

        if let Some(bar) = self.scrollbar_rect() {
            draw::fill_rect(window, bar, theme.scrollbar_background);
            if let Some(thumb) = self.scrollbar_thumb() { draw::fill_rect(window, thumb, theme.scrollbar_thumb); }
        }
    }

    fn event(&mut self, event: &UiEvent, focused: bool) -> EventResult {
        if self.focused != focused { self.focused = focused; self.dirty = true; }
        match *event {
            UiEvent::Down { x, y } if self.rect.contains(x, y) => {
                if let Some(thumb) = self.scrollbar_thumb() {
                    if thumb.contains(x, y) { self.dragging_scrollbar = true; self.scrollbar_grab_y = y - thumb.y; return EventResult::Consumed; }
                }
                if let Some(bar) = self.scrollbar_rect() {
                    if bar.contains(x, y) {
                        let grab = self.scrollbar_thumb().map(|t| t.h as i32 / 2).unwrap_or(0);
                        self.scroll_from_thumb_top(y - grab); self.dragging_scrollbar = true; self.scrollbar_grab_y = grab; return EventResult::Changed;
                    }
                }
                self.click_to_cursor(x, y); EventResult::Consumed
            }
            UiEvent::Move { y, .. } if self.dragging_scrollbar => { self.scroll_from_thumb_top(y - self.scrollbar_grab_y); EventResult::Changed }
            UiEvent::Up { .. } if self.dragging_scrollbar => { self.dragging_scrollbar = false; EventResult::Consumed }
            UiEvent::Wheel { delta, .. } => {
                let lines = (delta.unsigned_abs() as usize).saturating_mul(3);
                let next = if delta > 0 {
                    self.scroll_line.saturating_add(lines).min(self.max_scroll_line())
                } else {
                    self.scroll_line.saturating_sub(lines)
                };
                if next != self.scroll_line {
                    self.scroll_line = next;
                    self.dirty = true;
                    EventResult::Changed
                } else {
                    EventResult::Consumed
                }
            }
            UiEvent::KeyDown { scancode, ch, .. } if focused => {
                match scancode {
                    SCAN_BACKSPACE => self.backspace(),
                    SCAN_DELETE => self.delete(),
                    SCAN_ENTER => self.insert_char('\n'),
                    SCAN_TAB => self.insert_str("    "),
                    SCAN_LEFT => { self.cursor = self.prev_char(self.cursor); self.ensure_cursor_visible(); self.dirty = true; }
                    SCAN_RIGHT => { self.cursor = self.next_char(self.cursor); self.ensure_cursor_visible(); self.dirty = true; }
                    SCAN_UP => self.move_vertical(-1),
                    SCAN_DOWN => self.move_vertical(1),
                    SCAN_PAGE_UP => self.move_vertical(-(self.visible_rows() as isize)),
                    SCAN_PAGE_DOWN => self.move_vertical(self.visible_rows() as isize),
                    SCAN_HOME => { let (line, _) = self.cursor_line_col(); self.cursor = self.byte_at_line_col(line, 0); self.ensure_cursor_visible(); self.dirty = true; }
                    SCAN_END => { let (line, _) = self.cursor_line_col(); if let Some((_, end)) = self.line_bounds(line) { self.cursor = end; self.ensure_cursor_visible(); self.dirty = true; } }
                    _ if (0x20..0x7f).contains(&ch) => self.insert_char(ch as char),
                    _ => return EventResult::Consumed,
                }
                EventResult::Changed
            }
            UiEvent::KeyUp { .. } if focused => EventResult::Consumed,
            _ => EventResult::Ignored,
        }
    }

    fn focusable(&self) -> bool { true }
    fn set_focused(&mut self, focused: bool) { if self.focused != focused { self.focused = focused; self.dirty = true; } }
    fn dirty(&self) -> bool { self.dirty }
    fn clear_dirty(&mut self) { self.dirty = false; }
    fn as_any(&self) -> &dyn core::any::Any { self }
    fn as_any_mut(&mut self) -> &mut dyn core::any::Any { self }
}
