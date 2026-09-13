use std::{
    fs,
    io,
    string::String,
    time::{Duration, Instant, UNIX_EPOCH},
    vec::Vec,
};

use popugos::window::Window;

use crate::{draw, Color, Constraints, EventResult, Image, Point, Rect, Size, Theme, UiEvent, Widget};

const FONT_W: i32 = 9;
const FONT_H: i32 = 18;
const ICON_CELL_W: i32 = 92;
const ICON_CELL_H: i32 = 82;
const LIST_ROW_H: i32 = 30;
const DETAILS_ROW_H: i32 = 28;
const DETAILS_HEADER_H: i32 = 26;
const SCROLLBAR_W: i32 = 10;
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

const VIEW_BG: Color = Color::new(0xFA, 0xFA, 0xFA);
const VIEW_TEXT: Color = Color::new(0x16, 0x16, 0x16);
const VIEW_MUTED: Color = Color::new(0x60, 0x60, 0x60);
const VIEW_HOVER: Color = Color::new(0xE6, 0xF0, 0xFF);
const VIEW_SELECTED: Color = Color::new(0x31, 0x6A, 0xC5);
const VIEW_SELECTED_TEXT: Color = Color::new(0xFF, 0xFF, 0xFF);
const VIEW_FOCUS: Color = Color::new(0x1C, 0x4E, 0xA1);
const HEADER_BG: Color = Color::new(0xEC, 0xEF, 0xF4);
const HEADER_BORDER: Color = Color::new(0xC8, 0xCC, 0xD2);

const SCAN_ENTER: u8 = 0x1C;
const SCAN_HOME: u8 = 0x47;
const SCAN_UP: u8 = 0x48;
const SCAN_PAGE_UP: u8 = 0x49;
const SCAN_LEFT: u8 = 0x4B;
const SCAN_RIGHT: u8 = 0x4D;
const SCAN_END: u8 = 0x4F;
const SCAN_DOWN: u8 = 0x50;
const SCAN_PAGE_DOWN: u8 = 0x51;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Directory,
    File,
    Executable,
    Device,
    Mount,
    Unknown,
}

impl FileKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Directory => "Folder",
            Self::File => "File",
            Self::Executable => "Executable",
            Self::Device => "Device",
            Self::Mount => "Drive",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Clone, Debug)]
pub struct FileItem {
    pub name: String,
    pub kind: FileKind,
    pub inode: u64,
    pub mode: u32,
    pub size: u64,
    pub mtime: u64,
}

impl FileItem {
    pub fn new(name: impl Into<String>, kind: FileKind) -> Self {
        Self { name: name.into(), kind, inode: 0, mode: 0, size: 0, mtime: 0 }
    }
    pub fn with_inode(mut self, inode: u64) -> Self { self.inode = inode; self }
    pub fn with_mode(mut self, mode: u32) -> Self { self.mode = mode; self }
    pub fn with_size(mut self, size: u64) -> Self { self.size = size; self }
    pub fn with_mtime(mut self, mtime: u64) -> Self { self.mtime = mtime; self }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileViewMode { Icons, List, Details }

#[derive(Clone, Debug, Default)]
pub struct FileViewIcons {
    pub directory: Option<Image>,
    pub file: Option<Image>,
    pub executable: Option<Image>,
    pub device: Option<Image>,
    pub mount: Option<Image>,
    pub unknown: Option<Image>,
}

impl FileViewIcons {
    fn get(&self, kind: FileKind) -> Option<&Image> {
        match kind {
            FileKind::Directory => self.directory.as_ref(),
            FileKind::File => self.file.as_ref(),
            FileKind::Executable => self.executable.as_ref(),
            FileKind::Device => self.device.as_ref(),
            FileKind::Mount => self.mount.as_ref(),
            FileKind::Unknown => self.unknown.as_ref(),
        }
    }
    fn get_mut(&mut self, kind: FileKind) -> &mut Option<Image> {
        match kind {
            FileKind::Directory => &mut self.directory,
            FileKind::File => &mut self.file,
            FileKind::Executable => &mut self.executable,
            FileKind::Device => &mut self.device,
            FileKind::Mount => &mut self.mount,
            FileKind::Unknown => &mut self.unknown,
        }
    }
}

pub struct FileView {
    rect: Rect,
    path: String,
    items: Vec<FileItem>,
    icons: FileViewIcons,
    extension_icons: Vec<(String, Image)>,
    mode: FileViewMode,
    selected: Option<usize>,
    hovered: Option<usize>,
    pressed: Option<usize>,
    activated: Option<usize>,
    focused: bool,
    scroll_y: i32,
    dragging_scrollbar: bool,
    scrollbar_grab_y: i32,
    last_click: Option<(usize, Instant)>,
    dirty: bool,
    damage: Option<Rect>,
}

impl FileView {
    pub fn new() -> Self {
        Self {
            rect: Rect::default(),
            path: String::from("/"),
            items: Vec::new(),
            icons: FileViewIcons::default(),
            extension_icons: Vec::new(),
            mode: FileViewMode::Icons,
            selected: None,
            hovered: None,
            pressed: None,
            activated: None,
            focused: false,
            scroll_y: 0,
            dragging_scrollbar: false,
            scrollbar_grab_y: 0,
            last_click: None,
            dirty: true,
            damage: None,
        }
    }

