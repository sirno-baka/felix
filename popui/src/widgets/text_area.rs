use std::{string::String, vec::Vec};

use popugos::window::Window;

use crate::{draw, Color, Constraints, EventResult, Point, Rect, Size, Theme, UiEvent, Widget};

const FONT_W: i32 = 9;
const FONT_H: i32 = 18;
const LINE_H: i32 = 20;
const PAD: i32 = 4;
const SCROLLBAR_W: i32 = 10;
const GUTTER_W: i32 = 46;

const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_TAB: u8 = 0x0F;
const SCAN_P: u8 = 0x19;
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
const SCAN_ESCAPE: u8 = 0x01;
const MOD_CTRL: u8 = 1 << 1;

#[derive(Clone, Debug)]
pub struct EditorDiagnostic {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

struct MemberCompletion {
    type_name: String,
    name: String,
    detail: String,
}

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
    selection_anchor: Option<usize>,
    dragging_selection: bool,
    rhai_mode: bool,
    completions: Vec<String>,
    signatures: Vec<String>,
    type_hints: Vec<(String, String)>,
    member_completions: Vec<MemberCompletion>,
    completion_member_type: Option<String>,
    hover: Option<(usize, usize, i32, i32)>,
    completion_matches: Vec<usize>,
    completion_selected: usize,
    completion_prefix_start: usize,
    diagnostic: Option<EditorDiagnostic>,
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
            selection_anchor: None,
            dragging_selection: false,
            rhai_mode: false,
            completions: Vec::new(),
            signatures: Vec::new(),
            type_hints: Vec::new(),
            member_completions: Vec::new(),
            completion_member_type: None,
            hover: None,
            completion_matches: Vec::new(),
            completion_selected: 0,
            completion_prefix_start: 0,
            diagnostic: None,
            dirty: true,
        }
    }

    pub fn text(&self) -> &str { &self.text }
    pub fn is_modified(&self) -> bool { self.modified }
    pub fn mark_saved(&mut self) { self.modified = false; }
    pub fn cursor(&self) -> usize { self.cursor }

    /// Enable the code-editor presentation: line numbers, Rhai token colours,
    /// diagnostics and identifier completion.
    pub fn set_rhai_mode(&mut self, enabled: bool) { self.rhai_mode = enabled; self.dirty = true; }

    pub fn set_completions<I, S>(&mut self, values: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.completions = values.into_iter().map(Into::into).collect();
        self.completions.sort();
        self.completions.dedup();
        self.refresh_completion(false);
        self.dirty = true;
    }

    /// Function signatures shown in completion items and as parameter help.
    pub fn set_signatures<I, S>(&mut self, values: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.signatures = values.into_iter().map(Into::into).collect();
        self.signatures.sort();
        self.signatures.dedup();
        for signature in &self.signatures {
            let name = Self::signature_name(signature);
            if !name.is_empty() { self.completions.push(name.into()); }
        }
        self.completions.sort();
        self.completions.dedup();
        self.dirty = true;
    }

    pub fn set_type_hints<I, N, T>(&mut self, values: I)
    where
        I: IntoIterator<Item = (N, T)>,
        N: Into<String>,
        T: Into<String>,
    {
        self.type_hints = values.into_iter().map(|(name, ty)| (name.into(), ty.into())).collect();
        self.dirty = true;
    }

    /// Register members used by type-aware completion after `value.`.
    pub fn set_type_members<I, T, N, D>(&mut self, values: I)
    where
        I: IntoIterator<Item = (T, N, D)>,
        T: Into<String>,
        N: Into<String>,
        D: Into<String>,
    {
        self.member_completions = values.into_iter().map(|(type_name, name, detail)| MemberCompletion {
            type_name: type_name.into(), name: name.into(), detail: detail.into(),
        }).collect();
        for member in &self.member_completions { self.completions.push(member.name.clone()); }
        self.completions.sort();
        self.completions.dedup();
        self.dirty = true;
    }

    /// Re-indent Rhai source using four spaces per brace level.
    pub fn format_rhai(&mut self) {
        let (cursor_line, cursor_col) = self.cursor_line_col();
        let mut output = String::new();
        let mut indent = 0usize;
        for (line_index, line) in self.text.lines().enumerate() {
            let content = line.trim();
            let display_indent = indent.saturating_sub(usize::from(starts_with_closer(content)));
            if line_index > 0 { output.push('\n'); }
            if !content.is_empty() {
                for _ in 0..display_indent { output.push_str("    "); }
                output.push_str(content);
            }
            let delta = brace_delta(content);
            indent = if delta < 0 { indent.saturating_sub(delta.unsigned_abs()) } else { indent.saturating_add(delta as usize) };
        }
        self.text = output;
        self.cursor = self.byte_at_line_col(cursor_line.min(self.line_count().saturating_sub(1)), cursor_col);
        self.selection_anchor = None;
        self.modified = true;
        self.ensure_cursor_visible();
        self.dirty = true;
    }

    pub fn set_diagnostic(&mut self, diagnostic: Option<EditorDiagnostic>) {
        self.diagnostic = diagnostic;
        self.dirty = true;
    }

    pub fn diagnostic(&self) -> Option<&EditorDiagnostic> { self.diagnostic.as_ref() }

    pub fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.cursor = self.text.len();
        self.scroll_line = 0;
        self.scroll_col = 0;
        self.modified = false;
        self.selection_anchor = None;
        self.hover = None;
        self.completion_matches.clear();
        self.dirty = true;
    }

    pub fn set_owned_text(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.len();
        self.scroll_line = 0;
        self.scroll_col = 0;
        self.modified = false;
        self.selection_anchor = None;
        self.hover = None;
        self.completion_matches.clear();
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
        let gutter = if self.rhai_mode { GUTTER_W } else { 0 };
        ((self.rect.w as i32 - PAD * 2 - sb - gutter).max(FONT_W) / FONT_W).max(1) as usize
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
        let selected = self.selection().map(|(start, end)| end - start).unwrap_or(0);
        if self.text.len().saturating_sub(selected).saturating_add(ch.len_utf8()) > self.max_len { return; }
        self.delete_selection();
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.modified = true; self.ensure_cursor_visible(); self.refresh_completion(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'); self.dirty = true;
    }

    fn insert_str(&mut self, value: &str) {
        let selected = self.selection().map(|(start, end)| end - start).unwrap_or(0);
        if self.text.len().saturating_sub(selected).saturating_add(value.len()) > self.max_len { return; }
        self.delete_selection();
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
        self.modified = true; self.ensure_cursor_visible(); self.refresh_completion(false); self.dirty = true;
    }

    fn insert_newline(&mut self) {
        self.delete_selection();
        let line_start = self.text[..self.cursor].rfind('\n').map(|index| index + 1).unwrap_or(0);
        let line = &self.text[line_start..self.cursor];
        let leading = line.chars().take_while(|ch| *ch == ' ' || *ch == '\t').collect::<String>();
        let extra = line.trim_end().ends_with(['{', '[', '(']);
        let mut insertion = String::from("\n");
        insertion.push_str(&leading);
        if extra { insertion.push_str("    "); }
        self.insert_str(&insertion);
    }

    fn backspace(&mut self) {
        if self.delete_selection() { self.modified = true; self.refresh_completion(true); return; }
        if self.cursor == 0 { return; }
        let prev = self.prev_char(self.cursor);
        self.text.replace_range(prev..self.cursor, "");
        self.cursor = prev;
        self.modified = true; self.ensure_cursor_visible(); self.refresh_completion(true); self.dirty = true;
    }

    fn delete(&mut self) {
        if self.delete_selection() { self.modified = true; self.refresh_completion(false); return; }
        if self.cursor >= self.text.len() { return; }
        let next = self.next_char(self.cursor);
        self.text.replace_range(self.cursor..next, "");
        self.modified = true; self.ensure_cursor_visible(); self.refresh_completion(false); self.dirty = true;
    }

    fn move_vertical(&mut self, delta: isize) {
        let (line, col) = self.cursor_line_col();
        let last = self.line_count().saturating_sub(1);
        let next = if delta < 0 { line.saturating_sub(delta.unsigned_abs()) } else { line.saturating_add(delta as usize).min(last) };
        self.cursor = self.byte_at_line_col(next, col);
        self.ensure_cursor_visible(); self.dirty = true;
    }

    fn click_to_cursor(&mut self, x: i32, y: i32) {
        self.cursor = self.byte_from_point(x, y);
        self.completion_matches.clear();
        self.ensure_cursor_visible(); self.dirty = true;
    }

    fn byte_from_point(&self, x: i32, y: i32) -> usize {
        let row = ((y - self.rect.y - PAD).max(0) / LINE_H) as usize;
        let line = (self.scroll_line + row).min(self.line_count().saturating_sub(1));
        let gutter = if self.rhai_mode { GUTTER_W } else { 0 };
        let col = self.scroll_col + ((x - self.rect.x - PAD - gutter).max(0) / FONT_W) as usize;
        self.byte_at_line_col(line, col)
    }

    fn identifier_at(&self, mut at: usize) -> Option<(usize, usize, &str)> {
        at = at.min(self.text.len());
        if at == self.text.len() || !self.text.as_bytes().get(at).map(|b| b.is_ascii_alphanumeric() || *b == b'_').unwrap_or(false) {
            if at == 0 || !self.text.as_bytes()[at - 1].is_ascii_alphanumeric() && self.text.as_bytes()[at - 1] != b'_' { return None; }
            at -= 1;
        }
        let bytes = self.text.as_bytes();
        let mut start = at;
        let mut end = at + 1;
        while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') { start -= 1; }
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') { end += 1; }
        if bytes[start].is_ascii_digit() { return None; }
        Some((start, end, &self.text[start..end]))
    }

    fn update_hover(&mut self, x: i32, y: i32) {
        if !self.rhai_mode {
            if self.hover.take().is_some() { self.dirty = true; }
            return;
        }
        let at = self.byte_from_point(x, y);
        let hover = self.identifier_at(at).map(|(start, end, _)| (start, end, x, y));
        if hover != self.hover { self.hover = hover; self.dirty = true; }
    }

    fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        (anchor != self.cursor).then_some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else { return false; };
        self.text.replace_range(start..end, "");
        self.cursor = start;
        self.selection_anchor = None;
        self.completion_matches.clear();
        self.ensure_cursor_visible();
        self.dirty = true;
        true
    }

    fn prepare_navigation(&mut self, shift: bool) {
        if shift {
            if self.selection_anchor.is_none() { self.selection_anchor = Some(self.cursor); }
        } else {
            self.selection_anchor = None;
        }
    }

    fn refresh_completion(&mut self, open: bool) {
        self.completion_matches.clear();
        self.completion_member_type = None;
        self.selection_anchor = None;
        if !self.rhai_mode || !open { return; }
        let mut start = self.cursor;
        while start > 0 {
            let prev = self.prev_char(start);
            let ch = self.text[prev..start].chars().next().unwrap_or(' ');
            if !(ch.is_ascii_alphanumeric() || ch == '_') { break; }
            start = prev;
        }
        let prefix = &self.text[start..self.cursor];
        self.completion_prefix_start = start;

        if start > 0 && self.text.as_bytes()[start - 1] == b'.' {
            let receiver_end = start - 1;
            let mut receiver_start = receiver_end;
            while receiver_start > 0 {
                let byte = self.text.as_bytes()[receiver_start - 1];
                if byte.is_ascii_alphanumeric() || byte == b'_' { receiver_start -= 1; } else { break; }
            }
            if receiver_start == receiver_end { return; }
            let receiver = &self.text[receiver_start..receiver_end];
            let Some(type_name) = self.infer_identifier_type(receiver, receiver_start) else { return; };
            self.completion_member_type = Some(type_name.clone());
            for member in self.member_completions.iter().filter(|member| member.type_name == type_name) {
                if !member.name.starts_with(prefix) || member.name == prefix { continue; }
                if let Ok(index) = self.completions.binary_search(&member.name) {
                    if !self.completion_matches.contains(&index) { self.completion_matches.push(index); }
                }
            }
            self.completion_selected = 0;
            return;
        }

        if prefix.len() < 2 { return; }
        for (index, value) in self.completions.iter().enumerate() {
            if value.starts_with(prefix) && value != prefix { self.completion_matches.push(index); }
        }
        self.completion_selected = 0;
    }

    fn accept_completion(&mut self) -> bool {
        let Some(&index) = self.completion_matches.get(self.completion_selected) else { return false; };
        let value = self.completions[index].clone();
        let callable = self.completion_detail(&value).map(|detail| detail.contains('(')).unwrap_or(false);
        self.text.replace_range(self.completion_prefix_start..self.cursor, &value);
        self.cursor = self.completion_prefix_start + value.len();
        if callable && self.text.len().saturating_add(2) <= self.max_len {
            self.text.insert_str(self.cursor, "()");
            self.cursor += 1;
        }
        self.completion_matches.clear();
        self.completion_member_type = None;
        self.modified = true;
        self.ensure_cursor_visible();
        self.dirty = true;
        true
    }

    fn signature_name(signature: &str) -> &str {
        signature.split_once('(').map(|(name, _)| name.trim()).unwrap_or(signature.trim())
    }

    fn signature_for(&self, name: &str) -> Option<&str> {
        self.signatures
            .iter()
            .find(|signature| Self::signature_name(signature) == name)
            .map(String::as_str)
    }

    fn member_detail(&self, type_name: &str, name: &str) -> Option<&str> {
        self.member_completions.iter()
            .find(|member| member.type_name == type_name && member.name == name)
            .map(|member| member.detail.as_str())
    }

    fn completion_detail(&self, name: &str) -> Option<&str> {
        self.completion_member_type.as_deref()
            .and_then(|type_name| self.member_detail(type_name, name))
            .or_else(|| self.signature_for(name))
    }

    /// Return the innermost open function call and the zero-based argument
    /// containing the cursor. Strings and line comments do not count.
    fn active_call(&self) -> Option<(&str, usize, usize)> {
        let bytes = self.text.as_bytes();
        let end = self.cursor.min(bytes.len());
        let mut stack: Vec<(u8, usize, usize)> = Vec::new();
        let mut quote = 0u8;
        let mut escaped = false;
        let mut line_comment = false;
        let mut i = 0;
        while i < end {
            let byte = bytes[i];
            if line_comment {
                if byte == b'\n' { line_comment = false; }
                i += 1;
                continue;
            }
            if quote != 0 {
                if escaped { escaped = false; }
                else if byte == b'\\' { escaped = true; }
                else if byte == quote { quote = 0; }
                i += 1;
                continue;
            }
            if byte == b'/' && i + 1 < end && bytes[i + 1] == b'/' {
                line_comment = true;
                i += 2;
                continue;
            }
            if byte == b'\'' || byte == b'"' || byte == b'`' {
                quote = byte;
            } else if matches!(byte, b'(' | b'[' | b'{') {
                stack.push((byte, i, 0));
            } else if matches!(byte, b')' | b']' | b'}') {
                let wanted = match byte { b')' => b'(', b']' => b'[', _ => b'{' };
                if stack.last().map(|entry| entry.0) == Some(wanted) { stack.pop(); }
            } else if byte == b',' {
                if let Some((b'(', _, argument)) = stack.last_mut() { *argument += 1; }
            }
            i += 1;
        }

        let &(_, open, argument) = stack.iter().rev().find(|entry| entry.0 == b'(')?;
        let mut name_end = open;
        while name_end > 0 && bytes[name_end - 1].is_ascii_whitespace() { name_end -= 1; }
        let mut name_start = name_end;
        while name_start > 0 {
            let byte = bytes[name_start - 1];
            if byte.is_ascii_alphanumeric() || byte == b'_' { name_start -= 1; } else { break; }
        }
        (name_start < name_end).then_some((&self.text[name_start..name_end], argument, name_start))
    }

    fn active_signature(&self) -> Option<(&str, usize)> {
        let (name, argument, name_start) = self.active_call()?;
        if name_start > 0 && self.text.as_bytes()[name_start - 1] == b'.' {
            let receiver_end = name_start - 1;
            let mut receiver_start = receiver_end;
            while receiver_start > 0 {
                let byte = self.text.as_bytes()[receiver_start - 1];
                if byte.is_ascii_alphanumeric() || byte == b'_' { receiver_start -= 1; } else { break; }
            }
            let receiver = &self.text[receiver_start..receiver_end];
            if let Some(type_name) = self.infer_identifier_type(receiver, receiver_start) {
                if let Some(detail) = self.member_detail(&type_name, name) { return Some((detail, argument)); }
            }
        }
        self.signature_for(name).map(|signature| (signature, argument))
    }

    fn parameter_span(signature: &str, active: usize) -> Option<(usize, usize)> {
        let open = signature.find('(')? + 1;
        let close = signature[open..].find(')').map(|offset| open + offset)?;
        let parameters = &signature[open..close];
        if parameters.trim().is_empty() { return None; }
        let mut start = open;
        for (index, part) in parameters.split(',').enumerate() {
            let raw_start = start;
            let raw_end = raw_start + part.len();
            if index == active {
                let leading = part.len() - part.trim_start().len();
                let trailing = part.len() - part.trim_end().len();
                return Some((raw_start + leading, raw_end.saturating_sub(trailing)));
            }
            start = raw_end + 1;
        }
        None
    }

    fn type_hint_for(&self, word: &str, before: usize) -> Option<String> {
        if let Some(signature) = self.signature_for(word) { return Some(signature.into()); }
        self.infer_identifier_type(word, before).map(|ty| format!("{word}: {ty}"))
    }

    fn infer_identifier_type(&self, word: &str, before: usize) -> Option<String> {
        if let Some((_, ty)) = self.type_hints.iter().find(|(name, _)| name == word) { return Some(ty.clone()); }
        let source = &self.text[..before.min(self.text.len())];
        for line in source.lines().rev() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("let ").or_else(|| line.strip_prefix("const ")) else { continue; };
            let Some((name, expression)) = rest.split_once('=') else { continue; };
            let declared_name = name.trim().split(':').next().unwrap_or("").trim();
            if declared_name != word { continue; }
            if let Some((_, explicit_type)) = name.split_once(':') {
                let explicit_type = explicit_type.trim();
                if !explicit_type.is_empty() { return Some(explicit_type.into()); }
            }
            let expression = expression.trim();
            let ty = if expression.starts_with(['"', '\'', '`']) { "string" }
                else if expression.starts_with("#{") { "map" }
                else if expression.starts_with('[') { "array" }
                else if expression.starts_with("true") || expression.starts_with("false") { "bool" }
                else if expression.contains("http.request(") { "HttpRequest" }
                else if expression.contains(".header(") || expression.contains(".query(") || expression.contains(".body(") { "HttpRequest" }
                else if expression.starts_with("ui.window(") { "UiApp" }
                else if expression.contains("http.get(") || expression.contains("http.post(") || expression.contains("http.put(") || expression.contains("http.patch(") || expression.contains("http.delete(") || expression.contains(".send(") { "HttpResponse" }
                else if expression.contains(".root(") || expression.contains(".row(") || expression.contains(".column(") || expression.contains(".panel(") || expression.contains(".spacer(") { "UiContainer" }
                else if expression.contains(".label(") || expression.contains(".button(") || expression.contains(".text_input(") || expression.contains(".text_area(") { "UiElement" }
                else if expression.contains("fs.metadata(") { "FileMetadata" }
                else if expression.chars().next().map(|ch| ch.is_ascii_digit() || ch == '-').unwrap_or(false) { "int" }
                else { "dynamic" };
            return Some(ty.into());
        }
        None
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
        let selected_identifier = self.rhai_mode.then(|| self.identifier_at(self.cursor)).flatten()
            .map(|(_, _, word)| word).filter(|word| word.len() > 1);
        for (line_no, line) in self.text.split('\n').enumerate().skip(self.scroll_line).take(rows) {
            let start = line.char_indices().nth(self.scroll_col).map(|(i, _)| i).unwrap_or(line.len());
            let rest = &line[start..];
            let end = rest.char_indices().nth(self.viewport_cols()).map(|(i, _)| i).unwrap_or(rest.len());
            let y = self.rect.y + PAD + (line_no - self.scroll_line) as i32 * LINE_H;
            let gutter = if self.rhai_mode { GUTTER_W } else { 0 };
            if self.rhai_mode {
                draw::fill_rect(window, Rect::new(self.rect.x + 1, y, (GUTTER_W - 2) as u32, LINE_H as u32), Color::new(0xf0, 0xf2, 0xf5));
                draw::text(window, Point::new(self.rect.x + 4, y), &format!("{:>4}", line_no + 1), Color::new(0x78, 0x7d, 0x86));
            }
            if let Some(word) = selected_identifier {
                let visible = &rest[..end];
                for (offset, _) in visible.match_indices(word) {
                    let before = offset.checked_sub(1).and_then(|index| visible.as_bytes().get(index)).copied();
                    let after = visible.as_bytes().get(offset + word.len()).copied();
                    let boundary = |byte: Option<u8>| !byte.map(|b| b.is_ascii_alphanumeric() || b == b'_').unwrap_or(false);
                    if boundary(before) && boundary(after) {
                        draw::fill_rect(window, Rect::new(
                            self.rect.x + PAD + gutter + offset as i32 * FONT_W,
                            y,
                            (word.len() as i32 * FONT_W) as u32,
                            FONT_H as u32,
                        ), Color::new(0xff, 0xee, 0xaa));
                    }
                }
            }
            if let (Some((selection_start, selection_end)), Some((line_start, line_end))) = (self.selection(), self.line_bounds(line_no)) {
                let visible_start = line_start + start;
                let visible_end = line_start + start + end;
                let overlap_start = selection_start.max(visible_start);
                let overlap_end = selection_end.min(visible_end);
                let selects_newline = selection_start <= line_end && selection_end > line_end && line_end >= visible_start && line_end <= visible_end;
                if overlap_start < overlap_end || selects_newline {
                    let first = self.text[visible_start..overlap_start.min(visible_end)].chars().count();
                    let mut count = if overlap_start < overlap_end { self.text[overlap_start..overlap_end].chars().count() } else { 0 };
                    if selects_newline && overlap_end == line_end { count += 1; }
                    draw::fill_rect(window, Rect::new(
                        self.rect.x + PAD + gutter + first as i32 * FONT_W,
                        y,
                        (count.max(1) as i32 * FONT_W) as u32,
                        FONT_H as u32,
                    ), Color::new(0x9f, 0xc5, 0xef));
                }
            }
            if self.rhai_mode {
                draw_rhai_line(window, Point::new(self.rect.x + PAD + gutter, y), &rest[..end]);
            } else {
                draw::text(window, Point::new(self.rect.x + PAD, y), &rest[..end], fg);
            }
            if self.diagnostic.as_ref().map(|d| d.line == line_no + 1).unwrap_or(false) {
                draw::line(window,
                    Point::new(self.rect.x + PAD + gutter, y + FONT_H),
                    Point::new(self.rect.x + self.rect.w as i32 - SCROLLBAR_W - 2, y + FONT_H),
                    Color::new(0xd9, 0x22, 0x22), 1);
            }
        }

        if self.focused {
            let (line, col) = self.cursor_line_col();
            if line >= self.scroll_line && line < self.scroll_line + rows {
                let screen_col = col.saturating_sub(self.scroll_col);
                if screen_col <= self.viewport_cols() {
                    let gutter = if self.rhai_mode { GUTTER_W } else { 0 };
                    draw::fill_rect(window, Rect::new(
                        self.rect.x + PAD + gutter + screen_col as i32 * FONT_W,
                        self.rect.y + PAD + (line - self.scroll_line) as i32 * LINE_H,
                        1,
                        FONT_H as u32,
                    ), fg);
                }
            }
        }

        if !self.completion_matches.is_empty() {
            let (line, col) = self.cursor_line_col();
            let gutter = if self.rhai_mode { GUTTER_W } else { 0 };
            let x = self.rect.x + PAD + gutter + col.saturating_sub(self.scroll_col) as i32 * FONT_W;
            let mut y = self.rect.y + PAD + (line.saturating_sub(self.scroll_line) + 1) as i32 * LINE_H;
            let visible = self.completion_matches.len().min(6);
            let first = self.completion_selected.saturating_add(1).saturating_sub(visible);
            let popup_h = visible as i32 * LINE_H + 4;
            if y + popup_h > self.rect.y + self.rect.h as i32 { y -= popup_h + LINE_H; }
            let label_chars = self.completion_matches.iter().skip(first).take(visible).map(|&index| {
                let name = &self.completions[index];
                self.completion_detail(name).unwrap_or(name).chars().count()
            }).max().unwrap_or(18);
            let available = (self.rect.w as i32 - gutter - 8).max(120);
            let popup_w = (label_chars as i32 * FONT_W + 12).max(120).min(available) as u32;
            let popup = Rect::new(x.min(self.rect.x + self.rect.w as i32 - popup_w as i32 - 2).max(self.rect.x + gutter), y, popup_w, popup_h as u32);
            draw::fill_rect(window, popup, Color::new(0xff, 0xff, 0xff));
            draw::stroke_rect(window, popup, Color::new(0x55, 0x6d, 0x8a), 1);
            for (row, &index) in self.completion_matches.iter().skip(first).take(visible).enumerate() {
                let item = Rect::new(popup.x + 2, popup.y + 2 + row as i32 * LINE_H, popup.w - 4, LINE_H as u32);
                if first + row == self.completion_selected { draw::fill_rect(window, item, Color::new(0xd8, 0xe8, 0xfb)); }
                let name = &self.completions[index];
                let label = self.completion_detail(name).unwrap_or(name);
                draw::text(window, Point::new(item.x + 4, item.y + 1), label, Color::new(0x18, 0x18, 0x18));
            }
        }

        if let Some((signature, active_parameter)) = self.active_signature() {
            let (line, col) = self.cursor_line_col();
            if line >= self.scroll_line && line < self.scroll_line + rows {
                let gutter = if self.rhai_mode { GUTTER_W } else { 0 };
                let available = (self.rect.w as i32 - gutter - 8).max(120);
                let popup_w = (signature.chars().count() as i32 * FONT_W + 12).max(120).min(available) as u32;
                let x = (self.rect.x + PAD + gutter + col.saturating_sub(self.scroll_col) as i32 * FONT_W)
                    .min(self.rect.x + self.rect.w as i32 - popup_w as i32 - 2)
                    .max(self.rect.x + gutter);
                let cursor_y = self.rect.y + PAD + (line - self.scroll_line) as i32 * LINE_H;
                let y = if cursor_y - LINE_H - 6 >= self.rect.y {
                    cursor_y - LINE_H - 4
                } else {
                    cursor_y + LINE_H + 2
                };
                let popup = Rect::new(x, y, popup_w, (LINE_H + 4) as u32);
                draw::fill_rect(window, popup, Color::new(0xff, 0xfd, 0xe8));
                draw::stroke_rect(window, popup, Color::new(0x9b, 0x83, 0x42), 1);
                if let Some((start, end)) = Self::parameter_span(signature, active_parameter) {
                    draw::fill_rect(window, Rect::new(
                        popup.x + 6 + signature[..start].chars().count() as i32 * FONT_W,
                        popup.y + 2,
                        ((signature[start..end].chars().count().max(1) as i32) * FONT_W) as u32,
                        FONT_H as u32,
                    ), Color::new(0xff, 0xe3, 0x91));
                }
                draw::text(window, Point::new(popup.x + 6, popup.y + 3), signature, Color::new(0x28, 0x25, 0x1c));
            }
        }

        if let Some((start, end, mouse_x, mouse_y)) = self.hover {
            let word = &self.text[start..end];
            if let Some(hint) = self.type_hint_for(word, start) {
                let available = (self.rect.w as i32 - 8).max(120);
                let width = (hint.chars().count() as i32 * FONT_W + 12).max(120).min(available) as u32;
                let x = (mouse_x + 12).min(self.rect.x + self.rect.w as i32 - width as i32 - 2).max(self.rect.x + 2);
                let mut y = mouse_y + 18;
                if y + LINE_H + 4 > self.rect.y + self.rect.h as i32 { y = mouse_y - LINE_H - 6; }
                let popup = Rect::new(x, y, width, (LINE_H + 4) as u32);
                draw::fill_rect(window, popup, Color::new(0xf2, 0xf6, 0xff));
                draw::stroke_rect(window, popup, Color::new(0x58, 0x70, 0x98), 1);
                draw::text(window, Point::new(popup.x + 6, popup.y + 3), &hint, Color::new(0x20, 0x2b, 0x3c));
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
                    if thumb.contains(x, y) { self.selection_anchor = None; self.dragging_selection = false; self.dragging_scrollbar = true; self.scrollbar_grab_y = y - thumb.y; return EventResult::Consumed; }
                }
                if let Some(bar) = self.scrollbar_rect() {
                    if bar.contains(x, y) {
                        self.selection_anchor = None; self.dragging_selection = false;
                        let grab = self.scrollbar_thumb().map(|t| t.h as i32 / 2).unwrap_or(0);
                        self.scroll_from_thumb_top(y - grab); self.dragging_scrollbar = true; self.scrollbar_grab_y = grab; return EventResult::Changed;
                    }
                }
                self.click_to_cursor(x, y);
                self.selection_anchor = Some(self.cursor);
                self.dragging_selection = true;
                EventResult::Consumed
            }
            UiEvent::Move { y, .. } if self.dragging_scrollbar => { self.scroll_from_thumb_top(y - self.scrollbar_grab_y); EventResult::Changed }
            UiEvent::Up { .. } if self.dragging_scrollbar => { self.dragging_scrollbar = false; EventResult::Consumed }
            UiEvent::Move { x, y } if self.dragging_selection => {
                let left = self.rect.x + PAD + if self.rhai_mode { GUTTER_W } else { 0 };
                let right = self.rect.x + self.rect.w as i32 - SCROLLBAR_W;
                let top = self.rect.y + PAD;
                let bottom = self.rect.y + self.rect.h as i32 - PAD;
                if y < top { self.scroll_line = self.scroll_line.saturating_sub(1); }
                if y > bottom { self.scroll_line = self.scroll_line.saturating_add(1).min(self.max_scroll_line()); }
                if x < left { self.scroll_col = self.scroll_col.saturating_sub(1); }
                let drag_x = if x > right { right + FONT_W } else { x.max(left) };
                self.click_to_cursor(drag_x, y.clamp(top, (bottom - 1).max(top)));
                EventResult::Consumed
            }
            UiEvent::Move { x, y } if self.rect.contains(x, y) => {
                self.update_hover(x, y);
                EventResult::Consumed
            }
            UiEvent::Move { .. } => {
                if self.hover.take().is_some() { self.dirty = true; EventResult::Changed } else { EventResult::Ignored }
            }
            UiEvent::Leave => {
                if self.hover.take().is_some() { self.dirty = true; EventResult::Changed } else { EventResult::Ignored }
            }
            UiEvent::Up { .. } if self.dragging_selection => {
                self.dragging_selection = false;
                self.dirty = true;
                EventResult::Consumed
            }
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
            UiEvent::KeyDown { scancode, ch, mods } if focused => {
                if mods & MOD_CTRL != 0 && scancode == SCAN_P {
                    self.completion_matches.clear();
                    self.dirty = true;
                    return EventResult::Changed;
                }
                // WM supplies both a physical scancode and the translated
                // character.  Printable text wins even if a stale/mismatched
                // scancode is present in the event.
                if (0x20..0x7f).contains(&ch) {
                    self.insert_char(ch as char);
                    return EventResult::Changed;
                }
                if !self.completion_matches.is_empty() {
                    match scancode {
                        SCAN_ESCAPE => { self.completion_matches.clear(); self.dirty = true; return EventResult::Changed; }
                        SCAN_UP => { self.completion_selected = self.completion_selected.saturating_sub(1); self.dirty = true; return EventResult::Changed; }
                        SCAN_DOWN => { self.completion_selected = (self.completion_selected + 1).min(self.completion_matches.len().saturating_sub(1)); self.dirty = true; return EventResult::Changed; }
                        SCAN_TAB | SCAN_ENTER if self.accept_completion() => return EventResult::Changed,
                        _ => {}
                    }
                }
                let shift = mods & 1 != 0;
                match scancode {
                    SCAN_BACKSPACE => self.backspace(),
                    SCAN_DELETE => self.delete(),
                    SCAN_ENTER => self.insert_newline(),
                    SCAN_TAB => self.insert_str("    "),
                    SCAN_LEFT => { self.prepare_navigation(shift); self.cursor = self.prev_char(self.cursor); self.ensure_cursor_visible(); self.dirty = true; }
                    SCAN_RIGHT => { self.prepare_navigation(shift); self.cursor = self.next_char(self.cursor); self.ensure_cursor_visible(); self.dirty = true; }
                    SCAN_UP => { self.prepare_navigation(shift); self.move_vertical(-1); }
                    SCAN_DOWN => { self.prepare_navigation(shift); self.move_vertical(1); }
                    SCAN_PAGE_UP => { self.prepare_navigation(shift); self.move_vertical(-(self.visible_rows() as isize)); }
                    SCAN_PAGE_DOWN => { self.prepare_navigation(shift); self.move_vertical(self.visible_rows() as isize); }
                    SCAN_HOME => { self.prepare_navigation(shift); let (line, _) = self.cursor_line_col(); self.cursor = self.byte_at_line_col(line, 0); self.ensure_cursor_visible(); self.dirty = true; }
                    SCAN_END => { self.prepare_navigation(shift); let (line, _) = self.cursor_line_col(); if let Some((_, end)) = self.line_bounds(line) { self.cursor = end; self.ensure_cursor_visible(); self.dirty = true; } }
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

fn draw_rhai_line(window: &mut Window, at: Point, line: &str) {
    const KEYWORDS: &[&str] = &["let", "const", "if", "else", "switch", "while", "loop", "for", "in", "fn", "return", "throw", "try", "catch", "break", "continue", "import", "export", "as", "private", "true", "false"];
    let normal = Color::new(0x20, 0x24, 0x2a);
    let keyword = Color::new(0x78, 0x32, 0xa8);
    let string = Color::new(0x1b, 0x78, 0x35);
    let number = Color::new(0x1b, 0x58, 0xb8);
    let comment = Color::new(0x78, 0x83, 0x78);
    let function = Color::new(0x9a, 0x59, 0x13);
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let color;
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            i = bytes.len(); color = comment;
        } else if bytes[i] == b'"' || bytes[i] == b'\'' || bytes[i] == b'`' {
            let quote = bytes[i]; i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' { i = (i + 2).min(bytes.len()); continue; }
                let done = bytes[i] == quote; i += 1; if done { break; }
            }
            color = string;
        } else if bytes[i].is_ascii_digit() {
            i += 1; while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') { i += 1; }
            color = number;
        } else if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            i += 1; while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') { i += 1; }
            let word = &line[start..i];
            let mut look = i; while look < bytes.len() && bytes[look].is_ascii_whitespace() { look += 1; }
            color = if KEYWORDS.contains(&word) { keyword } else if look < bytes.len() && bytes[look] == b'(' { function } else { normal };
        } else {
            i += line[i..].chars().next().map(char::len_utf8).unwrap_or(1); color = normal;
        }
        draw::text(window, Point::new(at.x + start as i32 * FONT_W, at.y), &line[start..i], color);
    }
}

fn starts_with_closer(line: &str) -> bool {
    matches!(line.trim_start().as_bytes().first(), Some(b'}' | b']' | b')'))
}

fn brace_delta(line: &str) -> isize {
    let mut delta = 0isize;
    let mut quote = 0u8;
    let mut escaped = false;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if quote != 0 {
            if escaped { escaped = false; }
            else if byte == b'\\' { escaped = true; }
            else if byte == quote { quote = 0; }
        } else if byte == b'/' && bytes.get(i + 1) == Some(&b'/') {
            break;
        } else if matches!(byte, b'\'' | b'"' | b'`') {
            quote = byte;
        } else if matches!(byte, b'{' | b'[' | b'(') {
            delta += 1;
        } else if matches!(byte, b'}' | b']' | b')') {
            delta -= 1;
        }
        i += 1;
    }
    delta
}
