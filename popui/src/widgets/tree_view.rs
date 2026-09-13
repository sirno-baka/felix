use std::{fs, io, string::String, vec::Vec};

use popugos::window::Window;

use crate::{draw, Color, Constraints, EventResult, Image, Point, Rect, Size, Theme, UiEvent, Widget};

const ROW_H: i32 = 26;
const INDENT: i32 = 17;
const EXPANDER: i32 = 11;
const ICON_SIZE: i32 = 18;
const FONT_W: i32 = 9;
const FONT_H: i32 = 18;
const SCROLLBAR_W: i32 = 10;

const BG: Color = Color::new(0xFA, 0xFA, 0xFA);
const TEXT: Color = Color::new(0x18, 0x18, 0x18);
const HOVER: Color = Color::new(0xE6, 0xF0, 0xFF);
const SELECTED: Color = Color::new(0x31, 0x6A, 0xC5);
const SELECTED_TEXT: Color = Color::new(0xFF, 0xFF, 0xFF);
const LINE: Color = Color::new(0x8A, 0x8A, 0x8A);

const SCAN_ENTER: u8 = 0x1C;
const SCAN_HOME: u8 = 0x47;
const SCAN_UP: u8 = 0x48;
const SCAN_PAGE_UP: u8 = 0x49;
const SCAN_LEFT: u8 = 0x4B;
const SCAN_RIGHT: u8 = 0x4D;
const SCAN_END: u8 = 0x4F;
const SCAN_DOWN: u8 = 0x50;
const SCAN_PAGE_DOWN: u8 = 0x51;

#[derive(Clone, Debug)]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub depth: usize,
    pub expanded: bool,
    pub loaded: bool,
    pub has_children: bool,
    pub is_mount_point: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TreeViewIcons {
    pub folder: Option<Image>,
    pub folder_open: Option<Image>,
    pub mount: Option<Image>,
    pub root: Option<Image>,
}

pub struct TreeView {
    rect: Rect,
    nodes: Vec<TreeNode>,
    icons: TreeViewIcons,
    selected: Option<usize>,
    hovered: Option<usize>,
    pressed: Option<usize>,
    pending_path: Option<String>,
    focused: bool,
    scroll_y: i32,
    dragging_scrollbar: bool,
    scrollbar_grab_y: i32,
    dirty: bool,
}

impl TreeView {
    pub fn new() -> Self {
        Self {
            rect: Rect::default(),
            nodes: Vec::new(),
            icons: TreeViewIcons::default(),
            selected: None,
            hovered: None,
            pressed: None,
            pending_path: None,
            focused: false,
            scroll_y: 0,
            dragging_scrollbar: false,
            scrollbar_grab_y: 0,
            dirty: true,
        }
    }

    /// Load the root using ordinary `std::fs`. Mount-point discovery is not
    /// duplicated here; the root itself is marked as a root/mount visual node.
    pub fn load_root(&mut self, path: &str) -> io::Result<()> {
        let path = normalize_path(path);
        fs::metadata(&path)?;
        let name = if path == "/" { String::from("/") } else { String::from(path.rsplit('/').next().unwrap_or(path.as_str())) };
        self.nodes.clear();
        self.nodes.push(TreeNode {
            name,
            path,
            depth: 0,
            expanded: false,
            loaded: false,
            has_children: true,
            is_mount_point: true,
        });
        self.selected = Some(0);
        self.hovered = None;
        self.pressed = None;
        self.pending_path = None;
        self.scroll_y = 0;
        let _ = self.expand_index(0)?;
        self.dirty = true;
        Ok(())
    }