    pub fn path(&self) -> &str { &self.path }
    pub fn items(&self) -> &[FileItem] { &self.items }

    /// Load a directory using the standard library. PopUI deliberately has no
    /// private filesystem layer.
    pub fn load_dir(&mut self, path: &str) -> io::Result<()> {
        let mut items = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = entry.file_type()?;
            let metadata = entry.metadata().ok();
            let kind = if file_type.is_dir() {
                FileKind::Directory
            } else if file_type.is_file() {
                FileKind::File
            } else {
                FileKind::Unknown
            };
            let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
            let mtime = metadata
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            items.push(FileItem::new(name, kind).with_size(size).with_mtime(mtime));
        }
        items.sort_by(|a, b| {
            file_kind_sort_key(a.kind)
                .cmp(&file_kind_sort_key(b.kind))
                .then_with(|| a.name.cmp(&b.name))
        });
        self.path = normalized_view_path(path);
        self.set_items(items);
        Ok(())
    }

    pub fn reload(&mut self) -> io::Result<()> {
        let path = self.path.clone();
        self.load_dir(&path)
    }

    pub fn item_path(&self, index: usize) -> Option<String> {
        let item = self.items.get(index)?;
        Some(join_view_path(&self.path, &item.name))
    }
    pub fn selected_path(&self) -> Option<String> { self.selected.and_then(|i| self.item_path(i)) }
    pub fn take_activated_path(&mut self) -> Option<String> {
        let index = self.activated.take()?;
        self.item_path(index)
    }
    pub fn items_mut(&mut self) -> &mut Vec<FileItem> { self.dirty = true; &mut self.items }
    pub fn set_items(&mut self, items: Vec<FileItem>) {
        self.items = items;
        self.selected = None;
        self.hovered = None;
        self.pressed = None;
        self.activated = None;
        self.scroll_y = 0;
        self.last_click = None;
        self.dirty = true;
    }
    pub fn clear(&mut self) { self.set_items(Vec::new()); }
    pub fn mode(&self) -> FileViewMode { self.mode }
    pub fn set_mode(&mut self, mode: FileViewMode) {
        if self.mode != mode { self.mode = mode; self.scroll_y = 0; self.dirty = true; }
    }
    pub fn icons(&self) -> &FileViewIcons { &self.icons }
    pub fn icons_mut(&mut self) -> &mut FileViewIcons { self.dirty = true; &mut self.icons }
    pub fn set_kind_icon(&mut self, kind: FileKind, icon: Image) { *self.icons.get_mut(kind) = Some(icon); self.dirty = true; }
    pub fn clear_kind_icon(&mut self, kind: FileKind) { *self.icons.get_mut(kind) = None; self.dirty = true; }
    pub fn set_extension_icon(&mut self, extension: &str, icon: Image) {
        let extension = extension.trim_start_matches('.').to_ascii_lowercase();
        if extension.is_empty() { return; }
        if let Some((_, existing)) = self.extension_icons.iter_mut().find(|(ext, _)| ext == &extension) {
            *existing = icon;
        } else {
            self.extension_icons.push((extension, icon));
        }
        self.dirty = true;
    }
    pub fn clear_extension_icons(&mut self) {
        self.extension_icons.clear();
        self.dirty = true;
    }
    pub fn selected_index(&self) -> Option<usize> { self.selected }
    pub fn selected_item(&self) -> Option<&FileItem> { self.selected.and_then(|i| self.items.get(i)) }
    pub fn select(&mut self, index: Option<usize>) {
        let index = index.filter(|&i| i < self.items.len());
        if self.selected != index { self.selected = index; self.ensure_selected_visible(); self.dirty = true; }
    }
    pub fn take_activated(&mut self) -> Option<usize> { self.activated.take() }
    pub fn take_activated_item(&mut self) -> Option<&FileItem> {
        let index = self.activated.take()?;
        self.items.get(index)
    }
    pub fn scroll_y(&self) -> i32 { self.scroll_y }
    pub fn scroll_to(&mut self, y: i32) {
        let next = y.clamp(0, self.max_scroll());
        if next != self.scroll_y { self.scroll_y = next; self.dirty = true; }
    }
    pub fn scroll_by(&mut self, dy: i32) { self.scroll_to(self.scroll_y.saturating_add(dy)); }

