use super::font;
use crate::ui::{Constraints, EventResult, Rect, UiEvent, Widget};
use alloc::string::String;
use embedded_graphics::{
    mono_font::MonoTextStyle,
    pixelcolor::Rgb888,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use taffy::geometry::Size as TSize;

const FONT_W: i32 = 9;
const FONT_H: i32 = 18;
const LINE_H: i32 = 20;
const PAD: i32 = 4;
const SCROLLBAR_W: i32 = 10;

const BG: Rgb888 = Rgb888::new(0xFF, 0xFF, 0xFF);
const FG: Rgb888 = Rgb888::new(0x18, 0x18, 0x18);
const BORDER: Rgb888 = Rgb888::new(0x88, 0x88, 0x88);
const BORDER_FOCUS: Rgb888 = Rgb888::new(0x3A, 0x7C, 0xA5);
const CURSOR: Rgb888 = Rgb888::new(0x10, 0x10, 0x10);
const SCROLL_BG: Rgb888 = Rgb888::new(0xE5, 0xE5, 0xE5);
const SCROLL_THUMB: Rgb888 = Rgb888::new(0xA8, 0xA8, 0xA8);

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
    pub fn new() -> Self {
        Self::with_text("")
    }

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

    pub fn text(&self) -> &str {
        &self.text
    }

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

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.scroll_line = 0;
        self.scroll_col = 0;
        self.modified = false;
        self.dirty = true;
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn cursor_line_col(&self) -> (usize, usize) {
        let mut line = 0usize;
        let mut col = 0usize;
        for ch in self.text[..self.cursor].chars() {
            if ch == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    pub fn is_modified(&self) -> bool {
        self.modified
    }

    pub fn mark_saved(&mut self) {
        self.modified = false;
    }

    pub fn set_max_len(&mut self, max_len: usize) {
        self.max_len = max_len;
        if self.text.len() > max_len {
            let mut end = max_len.min(self.text.len());
            while end > 0 && !self.text.is_char_boundary(end) {
                end -= 1;
            }
            self.text.truncate(end);
            self.cursor = self.cursor.min(end);
            self.modified = true;
            self.ensure_cursor_visible();
            self.dirty = true;
        }
    }

    fn line_count(&self) -> usize {
        self.text.as_bytes().iter().filter(|&&b| b == b'\n').count() + 1
    }

    fn visible_rows(&self) -> usize {
        ((self.rect.h as i32 - PAD * 2).max(LINE_H) / LINE_H).max(1) as usize
    }

    fn viewport_cols(&self) -> usize {
        let scrollbar = if self.max_scroll_line() > 0 { SCROLLBAR_W } else { 0 };
        ((self.rect.w as i32 - PAD * 2 - scrollbar).max(FONT_W) / FONT_W).max(1) as usize
    }

    fn max_scroll_line(&self) -> usize {
        self.line_count().saturating_sub(self.visible_rows())
    }

    fn line_bounds(&self, wanted: usize) -> Option<(usize, usize)> {
        let bytes = self.text.as_bytes();
        let mut line = 0usize;
        let mut start = 0usize;

        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                if line == wanted {
                    return Some((start, i));
                }
                line += 1;
                start = i + 1;
            }
        }

        if line == wanted {
            Some((start, bytes.len()))
        } else {
            None
        }
    }

    fn byte_at_line_col(&self, line: usize, col: usize) -> usize {
        let Some((start, end)) = self.line_bounds(line) else {
            return self.text.len();
        };
        let slice = &self.text[start..end];
        match slice.char_indices().nth(col) {
            Some((offset, _)) => start + offset,
            None => end,
        }
    }

    fn prev_char(&self, at: usize) -> usize {
        if at == 0 {
            return 0;
        }
        self.text[..at]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn next_char(&self, at: usize) -> usize {
        if at >= self.text.len() {
            return self.text.len();
        }
        let len = self.text[at..].chars().next().map(char::len_utf8).unwrap_or(0);
        (at + len).min(self.text.len())
    }

    fn move_vertical(&mut self, delta: isize) {
        let (line, col) = self.cursor_line_col();
        let last = self.line_count().saturating_sub(1);
        let next_line = if delta < 0 {
            line.saturating_sub(delta.unsigned_abs())
        } else {
            line.saturating_add(delta as usize).min(last)
        };
        self.cursor = self.byte_at_line_col(next_line, col);
        self.ensure_cursor_visible();
        self.dirty = true;
    }

    fn move_page(&mut self, down: bool) {
        let rows = self.visible_rows().max(1);
        self.move_vertical(if down { rows as isize } else { -(rows as isize) });
    }

    fn ensure_cursor_visible(&mut self) {
        let (line, col) = self.cursor_line_col();
        let rows = self.visible_rows().max(1);
        if line < self.scroll_line {
            self.scroll_line = line;
        } else if line >= self.scroll_line + rows {
            self.scroll_line = line + 1 - rows;
        }
        self.scroll_line = self.scroll_line.min(self.max_scroll_line());

        let cols = self.viewport_cols().max(1);
        if col < self.scroll_col {
            self.scroll_col = col;
        } else if col >= self.scroll_col + cols {
            self.scroll_col = col + 1 - cols;
        }
    }

    fn insert_char(&mut self, ch: char) {
        if self.text.len().saturating_add(ch.len_utf8()) > self.max_len {
            return;
        }
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.modified = true;
        self.ensure_cursor_visible();
        self.dirty = true;
    }

    fn insert_str(&mut self, s: &str) {
        if self.text.len().saturating_add(s.len()) > self.max_len {
            return;
        }
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.modified = true;
        self.ensure_cursor_visible();
        self.dirty = true;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.prev_char(self.cursor);
        self.text.replace_range(prev..self.cursor, "");
        self.cursor = prev;
        self.modified = true;
        self.ensure_cursor_visible();
        self.dirty = true;
    }

    fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.next_char(self.cursor);
        self.text.replace_range(self.cursor..next, "");
        self.modified = true;
        self.ensure_cursor_visible();
        self.dirty = true;
    }

    fn click_to_cursor(&mut self, x: i32, y: i32) {
        let row = ((y - self.rect.y - PAD).max(0) / LINE_H) as usize;
        let line = (self.scroll_line + row).min(self.line_count().saturating_sub(1));
        let col = self.scroll_col + ((x - self.rect.x - PAD).max(0) / FONT_W) as usize;
        self.cursor = self.byte_at_line_col(line, col);
        self.ensure_cursor_visible();
        self.dirty = true;
    }

    fn scrollbar_rect(&self) -> Option<Rect> {
        if self.max_scroll_line() == 0 || self.rect.w < SCROLLBAR_W as u32 {
            return None;
        }
        Some(Rect::new(
            self.rect.x + self.rect.w as i32 - SCROLLBAR_W,
            self.rect.y,
            SCROLLBAR_W as u32,
            self.rect.h,
        ))
    }

    fn scrollbar_thumb(&self) -> Option<Rect> {
        let bar = self.scrollbar_rect()?;
        let total = self.line_count().max(1);
        let rows = self.visible_rows().max(1);
        let h = bar.h as i32;
        let thumb_h = ((h as i64 * rows as i64) / total as i64)
            .max(14)
            .min(h as i64) as i32;
        let travel = (h - thumb_h).max(0);
        let max = self.max_scroll_line();
        let top = if max == 0 {
            0
        } else {
            (self.scroll_line as i64 * travel as i64 / max as i64) as i32
        };
        Some(Rect::new(bar.x, bar.y + top, bar.w, thumb_h as u32))
    }

    fn scroll_from_thumb_top(&mut self, top: i32) {
        let Some(bar) = self.scrollbar_rect() else { return; };
        let Some(thumb) = self.scrollbar_thumb() else { return; };
        let travel = (bar.h as i32 - thumb.h as i32).max(0);
        if travel == 0 {
            self.scroll_line = 0;
            return;
        }
        let local = (top - bar.y).max(0).min(travel);
        let max = self.max_scroll_line();
        self.scroll_line = (local as i64 * max as i64 / travel as i64) as usize;
        self.dirty = true;
    }

    fn line_visible_slice<'a>(&self, line: &'a str) -> &'a str {
        let start = line
            .char_indices()
            .nth(self.scroll_col)
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        let rest = &line[start..];
        let cols = self.viewport_cols();
        let end = rest
            .char_indices()
            .nth(cols)
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        &rest[..end]
    }
}