    pub fn nodes(&self) -> &[TreeNode] { &self.nodes }
    pub fn selected_index(&self) -> Option<usize> { self.selected }
    pub fn selected_node(&self) -> Option<&TreeNode> { self.selected.and_then(|i| self.nodes.get(i)) }
    pub fn selected_path(&self) -> Option<&str> { self.selected_node().map(|n| n.path.as_str()) }
    pub fn take_selected_path(&mut self) -> Option<String> { self.pending_path.take() }
    pub fn icons(&self) -> &TreeViewIcons { &self.icons }
    pub fn icons_mut(&mut self) -> &mut TreeViewIcons { self.dirty = true; &mut self.icons }
    pub fn set_folder_icon(&mut self, icon: Image) { self.icons.folder = Some(icon); self.dirty = true; }
    pub fn set_folder_open_icon(&mut self, icon: Image) { self.icons.folder_open = Some(icon); self.dirty = true; }
    pub fn set_mount_icon(&mut self, icon: Image) { self.icons.mount = Some(icon); self.dirty = true; }
    pub fn set_root_icon(&mut self, icon: Image) { self.icons.root = Some(icon); self.dirty = true; }

    pub fn reload(&mut self) -> io::Result<()> {
        let root = self.nodes.first().map(|n| n.path.clone()).unwrap_or_else(|| String::from("/"));
        self.load_root(&root)
    }
    pub fn expand_selected(&mut self) -> io::Result<bool> { match self.selected { Some(i) => self.expand_index(i), None => Ok(false) } }
    pub fn collapse_selected(&mut self) -> bool { self.selected.map(|i| self.collapse_index(i)).unwrap_or(false) }
    pub fn expand_path(&mut self, path: &str) -> io::Result<bool> {
        let path = normalize_path(path);
        match self.nodes.iter().position(|n| n.path == path) { Some(i) => self.expand_index(i), None => Ok(false) }
    }

    pub fn select_path(&mut self, path: &str) -> io::Result<bool> {
        let target = normalize_path(path);
        let Some(root) = self.nodes.first().map(|n| n.path.clone()) else { return Ok(false); };
        if target == root {
            self.selected = Some(0);
            self.ensure_selected_visible();
            self.dirty = true;
            return Ok(true);
        }
        let relative = if root == "/" {
            target.strip_prefix('/').unwrap_or(target.as_str())
        } else {
            let Some(rest) = target.strip_prefix(root.as_str()) else { return Ok(false); };
            let Some(rest) = rest.strip_prefix('/') else { return Ok(false); };
            rest
        };
        let mut current = 0usize;
        let mut current_path = root;
        for part in relative.split('/').filter(|p| !p.is_empty()) {
            let _ = self.expand_index(current)?;
            current_path = join_path(&current_path, part);
            let parent_depth = self.nodes[current].depth;
            let mut found = None;
            let mut i = current + 1;
            while i < self.nodes.len() {
                let depth = self.nodes[i].depth;
                if depth <= parent_depth { break; }
                if depth == parent_depth + 1 && self.nodes[i].path == current_path { found = Some(i); break; }
                i += 1;
            }
            let Some(next) = found else { return Ok(false); };
            current = next;
        }
        self.selected = Some(current);
        self.ensure_selected_visible();
        self.dirty = true;
        Ok(true)
    }

    fn for_each_visible(&self, mut f: impl FnMut(usize, usize)) {
        let mut hidden_below = None;
        let mut visible_pos = 0usize;
        for (i, node) in self.nodes.iter().enumerate() {
            if let Some(depth) = hidden_below {
                if node.depth > depth { continue; }
                hidden_below = None;
            }
            f(visible_pos, i);
            visible_pos += 1;
            if !node.expanded { hidden_below = Some(node.depth); }
        }
    }
    fn visible_count(&self) -> usize { let mut n = 0; self.for_each_visible(|_, _| n += 1); n }
    fn visible_node_at(&self, wanted: usize) -> Option<usize> {
        let mut found = None;
        self.for_each_visible(|pos, index| if pos == wanted { found = Some(index) });
        found
    }
    fn visible_position_of(&self, wanted: usize) -> Option<usize> {
        let mut found = None;
        self.for_each_visible(|pos, index| if index == wanted { found = Some(pos) });
        found
    }
    fn first_visible(&self) -> Option<usize> { self.visible_node_at(0) }
    fn last_visible(&self) -> Option<usize> {
        let n = self.visible_count();
        if n == 0 { None } else { self.visible_node_at(n - 1) }
    }