    fn mark_hover_damage(&mut self, old: Option<usize>, new: Option<usize>) {
        let was_dirty = self.dirty;
        let old_rect = old.and_then(|index| self.item_rect(index)).and_then(|rect| rect.intersect(self.rect));
        let new_rect = new.and_then(|index| self.item_rect(index)).and_then(|rect| rect.intersect(self.rect));
        self.dirty = true;
        self.damage = if was_dirty { None } else { union_optional_rects(old_rect, new_rect) };
    }

    fn content_width(&self) -> i32 {
        (self.rect.w as i32 - if self.max_scroll_for_width(self.rect.w as i32) > 0 { SCROLLBAR_W } else { 0 }).max(1)
    }
    fn icon_columns_for_width(&self, width: i32) -> usize { (width.max(1) / ICON_CELL_W).max(1) as usize }
    fn content_height_for_width(&self, width: i32) -> i32 {
        match self.mode {
            FileViewMode::Icons => {
                let cols = self.icon_columns_for_width(width);
                let rows = if self.items.is_empty() { 0 } else { self.items.len().div_ceil(cols) };
                rows as i32 * ICON_CELL_H
            }
            FileViewMode::List => self.items.len() as i32 * LIST_ROW_H,
            FileViewMode::Details => DETAILS_HEADER_H + self.items.len() as i32 * DETAILS_ROW_H,
        }
    }
    fn max_scroll_for_width(&self, width: i32) -> i32 { (self.content_height_for_width(width) - self.rect.h as i32).max(0) }
    fn max_scroll(&self) -> i32 {
        let mut width = self.rect.w as i32;
        if self.max_scroll_for_width(width) > 0 { width = (width - SCROLLBAR_W).max(1); }
        self.max_scroll_for_width(width)
    }