impl Default for TextArea {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for TextArea {
    fn measure(&self, c: Constraints) -> TSize<f32> {
        c.clamp(TSize {
            width: 320.0,
            height: 220.0,
        })
    }

    fn set_rect(&mut self, rect: Rect) {
        if self.rect != rect {
            self.rect = rect;
            self.scroll_line = self.scroll_line.min(self.max_scroll_line());
            self.ensure_cursor_visible();
            self.dirty = true;
        }
    }

    fn rect(&self) -> Rect {
        self.rect
    }

    fn draw(&self, win: &mut crate::wm::Window) {
        if self.rect.w == 0 || self.rect.h == 0 {
            return;
        }

        let outer = Rectangle::new(
            Point::new(self.rect.x, self.rect.y),
            Size::new(self.rect.w, self.rect.h),
        );
        let _ = outer.into_styled(PrimitiveStyle::with_fill(BG)).draw(win);
        let _ = outer
            .into_styled(PrimitiveStyle::with_stroke(
                if self.focused { BORDER_FOCUS } else { BORDER },
                1,
            ))
            .draw(win);

        let style = MonoTextStyle::new(font(), FG);
        let rows = self.visible_rows();
        for (line_no, line) in self.text.split('\n').enumerate().skip(self.scroll_line).take(rows) {
            let visible = self.line_visible_slice(line);
            let y = self.rect.y + PAD + (line_no - self.scroll_line) as i32 * LINE_H;
            let _ = Text::with_baseline(
                visible,
                Point::new(self.rect.x + PAD, y),
                style,
                Baseline::Top,
            )
            .draw(win);
        }

        if self.focused {
            let (line, col) = self.cursor_line_col();
            if line >= self.scroll_line && line < self.scroll_line + rows {
                let screen_col = col.saturating_sub(self.scroll_col);
                if screen_col <= self.viewport_cols() {
                    let x = self.rect.x + PAD + screen_col as i32 * FONT_W;
                    let y = self.rect.y + PAD + (line - self.scroll_line) as i32 * LINE_H;
                    let _ = Rectangle::new(Point::new(x, y), Size::new(1, FONT_H as u32))
                        .into_styled(PrimitiveStyle::with_fill(CURSOR))
                        .draw(win);
                }
            }
        }

        if let Some(bar) = self.scrollbar_rect() {
            let _ = Rectangle::new(Point::new(bar.x, bar.y), Size::new(bar.w, bar.h))
                .into_styled(PrimitiveStyle::with_fill(SCROLL_BG))
                .draw(win);
            if let Some(thumb) = self.scrollbar_thumb() {
                let _ = Rectangle::new(Point::new(thumb.x, thumb.y), Size::new(thumb.w, thumb.h))
                    .into_styled(PrimitiveStyle::with_fill(SCROLL_THUMB))
                    .draw(win);
            }
        }
    }