    fn expand_index(&mut self, index: usize) -> io::Result<bool> {
        if index >= self.nodes.len() { return Ok(false); }
        if self.nodes[index].loaded {
            if self.nodes[index].has_children && !self.nodes[index].expanded {
                self.nodes[index].expanded = true;
                self.dirty = true;
                return Ok(true);
            }
            return Ok(false);
        }

        let parent_path = self.nodes[index].path.clone();
        let parent_depth = self.nodes[index].depth;
        let mut dirs = Vec::new();
        for entry in fs::read_dir(&parent_path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                dirs.push((name, entry.path().to_string_lossy().into_owned()));
            }
        }
        dirs.sort_by(|a, b| a.0.cmp(&b.0));

        let has_children = !dirs.is_empty();
        self.nodes[index].loaded = true;
        self.nodes[index].has_children = has_children;
        self.nodes[index].expanded = has_children;

        if has_children {
            let mut insert = index + 1;
            for (name, child_path) in dirs {
                let child_has_children = directory_has_children(&child_path);
                self.nodes.insert(insert, TreeNode {
                    name,
                    path: child_path,
                    depth: parent_depth + 1,
                    expanded: false,
                    loaded: false,
                    has_children: child_has_children,
                    is_mount_point: false,
                });
                insert += 1;
            }
        }
        self.dirty = true;
        Ok(true)
    }

    fn collapse_index(&mut self, index: usize) -> bool {
        let Some(node) = self.nodes.get_mut(index) else { return false; };
        if node.expanded { node.expanded = false; self.dirty = true; true } else { false }
    }
    fn toggle_index(&mut self, index: usize) -> io::Result<bool> {
        if self.nodes.get(index).map(|n| n.expanded).unwrap_or(false) { Ok(self.collapse_index(index)) } else { self.expand_index(index) }
    }

    fn content_height(&self) -> i32 { self.visible_count() as i32 * ROW_H }
    fn max_scroll(&self) -> i32 { (self.content_height() - self.rect.h as i32).max(0) }
    pub fn scroll_to(&mut self, y: i32) {
        let next = y.clamp(0, self.max_scroll());
        if next != self.scroll_y { self.scroll_y = next; self.dirty = true; }
    }
    pub fn scroll_by(&mut self, dy: i32) { self.scroll_to(self.scroll_y.saturating_add(dy)); }
    fn content_width(&self) -> i32 { (self.rect.w as i32 - if self.max_scroll() > 0 { SCROLLBAR_W } else { 0 }).max(1) }
    fn row_at(&self, x: i32, y: i32) -> Option<(usize, usize)> {
        if !self.rect.contains(x, y) || self.scrollbar_rect().map(|r| r.contains(x, y)).unwrap_or(false) { return None; }
        let local_y = y - self.rect.y + self.scroll_y;
        if local_y < 0 { return None; }
        let pos = (local_y / ROW_H) as usize;
        Some((pos, self.visible_node_at(pos)?))
    }
    fn row_rect(&self, pos: usize) -> Rect { Rect::new(self.rect.x, self.rect.y + pos as i32 * ROW_H - self.scroll_y, self.content_width() as u32, ROW_H as u32) }
    fn expander_rect(&self, pos: usize, node_index: usize) -> Rect {
        let row = self.row_rect(pos);
        let depth = self.nodes[node_index].depth as i32;
        Rect::new(row.x + 4 + depth * INDENT, row.y + (ROW_H - EXPANDER) / 2, EXPANDER as u32, EXPANDER as u32)
    }
    fn scrollbar_rect(&self) -> Option<Rect> {
        (self.max_scroll() > 0 && self.rect.w >= SCROLLBAR_W as u32).then_some(Rect::new(self.rect.x + self.rect.w as i32 - SCROLLBAR_W, self.rect.y, SCROLLBAR_W as u32, self.rect.h))
    }
    fn scrollbar_thumb(&self) -> Option<Rect> {
        let bar = self.scrollbar_rect()?;
        let max = self.max_scroll();
        let view_h = self.rect.h as i32;
        let content_h = self.content_height().max(1);
        let thumb_h = ((view_h as i64 * view_h as i64) / content_h as i64).max(14).min(view_h as i64) as i32;
        let travel = (view_h - thumb_h).max(0);
        let top = if max == 0 { 0 } else { (self.scroll_y as i64 * travel as i64 / max as i64) as i32 };
        Some(Rect::new(bar.x, bar.y + top, bar.w, thumb_h as u32))
    }
    fn scroll_from_thumb_top(&mut self, top: i32) {
        let Some(bar) = self.scrollbar_rect() else { return; };
        let Some(thumb) = self.scrollbar_thumb() else { return; };
        let travel = (bar.h as i32 - thumb.h as i32).max(0);
        if travel <= 0 { self.scroll_to(0); return; }
        let local = (top - bar.y).clamp(0, travel);
        self.scroll_to((local as i64 * self.max_scroll() as i64 / travel as i64) as i32);
    }
    fn ensure_selected_visible(&mut self) {
        let Some(selected) = self.selected else { return; };
        let Some(pos) = self.visible_position_of(selected) else { return; };
        let row = self.row_rect(pos);
        if row.y < self.rect.y { self.scroll_by(row.y - self.rect.y); }
        else if row.y + row.h as i32 > self.rect.y + self.rect.h as i32 { self.scroll_by(row.y + row.h as i32 - (self.rect.y + self.rect.h as i32)); }
    }
    fn move_selection(&mut self, delta: isize) {
        let count = self.visible_count();
        if count == 0 { self.selected = None; return; }
        let current = self.selected.and_then(|s| self.visible_position_of(s)).unwrap_or(0) as isize;
        self.selected = self.visible_node_at((current + delta).clamp(0, count as isize - 1) as usize);
        self.ensure_selected_visible();
        self.dirty = true;
    }
    fn select_parent(&mut self) -> bool {
        let Some(selected) = self.selected else { return false; };
        let depth = self.nodes[selected].depth;
        if depth == 0 { return false; }
        for i in (0..selected).rev() {
            if self.nodes[i].depth + 1 == depth {
                self.selected = Some(i);
                self.ensure_selected_visible();
                self.dirty = true;
                return true;
            }
        }
        false
    }
    fn handle_key(&mut self, scancode: u8) -> EventResult {
        match scancode {
            SCAN_UP => { self.move_selection(-1); EventResult::Changed }
            SCAN_DOWN => { self.move_selection(1); EventResult::Changed }
            SCAN_HOME => { if let Some(i) = self.first_visible() { self.selected = Some(i); self.ensure_selected_visible(); self.dirty = true; } EventResult::Changed }
            SCAN_END => { if let Some(i) = self.last_visible() { self.selected = Some(i); self.ensure_selected_visible(); self.dirty = true; } EventResult::Changed }
            SCAN_PAGE_UP => { self.scroll_by(-(self.rect.h as i32).max(1)); EventResult::Changed }
            SCAN_PAGE_DOWN => { self.scroll_by((self.rect.h as i32).max(1)); EventResult::Changed }
            SCAN_RIGHT => { if let Some(i) = self.selected { let _ = self.expand_index(i); } EventResult::Changed }
            SCAN_LEFT => { if let Some(i) = self.selected { if !self.collapse_index(i) { let _ = self.select_parent(); } } EventResult::Changed }
            SCAN_ENTER => {
                if let Some(i) = self.selected {
                    self.pending_path = Some(self.nodes[i].path.clone());
                    self.dirty = true;
                    EventResult::Submitted
                } else { EventResult::Consumed }
            }
            _ => EventResult::Ignored,
        }
    }

    fn icon_for(&self, index: usize) -> Option<&Image> {
        let node = self.nodes.get(index)?;
        if node.depth == 0 { self.icons.root.as_ref().or(self.icons.mount.as_ref()).or(self.icons.folder_open.as_ref()) }
        else if node.is_mount_point { self.icons.mount.as_ref().or(self.icons.folder.as_ref()) }
        else if node.expanded { self.icons.folder_open.as_ref().or(self.icons.folder.as_ref()) }
        else { self.icons.folder.as_ref() }
    }
    fn draw_icon(&self, window: &mut Window, image: &Image, x: i32, y: i32) {
        draw::blit_rgb565_cropped(
            window,
            Point::new(x, y),
            image.width,
            image.height,
            image.pixels(),
            image.mask(),
            ICON_SIZE,
            ICON_SIZE,
        );
    }
    fn draw_expander(&self, window: &mut Window, rect: Rect, expanded: bool) {
        draw::stroke_rect(window, rect, LINE, 1);
        let cy = rect.y + rect.h as i32 / 2;
        let cx = rect.x + rect.w as i32 / 2;
        draw::line(window, Point::new(rect.x + 2, cy), Point::new(rect.x + rect.w as i32 - 3, cy), LINE, 1);
        if !expanded { draw::line(window, Point::new(cx, rect.y + 2), Point::new(cx, rect.y + rect.h as i32 - 3), LINE, 1); }
    }
}