    fn item_rect(&self, index: usize) -> Option<Rect> {
        if index >= self.items.len() { return None; }
        match self.mode {
            FileViewMode::Icons => {
                let cols = self.icon_columns_for_width(self.content_width());
                let col = index % cols;
                let row = index / cols;
                Some(Rect::new(self.rect.x + col as i32 * ICON_CELL_W, self.rect.y + row as i32 * ICON_CELL_H - self.scroll_y, ICON_CELL_W as u32, ICON_CELL_H as u32))
            }
            FileViewMode::List => Some(Rect::new(self.rect.x, self.rect.y + index as i32 * LIST_ROW_H - self.scroll_y, self.content_width() as u32, LIST_ROW_H as u32)),
            FileViewMode::Details => Some(Rect::new(self.rect.x, self.rect.y + DETAILS_HEADER_H + index as i32 * DETAILS_ROW_H - self.scroll_y, self.content_width() as u32, DETAILS_ROW_H as u32)),
        }
    }

    fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        if !self.rect.contains(x, y) || self.scrollbar_rect().map(|r| r.contains(x, y)).unwrap_or(false) { return None; }
        match self.mode {
            FileViewMode::Icons => {
                let lx = x - self.rect.x;
                let ly = y - self.rect.y + self.scroll_y;
                if lx < 0 || ly < 0 { return None; }
                let cols = self.icon_columns_for_width(self.content_width());
                let col = (lx / ICON_CELL_W) as usize;
                if col >= cols { return None; }
                let index = (ly / ICON_CELL_H) as usize * cols + col;
                (index < self.items.len()).then_some(index)
            }
            FileViewMode::List => {
                let ly = y - self.rect.y + self.scroll_y;
                if ly < 0 { None } else { let i = (ly / LIST_ROW_H) as usize; (i < self.items.len()).then_some(i) }
            }
            FileViewMode::Details => {
                let ly = y - self.rect.y + self.scroll_y - DETAILS_HEADER_H;
                if ly < 0 { None } else { let i = (ly / DETAILS_ROW_H) as usize; (i < self.items.len()).then_some(i) }
            }
        }
    }

    fn scrollbar_rect(&self) -> Option<Rect> {
        (self.max_scroll() > 0 && self.rect.w >= SCROLLBAR_W as u32).then_some(Rect::new(
            self.rect.x + self.rect.w as i32 - SCROLLBAR_W,
            self.rect.y,
            SCROLLBAR_W as u32,
            self.rect.h,
        ))
    }
    fn scrollbar_thumb(&self) -> Option<Rect> {
        let bar = self.scrollbar_rect()?;
        let max = self.max_scroll();
        let view_h = self.rect.h as i32;
        let content_h = self.content_height_for_width(self.content_width()).max(1);
        let thumb_h = ((view_h as i64 * view_h as i64) / content_h as i64).max(14).min(view_h as i64) as i32;
        let travel = (view_h - thumb_h).max(0);
        let top = if max == 0 { 0 } else { (self.scroll_y as i64 * travel as i64 / max as i64) as i32 };
        Some(Rect::new(bar.x, bar.y + top, bar.w, thumb_h as u32))
    }
    fn scroll_from_thumb_top(&mut self, top: i32) {
        let Some(bar) = self.scrollbar_rect() else { return; };
        let Some(thumb) = self.scrollbar_thumb() else { return; };
        let travel = (bar.h as i32 - thumb.h as i32).max(0);
        if travel == 0 { self.scroll_to(0); return; }
        let local = (top - bar.y).clamp(0, travel);
        let max = self.max_scroll();
        self.scroll_to((local as i64 * max as i64 / travel as i64) as i32);
    }
    fn ensure_selected_visible(&mut self) {
        let Some(index) = self.selected else { return; };
        let Some(r) = self.item_rect(index) else { return; };
        let top = self.rect.y + if self.mode == FileViewMode::Details { DETAILS_HEADER_H } else { 0 };
        let bottom = self.rect.y + self.rect.h as i32;
        if r.y < top { self.scroll_by(r.y - top); }
        else if r.y + r.h as i32 > bottom { self.scroll_by(r.y + r.h as i32 - bottom); }
    }
    fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() { self.select(None); return; }
        let current = self.selected.unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, self.items.len() as isize - 1) as usize;
        self.select(Some(next));
    }
    fn handle_key(&mut self, scancode: u8) -> EventResult {
        if self.items.is_empty() { return EventResult::Consumed; }
        match scancode {
            SCAN_ENTER => { if let Some(i) = self.selected { self.activated = Some(i); self.dirty = true; EventResult::Submitted } else { EventResult::Consumed } }
            SCAN_HOME => { self.select(Some(0)); EventResult::Changed }
            SCAN_END => { self.select(Some(self.items.len() - 1)); EventResult::Changed }
            SCAN_LEFT => { self.move_selection(-1); EventResult::Changed }
            SCAN_RIGHT => { self.move_selection(1); EventResult::Changed }
            SCAN_UP => { let step = if self.mode == FileViewMode::Icons { self.icon_columns_for_width(self.content_width()) as isize } else { 1 }; self.move_selection(-step); EventResult::Changed }
            SCAN_DOWN => { let step = if self.mode == FileViewMode::Icons { self.icon_columns_for_width(self.content_width()) as isize } else { 1 }; self.move_selection(step); EventResult::Changed }
            SCAN_PAGE_UP => { self.scroll_by(-(self.rect.h as i32).max(1)); EventResult::Changed }
            SCAN_PAGE_DOWN => { self.scroll_by((self.rect.h as i32).max(1)); EventResult::Changed }
            _ => EventResult::Ignored,
        }
    }

    fn icon_for_item(&self, item: &FileItem) -> Option<&Image> {
        if item.kind == FileKind::File {
            if let Some((_, ext)) = item.name.rsplit_once('.') {
                if let Some((_, image)) = self.extension_icons.iter().find(|(candidate, _)| candidate.eq_ignore_ascii_case(ext)) {
                    return Some(image);
                }
            }
        }
        self.icons.get(item.kind)
    }

    fn draw_icon_at(&self, window: &mut Window, image: &Image, x: i32, y: i32, max_w: i32, max_h: i32) {
        draw::blit_rgb565_cropped(
            window,
            Point::new(x, y),
            image.width,
            image.height,
            image.pixels(),
            image.mask(),
            max_w,
            max_h,
        );
    }

    fn draw_icons(&self, window: &mut Window) {
        for (i, item) in self.items.iter().enumerate() {
            let Some(cell) = self.item_rect(i) else { continue; };
            if cell.intersect(self.rect).is_none() { continue; }
            let selected = self.selected == Some(i);
            let hovered = self.hovered == Some(i);
            if selected || hovered {
                draw::fill_rect(window, cell, if selected { VIEW_SELECTED } else { VIEW_HOVER });
                if selected && self.focused { draw::stroke_rect(window, inset_rect(cell, 1), VIEW_FOCUS, 1); }
            }
            if let Some(icon) = self.icon_for_item(item) { self.draw_icon_at(window, icon, cell.x + 8, cell.y + 5, ICON_CELL_W - 16, 42); }
            let name = truncate_text(&item.name, (ICON_CELL_W - 10) / FONT_W);
            let tw = name.chars().count() as i32 * FONT_W;
            draw::text(window, Point::new(cell.x + ((ICON_CELL_W - tw) / 2).max(3), cell.y + 53), name, if selected { VIEW_SELECTED_TEXT } else { VIEW_TEXT });
        }
    }

    fn draw_list(&self, window: &mut Window) {
        for (i, item) in self.items.iter().enumerate() {
            let Some(row) = self.item_rect(i) else { continue; };
            if row.intersect(self.rect).is_none() { continue; }
            let selected = self.selected == Some(i);
            let hovered = self.hovered == Some(i);
            if selected || hovered { draw::fill_rect(window, row, if selected { VIEW_SELECTED } else { VIEW_HOVER }); }
            if let Some(icon) = self.icon_for_item(item) { self.draw_icon_at(window, icon, row.x + 4, row.y + 3, 24, 24); }
            let name = truncate_text(&item.name, ((self.content_width() - 38) / FONT_W).max(1));
            draw::text(window, Point::new(row.x + 34, row.y + (LIST_ROW_H - FONT_H) / 2), name, if selected { VIEW_SELECTED_TEXT } else { VIEW_TEXT });
        }
    }

    fn draw_details(&self, window: &mut Window) {
        let width = self.content_width();
        let header = Rect::new(self.rect.x, self.rect.y, width as u32, DETAILS_HEADER_H as u32);
        draw::fill_rect(window, header, HEADER_BG);
        draw::stroke_rect(window, header, HEADER_BORDER, 1);
        let name_w = (width * 55 / 100).max(110);
        let size_w = (width * 20 / 100).max(70);
        let kind_x = self.rect.x + name_w + size_w;
        draw::text(window, Point::new(self.rect.x + 6, self.rect.y + 4), "Name", VIEW_MUTED);
        draw::text(window, Point::new(self.rect.x + name_w + 6, self.rect.y + 4), "Size", VIEW_MUTED);
        draw::text(window, Point::new(kind_x + 6, self.rect.y + 4), "Type", VIEW_MUTED);

        for (i, item) in self.items.iter().enumerate() {
            let Some(row) = self.item_rect(i) else { continue; };
            if row.intersect(self.rect).is_none() || row.y < self.rect.y + DETAILS_HEADER_H { continue; }
            let selected = self.selected == Some(i);
            let hovered = self.hovered == Some(i);
            if selected || hovered { draw::fill_rect(window, row, if selected { VIEW_SELECTED } else { VIEW_HOVER }); }
            if let Some(icon) = self.icon_for_item(item) { self.draw_icon_at(window, icon, row.x + 3, row.y + 3, 22, 22); }
            let color = if selected { VIEW_SELECTED_TEXT } else { VIEW_TEXT };
            let name = truncate_text(&item.name, ((name_w - 34) / FONT_W).max(1));
            draw::text(window, Point::new(row.x + 30, row.y + 5), name, color);
            if item.kind != FileKind::Directory {
                draw::text(window, Point::new(row.x + name_w + 6, row.y + 5), &format_size(item.size), color);
            }
            draw::text(window, Point::new(kind_x + 6, row.y + 5), item.kind.label(), color);
        }
    }

    fn draw_scrollbar(&self, window: &mut Window, theme: &Theme) {
        let Some(bar) = self.scrollbar_rect() else { return; };
        draw::fill_rect(window, bar, theme.scrollbar_background);
        if let Some(thumb) = self.scrollbar_thumb() { draw::fill_rect(window, thumb, theme.scrollbar_thumb); }
    }
}