    fn event(&mut self, ev: &UiEvent, focused: bool) -> EventResult {
        if self.focused != focused {
            self.focused = focused;
            self.dirty = true;
        }

        match *ev {
            UiEvent::Down { x, y } if self.rect.contains(x, y) => {
                if let Some(thumb) = self.scrollbar_thumb() {
                    if thumb.contains(x, y) {
                        self.dragging_scrollbar = true;
                        self.scrollbar_grab_y = y - thumb.y;
                        return EventResult::Consumed;
                    }
                }
                if let Some(bar) = self.scrollbar_rect() {
                    if bar.contains(x, y) {
                        let grab = self.scrollbar_thumb().map(|t| t.h as i32 / 2).unwrap_or(0);
                        self.scroll_from_thumb_top(y - grab);
                        self.dragging_scrollbar = true;
                        self.scrollbar_grab_y = grab;
                        return EventResult::Changed;
                    }
                }
                self.click_to_cursor(x, y);
                EventResult::Consumed
            }
            UiEvent::Move { y, .. } if self.dragging_scrollbar => {
                self.scroll_from_thumb_top(y - self.scrollbar_grab_y);
                EventResult::Changed
            }
            UiEvent::Up { .. } if self.dragging_scrollbar => {
                self.dragging_scrollbar = false;
                EventResult::Consumed
            }
            UiEvent::KeyDown { scancode, ch, .. } if focused => {
                match scancode {
                    SCAN_BACKSPACE => self.backspace(),
                    SCAN_DELETE => self.delete(),
                    SCAN_ENTER => self.insert_char('\n'),
                    SCAN_TAB => self.insert_str("    "),
                    SCAN_LEFT => {
                        self.cursor = self.prev_char(self.cursor);
                        self.ensure_cursor_visible();
                        self.dirty = true;
                    }
                    SCAN_RIGHT => {
                        self.cursor = self.next_char(self.cursor);
                        self.ensure_cursor_visible();
                        self.dirty = true;
                    }
                    SCAN_UP => self.move_vertical(-1),
                    SCAN_DOWN => self.move_vertical(1),
                    SCAN_PAGE_UP => self.move_page(false),
                    SCAN_PAGE_DOWN => self.move_page(true),
                    SCAN_HOME => {
                        let (line, _) = self.cursor_line_col();
                        self.cursor = self.byte_at_line_col(line, 0);
                        self.ensure_cursor_visible();
                        self.dirty = true;
                    }
                    SCAN_END => {
                        let (line, _) = self.cursor_line_col();
                        if let Some((_, end)) = self.line_bounds(line) {
                            self.cursor = end;
                            self.ensure_cursor_visible();
                            self.dirty = true;
                        }
                    }
                    _ if ch >= 0x20 && ch < 0x7f => self.insert_char(ch as char),
                    _ => return EventResult::Consumed,
                }
                EventResult::Changed
            }
            UiEvent::KeyUp { .. } if focused => EventResult::Consumed,
            _ => EventResult::Ignored,
        }
    }

    fn focusable(&self) -> bool {
        true
    }

    fn set_focused(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.dirty = true;
        }
    }

    fn dirty(&self) -> bool {
        self.dirty
    }

    fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}
