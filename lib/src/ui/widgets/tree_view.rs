use super::{font, IconImage};
use crate::fs::{self, IoResult};
use crate::ui::{Constraints, EventResult, Rect, UiEvent, Widget};
use alloc::{string::String, vec::Vec};
use embedded_graphics::{
    mono_font::MonoTextStyle,
    pixelcolor::Rgb888,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
    Pixel,
};
use taffy::geometry::Size as TSize;

const ROW_H: i32 = 26;
const INDENT: i32 = 17;
const EXPANDER: i32 = 11;
const ICON_SIZE: i32 = 18;
const FONT_W: i32 = 9;
const FONT_H: i32 = 18;
const SCROLLBAR_W: i32 = 10;

const BG: Rgb888 = Rgb888::new(0xFA, 0xFA, 0xFA);
const TEXT: Rgb888 = Rgb888::new(0x18, 0x18, 0x18);
const HOVER: Rgb888 = Rgb888::new(0xE6, 0xF0, 0xFF);
const SELECTED: Rgb888 = Rgb888::new(0x31, 0x6A, 0xC5);
const SELECTED_TEXT: Rgb888 = Rgb888::new(0xFF, 0xFF, 0xFF);
const LINE: Rgb888 = Rgb888::new(0x8A, 0x8A, 0x8A);
const SCROLL_BG: Rgb888 = Rgb888::new(0xE5, 0xE5, 0xE5);
const SCROLL_THUMB: Rgb888 = Rgb888::new(0xA8, 0xA8, 0xA8);

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
    pub folder: Option<IconImage>,
    pub folder_open: Option<IconImage>,
    pub mount: Option<IconImage>,
    pub root: Option<IconImage>,
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

    /// Set a root and load only its first directory level.
    pub fn load_root(&mut self, path: &str) -> IoResult<()> {
        let path = normalize_path(path);
        let name = if path == "/" {
            String::from("/")
        } else {
            String::from(path.rsplit('/').next().unwrap_or(path.as_str()))
        };

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

    pub fn nodes(&self) -> &[TreeNode] {
        &self.nodes
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    pub fn selected_node(&self) -> Option<&TreeNode> {
        self.selected.and_then(|i| self.nodes.get(i))
    }

    pub fn selected_path(&self) -> Option<&str> {
        self.selected_node().map(|n| n.path.as_str())
    }

    /// Consume a path selected by mouse click or Enter.
    pub fn take_selected_path(&mut self) -> Option<String> {
        self.pending_path.take()
    }

    pub fn icons(&self) -> &TreeViewIcons {
        &self.icons
    }

    pub fn icons_mut(&mut self) -> &mut TreeViewIcons {
        self.dirty = true;
        &mut self.icons
    }

    pub fn set_folder_icon(&mut self, icon: IconImage) {
        self.icons.folder = Some(icon);
        self.dirty = true;
    }

    pub fn set_folder_open_icon(&mut self, icon: IconImage) {
        self.icons.folder_open = Some(icon);
        self.dirty = true;
    }

    pub fn set_mount_icon(&mut self, icon: IconImage) {
        self.icons.mount = Some(icon);
        self.dirty = true;
    }

    pub fn set_root_icon(&mut self, icon: IconImage) {
        self.icons.root = Some(icon);
        self.dirty = true;
    }

    pub fn reload(&mut self) -> IoResult<()> {
        let root = self.nodes.first().map(|n| n.path.clone()).unwrap_or_else(|| String::from("/"));
        self.load_root(&root)
    }

    pub fn expand_selected(&mut self) -> IoResult<bool> {
        match self.selected {
            Some(i) => self.expand_index(i),
            None => Ok(false),
        }
    }

    pub fn collapse_selected(&mut self) -> bool {
        let Some(i) = self.selected else { return false; };
        self.collapse_index(i)
    }

    pub fn expand_path(&mut self, path: &str) -> IoResult<bool> {
        let path = normalize_path(path);
        if let Some(i) = self.nodes.iter().position(|n| n.path == path) {
            self.expand_index(i)
        } else {
            Ok(false)
        }
    }

    /// Lazily expand ancestors and select an absolute path already below this tree root.
    pub fn select_path(&mut self, path: &str) -> IoResult<bool> {
        let target = normalize_path(path);
        let Some(root) = self.nodes.first().map(|n| n.path.clone()) else {
            return Ok(false);
        };

        if target == root {
            self.selected = Some(0);
            self.ensure_selected_visible();
            self.dirty = true;
            return Ok(true);
        }

        let relative = if root == "/" {
            target.strip_prefix('/').unwrap_or(target.as_str())
        } else {
            let Some(rest) = target.strip_prefix(root.as_str()) else {
                return Ok(false);
            };
            let Some(rest) = rest.strip_prefix('/') else {
                return Ok(false);
            };
            rest
        };

        if relative.is_empty() {
            self.selected = Some(0);
            self.ensure_selected_visible();
            self.dirty = true;
            return Ok(true);
        }

        let mut current = 0usize;
        let mut current_path = root;
        for part in relative.split('/').filter(|part| !part.is_empty()) {
            let _ = self.expand_index(current)?;
            current_path = join_path(&current_path, part);
            let parent_depth = self.nodes[current].depth;
            let mut found = None;
            let mut i = current + 1;
            while i < self.nodes.len() {
                let depth = self.nodes[i].depth;
                if depth <= parent_depth {
                    break;
                }
                if depth == parent_depth + 1 && self.nodes[i].path == current_path {
                    found = Some(i);
                    break;
                }
                i += 1;
            }
            let Some(next) = found else {
                return Ok(false);
            };
            current = next;
        }

        self.selected = Some(current);
        self.ensure_selected_visible();
        self.dirty = true;
        Ok(true)
    }

    /// Walk the flat preorder tree without allocating a temporary visible list.
    fn for_each_visible(&self, mut f: impl FnMut(usize, usize)) {
        let mut hidden_below: Option<usize> = None;
        let mut visible_pos = 0usize;
        for (i, node) in self.nodes.iter().enumerate() {
            if let Some(depth) = hidden_below {
                if node.depth > depth {
                    continue;
                }
                hidden_below = None;
            }
            f(visible_pos, i);
            visible_pos += 1;
            if !node.expanded {
                hidden_below = Some(node.depth);
            }
        }
    }

    fn visible_count(&self) -> usize {
        let mut count = 0usize;
        self.for_each_visible(|_, _| count += 1);
        count
    }

    fn visible_node_at(&self, wanted: usize) -> Option<usize> {
        let mut found = None;
        self.for_each_visible(|pos, index| {
            if pos == wanted {
                found = Some(index);
            }
        });
        found
    }

    fn visible_position_of(&self, wanted: usize) -> Option<usize> {
        let mut found = None;
        self.for_each_visible(|pos, index| {
            if index == wanted {
                found = Some(pos);
            }
        });
        found
    }

    fn first_visible(&self) -> Option<usize> {
        self.visible_node_at(0)
    }

    fn last_visible(&self) -> Option<usize> {
        let count = self.visible_count();
        if count == 0 { None } else { self.visible_node_at(count - 1) }
    }

    fn expand_index(&mut self, index: usize) -> IoResult<bool> {
        if index >= self.nodes.len() {
            return Ok(false);
        }
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
        let entries = fs::read_dir_entries(&parent_path)?;
        let mut dirs: Vec<_> = entries.into_iter().filter(|e| e.is_dir()).collect();
        dirs.sort_by(|a, b| a.name.cmp(&b.name));

        let has_children = !dirs.is_empty();
        self.nodes[index].loaded = true;
        self.nodes[index].has_children = has_children;
        self.nodes[index].expanded = has_children;

        if has_children {
            let mut insert = index + 1;
            for entry in dirs {
                let child_path = join_path(&parent_path, &entry.name);
                self.nodes.insert(
                    insert,
                    TreeNode {
                        name: entry.name,
                        path: child_path,
                        depth: parent_depth + 1,
                        expanded: false,
                        loaded: false,
                        has_children: true,
                        is_mount_point: entry.is_mount_point,
                    },
                );
                insert += 1;
            }
        }

        self.dirty = true;
        Ok(true)
    }

    fn collapse_index(&mut self, index: usize) -> bool {
        let Some(node) = self.nodes.get_mut(index) else { return false; };
        if node.expanded {
            node.expanded = false;
            self.dirty = true;
            true
        } else {
            false
        }
    }

    fn toggle_index(&mut self, index: usize) -> IoResult<bool> {
        if self.nodes.get(index).map(|n| n.expanded).unwrap_or(false) {
            Ok(self.collapse_index(index))
        } else {
            self.expand_index(index)
        }
    }

    fn content_height(&self) -> i32 {
        self.visible_count() as i32 * ROW_H
    }

    fn max_scroll(&self) -> i32 {
        (self.content_height() - self.rect.h as i32).max(0)
    }

    pub fn scroll_to(&mut self, y: i32) {
        let next = y.max(0).min(self.max_scroll());
        if next != self.scroll_y {
            self.scroll_y = next;
            self.dirty = true;
        }
    }

    pub fn scroll_by(&mut self, dy: i32) {
        self.scroll_to(self.scroll_y.saturating_add(dy));
    }

    fn row_at(&self, x: i32, y: i32) -> Option<(usize, usize)> {
        if !self.rect.contains(x, y) || self.scrollbar_rect().map(|r| r.contains(x, y)).unwrap_or(false) {
            return None;
        }
        let local_y = y - self.rect.y + self.scroll_y;
        if local_y < 0 {
            return None;
        }
        let visible_pos = (local_y / ROW_H) as usize;
        let node_index = self.visible_node_at(visible_pos)?;
        Some((visible_pos, node_index))
    }

    fn row_rect(&self, visible_pos: usize) -> Rect {
        Rect::new(
            self.rect.x,
            self.rect.y + visible_pos as i32 * ROW_H - self.scroll_y,
            self.content_width() as u32,
            ROW_H as u32,
        )
    }

    fn expander_rect(&self, visible_pos: usize, node_index: usize) -> Rect {
        let row = self.row_rect(visible_pos);
        let depth = self.nodes[node_index].depth as i32;
        Rect::new(
            row.x + 4 + depth * INDENT,
            row.y + (ROW_H - EXPANDER) / 2,
            EXPANDER as u32,
            EXPANDER as u32,
        )
    }

    fn content_width(&self) -> i32 {
        (self.rect.w as i32 - if self.max_scroll() > 0 { SCROLLBAR_W } else { 0 }).max(1)
    }

    fn scrollbar_rect(&self) -> Option<Rect> {
        if self.max_scroll() <= 0 || self.rect.w < SCROLLBAR_W as u32 {
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
        let max = self.max_scroll();
        let content_h = self.content_height().max(1);
        let view_h = self.rect.h as i32;
        let thumb_h = ((view_h as i64 * view_h as i64) / content_h as i64)
            .max(14)
            .min(view_h as i64) as i32;
        let travel = (view_h - thumb_h).max(0);
        let top = if max == 0 { 0 } else { (self.scroll_y as i64 * travel as i64 / max as i64) as i32 };
        Some(Rect::new(bar.x, bar.y + top, bar.w, thumb_h as u32))
    }

    fn scroll_from_thumb_top(&mut self, top: i32) {
        let Some(bar) = self.scrollbar_rect() else { return; };
        let Some(thumb) = self.scrollbar_thumb() else { return; };
        let travel = (bar.h as i32 - thumb.h as i32).max(0);
        if travel <= 0 {
            self.scroll_to(0);
            return;
        }
        let local = (top - bar.y).max(0).min(travel);
        let max = self.max_scroll();
        self.scroll_to((local as i64 * max as i64 / travel as i64) as i32);
    }

    fn ensure_selected_visible(&mut self) {
        let Some(selected) = self.selected else { return; };
        let Some(pos) = self.visible_position_of(selected) else { return; };
        let row = self.row_rect(pos);
        if row.y < self.rect.y {
            self.scroll_by(row.y - self.rect.y);
        } else if row.y + row.h as i32 > self.rect.y + self.rect.h as i32 {
            self.scroll_by(row.y + row.h as i32 - (self.rect.y + self.rect.h as i32));
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let count = self.visible_count();
        if count == 0 {
            self.selected = None;
            return;
        }
        let current_pos = self.selected
            .and_then(|s| self.visible_position_of(s))
            .unwrap_or(0) as isize;
        let next_pos = (current_pos + delta).max(0).min(count as isize - 1) as usize;
        self.selected = self.visible_node_at(next_pos);
        self.ensure_selected_visible();
        self.dirty = true;
    }

    fn select_parent(&mut self) -> bool {
        let Some(selected) = self.selected else { return false; };
        let depth = self.nodes[selected].depth;
        if depth == 0 {
            return false;
        }
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
            SCAN_UP => {
                self.move_selection(-1);
                EventResult::Changed
            }
            SCAN_DOWN => {
                self.move_selection(1);
                EventResult::Changed
            }
            SCAN_HOME => {
                if let Some(i) = self.first_visible() {
                    self.selected = Some(i);
                    self.ensure_selected_visible();
                    self.dirty = true;
                }
                EventResult::Changed
            }
            SCAN_END => {
                if let Some(i) = self.last_visible() {
                    self.selected = Some(i);
                    self.ensure_selected_visible();
                    self.dirty = true;
                }
                EventResult::Changed
            }
            SCAN_PAGE_UP => {
                self.scroll_by(-(self.rect.h as i32).max(1));
                EventResult::Changed
            }
            SCAN_PAGE_DOWN => {
                self.scroll_by((self.rect.h as i32).max(1));
                EventResult::Changed
            }
            SCAN_RIGHT => {
                if let Some(i) = self.selected {
                    let _ = self.expand_index(i);
                }
                EventResult::Changed
            }
            SCAN_LEFT => {
                if let Some(i) = self.selected {
                    if !self.collapse_index(i) {
                        let _ = self.select_parent();
                    }
                }
                EventResult::Changed
            }
            SCAN_ENTER => {
                if let Some(i) = self.selected {
                    if let Some(node) = self.nodes.get(i) {
                        self.pending_path = Some(node.path.clone());
                        self.dirty = true;
                        return EventResult::Submitted;
                    }
                }
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    }

    fn icon_for(&self, index: usize) -> Option<&IconImage> {
        let node = self.nodes.get(index)?;
        if node.depth == 0 {
            self.icons.root.as_ref().or(self.icons.mount.as_ref()).or(self.icons.folder_open.as_ref())
        } else if node.is_mount_point {
            self.icons.mount.as_ref().or(self.icons.folder.as_ref())
        } else if node.expanded {
            self.icons.folder_open.as_ref().or(self.icons.folder.as_ref())
        } else {
            self.icons.folder.as_ref()
        }
    }

    fn draw_icon(&self, win: &mut crate::wm::Window, image: &IconImage, x: i32, y: i32) {
        if image.width == 0 || image.height == 0 {
            return;
        }
        let draw_w = (image.width as i32).min(ICON_SIZE);
        let draw_h = (image.height as i32).min(ICON_SIZE);
        let sx0 = ((image.width as i32 - draw_w) / 2).max(0) as usize;
        let sy0 = ((image.height as i32 - draw_h) / 2).max(0) as usize;
        let dx0 = x + (ICON_SIZE - draw_w) / 2;
        let dy0 = y + (ICON_SIZE - draw_h) / 2;
        let src_w = image.width as usize;
        let pixels = image.pixels();
        let mask = image.mask();
        let iter = (0..draw_h as usize).flat_map(|dy| {
            (0..draw_w as usize).filter_map(move |dx| {
                let index = (sy0 + dy).checked_mul(src_w)?.checked_add(sx0 + dx)?;
                if !mask_opaque(mask, index) {
                    return None;
                }
                Some(Pixel(
                    Point::new(dx0 + dx as i32, dy0 + dy as i32),
                    rgb565_to_rgb888(*pixels.get(index)?),
                ))
            })
        });
        let _ = win.draw_iter(iter);
    }

    fn draw_expander(&self, win: &mut crate::wm::Window, rect: Rect, expanded: bool) {
        let box_rect = Rectangle::new(Point::new(rect.x, rect.y), Size::new(rect.w, rect.h));
        let _ = box_rect.into_styled(PrimitiveStyle::with_stroke(LINE, 1)).draw(win);
        let cy = rect.y + rect.h as i32 / 2;
        let cx = rect.x + rect.w as i32 / 2;
        let _ = Line::new(Point::new(rect.x + 2, cy), Point::new(rect.x + rect.w as i32 - 3, cy))
            .into_styled(PrimitiveStyle::with_stroke(LINE, 1))
            .draw(win);
        if !expanded {
            let _ = Line::new(Point::new(cx, rect.y + 2), Point::new(cx, rect.y + rect.h as i32 - 3))
                .into_styled(PrimitiveStyle::with_stroke(LINE, 1))
                .draw(win);
        }
    }
}

impl Default for TreeView {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for TreeView {
    fn measure(&self, c: Constraints) -> TSize<f32> {
        c.clamp(TSize { width: 190.0, height: 240.0 })
    }

    fn set_rect(&mut self, rect: Rect) {
        if self.rect != rect {
            self.rect = rect;
            self.scroll_y = self.scroll_y.max(0).min(self.max_scroll());
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
        let _ = Rectangle::new(Point::new(self.rect.x, self.rect.y), Size::new(self.rect.w, self.rect.h))
            .into_styled(PrimitiveStyle::with_fill(BG))
            .draw(win);

        self.for_each_visible(|pos, index| {
            let row = self.row_rect(pos);
            if row.y + row.h as i32 <= self.rect.y || row.y >= self.rect.y + self.rect.h as i32 {
                return;
            }
            let selected = self.selected == Some(index);
            let hovered = self.hovered == Some(index);
            if selected || hovered {
                let _ = Rectangle::new(Point::new(row.x, row.y), Size::new(row.w, row.h))
                    .into_styled(PrimitiveStyle::with_fill(if selected { SELECTED } else { HOVER }))
                    .draw(win);
            }

            let node = &self.nodes[index];
            let exp = self.expander_rect(pos, index);
            if node.has_children {
                self.draw_expander(win, exp, node.expanded);
            }

            let icon_x = exp.x + EXPANDER + 3;
            let icon_y = row.y + (ROW_H - ICON_SIZE) / 2;
            if let Some(icon) = self.icon_for(index) {
                self.draw_icon(win, icon, icon_x, icon_y);
            }

            let text_x = icon_x + ICON_SIZE + 4;
            let max_chars = ((self.content_width() - (text_x - row.x) - 4) / FONT_W).max(1);
            let name = truncate_text(&node.name, max_chars);
            let style = MonoTextStyle::new(font(), if selected { SELECTED_TEXT } else { TEXT });
            let _ = Text::with_baseline(
                name,
                Point::new(text_x, row.y + (ROW_H - FONT_H) / 2),
                style,
                Baseline::Top,
            )
            .draw(win);
        });

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
            UiEvent::Down { x, y } => {
                if !self.rect.contains(x, y) {
                    return EventResult::Ignored;
                }
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

                let hit = self.row_at(x, y);
                self.pressed = hit.map(|(_, i)| i);
                if let Some((pos, index)) = hit {
                    let exp = self.expander_rect(pos, index);
                    if exp.contains(x, y) && self.nodes[index].has_children {
                        self.pressed = None;
                        let _ = self.toggle_index(index);
                        return EventResult::Changed;
                    }
                    if self.selected != Some(index) {
                        self.selected = Some(index);
                        self.dirty = true;
                        return EventResult::Changed;
                    }
                    EventResult::Consumed
                } else {
                    EventResult::Consumed
                }
            }
            UiEvent::Move { x, y } => {
                if self.dragging_scrollbar {
                    self.scroll_from_thumb_top(y - self.scrollbar_grab_y);
                    return EventResult::Changed;
                }
                let hover = self.row_at(x, y).map(|(_, i)| i);
                if hover != self.hovered {
                    self.hovered = hover;
                    self.dirty = true;
                    EventResult::Changed
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::Up { x, y } => {
                if self.dragging_scrollbar {
                    self.dragging_scrollbar = false;
                    return EventResult::Consumed;
                }
                let pressed = self.pressed.take();
                let hit = self.row_at(x, y).map(|(_, i)| i);
                if let (Some(a), Some(b)) = (pressed, hit) {
                    if a == b {
                        self.selected = Some(a);
                        self.pending_path = Some(self.nodes[a].path.clone());
                        self.dirty = true;
                        return EventResult::Clicked;
                    }
                }
                EventResult::Consumed
            }
            UiEvent::KeyDown { scancode, .. } if focused => self.handle_key(scancode),
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

fn normalize_path(path: &str) -> String {
    if path.is_empty() || path == "/" {
        String::from("/")
    } else {
        String::from(path.trim_end_matches('/'))
    }
}

fn join_path(parent: &str, name: &str) -> String {
    if parent == "/" || parent.is_empty() {
        let mut out = String::from("/");
        out.push_str(name);
        out
    } else {
        let mut out = String::from(parent.trim_end_matches('/'));
        out.push('/');
        out.push_str(name);
        out
    }
}

fn truncate_text(text: &str, max_chars: i32) -> &str {
    let max_chars = max_chars.max(1) as usize;
    match text.char_indices().nth(max_chars) {
        Some((byte, _)) => &text[..byte],
        None => text,
    }
}

fn mask_opaque(mask: Option<&[u8]>, index: usize) -> bool {
    let Some(mask) = mask else { return true; };
    let byte = index >> 3;
    if byte >= mask.len() {
        return false;
    }
    let bit = 7 - (index & 7);
    (mask[byte] & (1u8 << bit)) != 0
}

fn rgb565_to_rgb888(raw: u16) -> Rgb888 {
    let r5 = ((raw >> 11) & 0x1f) as u8;
    let g6 = ((raw >> 5) & 0x3f) as u8;
    let b5 = (raw & 0x1f) as u8;
    Rgb888::new(
        (r5 << 3) | (r5 >> 2),
        (g6 << 2) | (g6 >> 4),
        (b5 << 3) | (b5 >> 2),
    )
}