impl Default for FileView { fn default() -> Self { Self::new() } }

impl Widget for FileView {
    fn measure(&self, constraints: Constraints) -> Size { constraints.clamp(Size::new(320.0, 220.0)) }
    fn set_rect(&mut self, rect: Rect) {
        if self.rect != rect { self.rect = rect; self.scroll_y = self.scroll_y.clamp(0, self.max_scroll()); self.dirty = true; }
    }
    fn rect(&self) -> Rect { self.rect }
    fn draw(&self, window: &mut Window, theme: &Theme) {
        if self.rect.w == 0 || self.rect.h == 0 { return; }
        draw::fill_rect(window, self.rect, VIEW_BG);
        match self.mode {
            FileViewMode::Icons => self.draw_icons(window),
            FileViewMode::List => self.draw_list(window),
            FileViewMode::Details => self.draw_details(window),
        }
        self.draw_scrollbar(window, theme);
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
                let hit = self.item_at(x, y);
                self.pressed = hit;
                if self.selected != hit { self.selected = hit; self.dirty = true; EventResult::Changed } else { EventResult::Consumed }
            }
            UiEvent::Move { x, y } => {
                if self.dragging_scrollbar { self.scroll_from_thumb_top(y - self.scrollbar_grab_y); return EventResult::Changed; }
                let hover = self.item_at(x, y);
                if hover != self.hovered {
                    let old = self.hovered;
                    self.hovered = hover;
                    self.mark_hover_damage(old, hover);
                    EventResult::Changed
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::Leave => {
                if let Some(old) = self.hovered.take() {
                    self.mark_hover_damage(Some(old), None);
                    EventResult::Changed
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::Wheel { delta, .. } => {
                let old = self.scroll_y;
                self.scroll_by(delta.saturating_mul(LIST_ROW_H * 3));
                if self.scroll_y != old { EventResult::Changed } else { EventResult::Consumed }
            }
            UiEvent::Up { x, y } => {
                if self.dragging_scrollbar { self.dragging_scrollbar = false; return EventResult::Consumed; }
                let pressed = self.pressed.take();
                let hit = self.item_at(x, y);
                if let (Some(a), Some(b)) = (pressed, hit) {
                    if a == b {
                        let now = Instant::now();
                        let double = self.last_click.map(|(i, at)| i == a && now.duration_since(at) <= DOUBLE_CLICK).unwrap_or(false);
                        if double {
                            self.last_click = None;
                            self.activated = Some(a);
                            self.dirty = true;
                            return EventResult::Clicked;
                        }
                        self.last_click = Some((a, now));
                    }
                }
                EventResult::Consumed
            }
            UiEvent::Context { x, y } if self.rect.contains(x, y) => {
                let hit = self.item_at(x, y);
                if self.selected != hit {
                    self.selected = hit;
                    self.ensure_selected_visible();
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
    fn dirty_region(&self) -> Option<Rect> { self.damage }
    fn clear_dirty(&mut self) { self.dirty = false; self.damage = None; }
    fn as_any(&self) -> &dyn core::any::Any { self }
    fn as_any_mut(&mut self) -> &mut dyn core::any::Any { self }
}

fn union_optional_rects(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let x = a.x.min(b.x);
            let y = a.y.min(b.y);
            let right = (a.x + a.w as i32).max(b.x + b.w as i32);
            let bottom = (a.y + a.h as i32).max(b.y + b.h as i32);
            Some(Rect::new(x, y, (right - x) as u32, (bottom - y) as u32))
        }
        (Some(rect), None) | (None, Some(rect)) => Some(rect),
        (None, None) => None,
    }
}

fn truncate_text(text: &str, max_chars: i32) -> &str {
    match text.char_indices().nth(max_chars.max(1) as usize) { Some((byte, _)) => &text[..byte], None => text }
}
fn file_kind_sort_key(kind: FileKind) -> u8 {
    match kind { FileKind::Mount => 0, FileKind::Directory => 1, FileKind::Executable => 2, FileKind::File => 3, FileKind::Device => 4, FileKind::Unknown => 5 }
}
fn normalized_view_path(path: &str) -> String {
    if path.is_empty() || path == "/" { String::from("/") } else { let p = path.trim_end_matches('/'); if p.is_empty() { String::from("/") } else { String::from(p) } }
}
fn join_view_path(parent: &str, name: &str) -> String {
    if parent.is_empty() || parent == "/" { format!("/{name}") } else { format!("{}/{}", parent.trim_end_matches('/'), name) }
}
fn format_size(bytes: u64) -> String {
    if bytes < 1024 { format!("{bytes} B") }
    else if bytes < 1024 * 1024 { format!("{} KB", bytes.div_ceil(1024)) }
    else if bytes < 1024 * 1024 * 1024 { format!("{} MB", bytes.div_ceil(1024 * 1024)) }
    else { format!("{} GB", bytes.div_ceil(1024 * 1024 * 1024)) }
}
fn inset_rect(rect: Rect, amount: i32) -> Rect {
    let amount = amount.max(0);
    Rect::new(rect.x + amount, rect.y + amount, rect.w.saturating_sub(amount as u32 * 2), rect.h.saturating_sub(amount as u32 * 2))
}