impl Default for TreeView { fn default() -> Self { Self::new() } }

impl Widget for TreeView {
    fn measure(&self, constraints: Constraints) -> Size { constraints.clamp(Size::new(190.0, 240.0)) }
    fn set_rect(&mut self, rect: Rect) {
        if self.rect != rect { self.rect = rect; self.scroll_y = self.scroll_y.clamp(0, self.max_scroll()); self.dirty = true; }
    }
    fn rect(&self) -> Rect { self.rect }
    fn draw(&self, window: &mut Window, theme: &Theme) {
        if self.rect.w == 0 || self.rect.h == 0 { return; }
        draw::fill_rect(window, self.rect, BG);
        self.for_each_visible(|pos, index| {
            let row = self.row_rect(pos);
            if row.y + row.h as i32 <= self.rect.y || row.y >= self.rect.y + self.rect.h as i32 { return; }
            let selected = self.selected == Some(index);
            let hovered = self.hovered == Some(index);
            if selected || hovered { draw::fill_rect(window, row, if selected { SELECTED } else { HOVER }); }
            let node = &self.nodes[index];
            let exp = self.expander_rect(pos, index);
            if node.has_children { self.draw_expander(window, exp, node.expanded); }
            let icon_x = exp.x + EXPANDER + 3;
            let icon_y = row.y + (ROW_H - ICON_SIZE) / 2;
            if let Some(icon) = self.icon_for(index) { self.draw_icon(window, icon, icon_x, icon_y); }
            let text_x = icon_x + ICON_SIZE + 4;
            let max_chars = ((self.content_width() - (text_x - row.x) - 4) / FONT_W).max(1);
            let name = truncate_text(&node.name, max_chars);
            draw::text(window, Point::new(text_x, row.y + (ROW_H - FONT_H) / 2), name, if selected { SELECTED_TEXT } else { TEXT });
        });
        if let Some(bar) = self.scrollbar_rect() {
            draw::fill_rect(window, bar, theme.scrollbar_background);
            if let Some(thumb) = self.scrollbar_thumb() { draw::fill_rect(window, thumb, theme.scrollbar_thumb); }
        }
    }
    fn event(&mut self, event: &UiEvent, focused: bool) -> EventResult {
        if self.focused != focused { self.focused = focused; self.dirty = true; }
        match *event {
            UiEvent::Down { x, y } => {
                if !self.rect.contains(x, y) { return EventResult::Ignored; }
                if let Some(thumb) = self.scrollbar_thumb() {
                    if thumb.contains(x, y) { self.dragging_scrollbar = true; self.scrollbar_grab_y = y - thumb.y; return EventResult::Consumed; }
                }
                if let Some(bar) = self.scrollbar_rect() {
                    if bar.contains(x, y) {
                        let grab = self.scrollbar_thumb().map(|t| t.h as i32 / 2).unwrap_or(0);
                        self.scroll_from_thumb_top(y - grab); self.dragging_scrollbar = true; self.scrollbar_grab_y = grab; return EventResult::Changed;
                    }
                }
                let hit = self.row_at(x, y);
                self.pressed = hit.map(|(_, i)| i);
                if let Some((pos, index)) = hit {
                    let exp = self.expander_rect(pos, index);
                    if exp.contains(x, y) && self.nodes[index].has_children {
                        self.pressed = None;
                        let _ = self.toggle_index(index);
                        return EventResult::Changed;
                    }
                    if self.selected != Some(index) { self.selected = Some(index); self.dirty = true; return EventResult::Changed; }
                }
                EventResult::Consumed
            }
            UiEvent::Move { x, y } => {
                if self.dragging_scrollbar { self.scroll_from_thumb_top(y - self.scrollbar_grab_y); return EventResult::Changed; }
                let hover = self.row_at(x, y).map(|(_, i)| i);
                if hover != self.hovered { self.hovered = hover; self.dirty = true; EventResult::Changed } else { EventResult::Ignored }
            }
            UiEvent::Leave => {
                if self.hovered.take().is_some() { self.dirty = true; EventResult::Changed } else { EventResult::Ignored }
            }
            UiEvent::Wheel { delta, .. } => {
                let old = self.scroll_y;
                self.scroll_by(delta.saturating_mul(ROW_H * 3));
                if self.scroll_y != old { EventResult::Changed } else { EventResult::Consumed }
            }
            UiEvent::Up { x, y } => {
                if self.dragging_scrollbar { self.dragging_scrollbar = false; return EventResult::Consumed; }
                let pressed = self.pressed.take();
                let hit = self.row_at(x, y).map(|(_, i)| i);
                if let (Some(a), Some(b)) = (pressed, hit) {
                    if a == b { self.selected = Some(a); self.pending_path = Some(self.nodes[a].path.clone()); self.dirty = true; return EventResult::Clicked; }
                }
                EventResult::Consumed
            }
            UiEvent::Context { x, y } if self.rect.contains(x, y) => {
                if let Some((_, index)) = self.row_at(x, y) {
                    self.selected = Some(index);
                    self.ensure_selected_visible();
                    self.dirty = true;
                } else if self.selected.take().is_some() {
                    self.dirty = true;
                }
                EventResult::ContextRequested
            }
            UiEvent::KeyDown { scancode, .. } if focused => self.handle_key(scancode),
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

fn directory_has_children(path: &str) -> bool {
    match fs::read_dir(path) {
        Ok(entries) => entries.filter_map(|entry| entry.ok()).any(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false)),
        Err(_) => false,
    }
}
fn normalize_path(path: &str) -> String {
    if path.is_empty() || path == "/" { String::from("/") } else { String::from(path.trim_end_matches('/')) }
}
fn join_path(parent: &str, name: &str) -> String {
    if parent == "/" || parent.is_empty() { format!("/{name}") } else { format!("{}/{}", parent.trim_end_matches('/'), name) }
}
fn truncate_text(text: &str, max_chars: i32) -> &str {
    match text.char_indices().nth(max_chars.max(1) as usize) { Some((byte, _)) => &text[..byte], None => text }
}
