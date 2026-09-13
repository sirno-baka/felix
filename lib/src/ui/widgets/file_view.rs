use super::{font, IconImage};
use crate::fs::{self, FileType, IoResult};
use crate::syscall::{clock_gettime, TimeSpec, CLOCK_MONOTONIC};
use crate::ui::{Constraints, EventResult, Rect, UiEvent, Widget};
use alloc::{format, string::String, vec::Vec};
use embedded_graphics::{
    mono_font::MonoTextStyle,
    pixelcolor::Rgb888,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
    Pixel,
};
use taffy::geometry::Size as TSize;

const FONT_W: i32 = 9;
const FONT_H: i32 = 18;
const ICON_CELL_W: i32 = 92;
const ICON_CELL_H: i32 = 82;
const LIST_ROW_H: i32 = 30;
const DETAILS_ROW_H: i32 = 28;
const DETAILS_HEADER_H: i32 = 26;
const SCROLLBAR_W: i32 = 10;
const DOUBLE_CLICK_MS: u64 = 500;

const VIEW_BG: Rgb888 = Rgb888::new(0xFA, 0xFA, 0xFA);
const VIEW_TEXT: Rgb888 = Rgb888::new(0x16, 0x16, 0x16);
const VIEW_MUTED: Rgb888 = Rgb888::new(0x60, 0x60, 0x60);
const VIEW_HOVER: Rgb888 = Rgb888::new(0xE6, 0xF0, 0xFF);
const VIEW_SELECTED: Rgb888 = Rgb888::new(0x31, 0x6A, 0xC5);
const VIEW_SELECTED_TEXT: Rgb888 = Rgb888::new(0xFF, 0xFF, 0xFF);
const VIEW_FOCUS: Rgb888 = Rgb888::new(0x1C, 0x4E, 0xA1);
const HEADER_BG: Rgb888 = Rgb888::new(0xEC, 0xEF, 0xF4);
const HEADER_BORDER: Rgb888 = Rgb888::new(0xC8, 0xCC, 0xD2);
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
        Self {
            name: name.into(),
            kind,
            inode: 0,
            mode: 0,
            size: 0,
            mtime: 0,
        }
    }

    pub fn with_inode(mut self, inode: u64) -> Self {
        self.inode = inode;
        self
    }

    pub fn with_mode(mut self, mode: u32) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_size(mut self, size: u64) -> Self {
        self.size = size;
        self
    }

    pub fn with_mtime(mut self, mtime: u64) -> Self {
        self.mtime = mtime;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileViewMode {
    Icons,
    List,
    Details,
}

#[derive(Clone, Debug, Default)]
pub struct FileViewIcons {
    pub directory: Option<IconImage>,
    pub file: Option<IconImage>,
    pub executable: Option<IconImage>,
    pub device: Option<IconImage>,
    pub mount: Option<IconImage>,
    pub unknown: Option<IconImage>,
}

impl FileViewIcons {
    fn get(&self, kind: FileKind) -> Option<&IconImage> {
        match kind {
            FileKind::Directory => self.directory.as_ref(),
            FileKind::File => self.file.as_ref(),
            FileKind::Executable => self.executable.as_ref(),
            FileKind::Device => self.device.as_ref(),
            FileKind::Mount => self.mount.as_ref(),
            FileKind::Unknown => self.unknown.as_ref(),
        }
    }

    fn get_mut(&mut self, kind: FileKind) -> &mut Option<IconImage> {
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
    mode: FileViewMode,
    selected: Option<usize>,
    hovered: Option<usize>,
    pressed: Option<usize>,
    activated: Option<usize>,
    focused: bool,
    scroll_y: i32,
    dragging_scrollbar: bool,
    scrollbar_grab_y: i32,
    last_click_index: Option<usize>,
    last_click_ms: u64,
    dirty: bool,
}

impl FileView {
    pub fn new() -> Self {
        Self {
            rect: Rect::default(),
            path: String::from("/"),
            items: Vec::new(),
            icons: FileViewIcons::default(),
            mode: FileViewMode::Icons,
            selected: None,
            hovered: None,
            pressed: None,
            activated: None,
            focused: false,
            scroll_y: 0,
            dragging_scrollbar: false,
            scrollbar_grab_y: 0,
            last_click_index: None,
            last_click_ms: 0,
            dirty: true,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn items(&self) -> &[FileItem] {
        &self.items
    }

    /// Read a directory from Felix VFS and replace the current contents.
    /// Directories and mount points are sorted before regular files.
    pub fn load_dir(&mut self, path: &str) -> IoResult<()> {
        let entries = fs::read_dir_entries(path)?;
        let mut items: Vec<FileItem> = entries
            .into_iter()
            .map(|entry| {
                let kind = if entry.is_mount_point {
                    FileKind::Mount
                } else if entry.is_dir() {
                    FileKind::Directory
                } else if entry.is_executable() {
                    FileKind::Executable
                } else {
                    match entry.file_type {
                        FileType::Regular => FileKind::File,
                        FileType::CharDevice
                        | FileType::BlockDevice
                        | FileType::Fifo
                        | FileType::Socket => FileKind::Device,
                        FileType::Directory => FileKind::Directory,
                        FileType::Unknown => FileKind::Unknown,
                    }
                };

                FileItem::new(entry.name, kind)
                    .with_inode(entry.inode)
                    .with_mode(entry.mode)
                    .with_size(entry.size)
                    .with_mtime(entry.mtime)
            })
            .collect();

        items.sort_by(|a, b| {
            file_kind_sort_key(a.kind)
                .cmp(&file_kind_sort_key(b.kind))
                .then_with(|| a.name.cmp(&b.name))
        });

        self.path = normalized_view_path(path);
        self.set_items(items);
        Ok(())
    }

    /// Reload the directory currently shown by this FileView.
    pub fn reload(&mut self) -> IoResult<()> {
        let path = self.path.clone();
        self.load_dir(&path)
    }

    /// Build the full path of one item in the current directory.
    pub fn item_path(&self, index: usize) -> Option<String> {
        let item = self.items.get(index)?;
        Some(join_view_path(&self.path, &item.name))
    }

    pub fn selected_path(&self) -> Option<String> {
        self.selected.and_then(|i| self.item_path(i))
    }

    /// Consume pending activation and return its full path.
    pub fn take_activated_path(&mut self) -> Option<String> {
        let index = self.activated.take()?;
        self.item_path(index)
    }

    pub fn items_mut(&mut self) -> &mut Vec<FileItem> {
        self.dirty = true;
        &mut self.items
    }

    pub fn set_items(&mut self, items: Vec<FileItem>) {
        self.items = items;
        self.selected = None;
        self.hovered = None;
        self.pressed = None;
        self.activated = None;
        self.scroll_y = 0;
        self.last_click_index = None;
        self.dirty = true;
    }

    pub fn clear(&mut self) {
        self.set_items(Vec::new());
    }

    pub fn mode(&self) -> FileViewMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: FileViewMode) {
        if self.mode != mode {
            self.mode = mode;
            self.scroll_y = 0;
            self.dirty = true;
        }
    }

    pub fn icons(&self) -> &FileViewIcons {
        &self.icons
    }

    pub fn icons_mut(&mut self) -> &mut FileViewIcons {
        self.dirty = true;
        &mut self.icons
    }

    pub fn set_kind_icon(&mut self, kind: FileKind, icon: IconImage) {
        *self.icons.get_mut(kind) = Some(icon);
        self.dirty = true;
    }

    pub fn clear_kind_icon(&mut self, kind: FileKind) {
        *self.icons.get_mut(kind) = None;
        self.dirty = true;
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    pub fn selected_item(&self) -> Option<&FileItem> {
        self.selected.and_then(|i| self.items.get(i))
    }

    pub fn select(&mut self, index: Option<usize>) {
        let index = index.filter(|&i| i < self.items.len());
        if self.selected != index {
            self.selected = index;
            self.ensure_selected_visible();
            self.dirty = true;
        }
    }

    /// Returns the item that was double-clicked or activated with Enter.
    /// The pending activation is consumed by this call.
    pub fn take_activated(&mut self) -> Option<usize> {
        self.activated.take()
    }

    pub fn take_activated_item(&mut self) -> Option<&FileItem> {
        let i = self.activated.take()?;
        self.items.get(i)
    }

    pub fn scroll_y(&self) -> i32 {
        self.scroll_y
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

    fn content_width(&self) -> i32 {
        let scrollbar = if self.max_scroll_for_width(self.rect.w as i32) > 0 {
            SCROLLBAR_W
        } else {
            0
        };
        (self.rect.w as i32 - scrollbar).max(1)
    }

    fn icon_columns_for_width(&self, width: i32) -> usize {
        (width.max(1) / ICON_CELL_W).max(1) as usize
    }

    fn content_height_for_width(&self, width: i32) -> i32 {
        match self.mode {
            FileViewMode::Icons => {
                let cols = self.icon_columns_for_width(width);
                let rows = if self.items.is_empty() {
                    0
                } else {
                    (self.items.len() + cols - 1) / cols
                };
                rows as i32 * ICON_CELL_H
            }
            FileViewMode::List => self.items.len() as i32 * LIST_ROW_H,
            FileViewMode::Details => {
                DETAILS_HEADER_H + self.items.len() as i32 * DETAILS_ROW_H
            }
        }
    }

    fn max_scroll_for_width(&self, width: i32) -> i32 {
        (self.content_height_for_width(width) - self.rect.h as i32).max(0)
    }

    fn max_scroll(&self) -> i32 {
        let mut width = self.rect.w as i32;
        let first = self.max_scroll_for_width(width);
        if first > 0 {
            width = (width - SCROLLBAR_W).max(1);
        }
        self.max_scroll_for_width(width)
    }

    fn item_rect(&self, index: usize) -> Option<Rect> {
        if index >= self.items.len() {
            return None;
        }

        match self.mode {
            FileViewMode::Icons => {
                let cols = self.icon_columns_for_width(self.content_width());
                let col = index % cols;
                let row = index / cols;
                Some(Rect::new(
                    self.rect.x + col as i32 * ICON_CELL_W,
                    self.rect.y + row as i32 * ICON_CELL_H - self.scroll_y,
                    ICON_CELL_W as u32,
                    ICON_CELL_H as u32,
                ))
            }
            FileViewMode::List => Some(Rect::new(
                self.rect.x,
                self.rect.y + index as i32 * LIST_ROW_H - self.scroll_y,
                self.content_width() as u32,
                LIST_ROW_H as u32,
            )),
            FileViewMode::Details => Some(Rect::new(
                self.rect.x,
                self.rect.y + DETAILS_HEADER_H + index as i32 * DETAILS_ROW_H - self.scroll_y,
                self.content_width() as u32,
                DETAILS_ROW_H as u32,
            )),
        }
    }

    fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        if !self.rect.contains(x, y) || self.scrollbar_rect().map(|r| r.contains(x, y)).unwrap_or(false) {
            return None;
        }

        match self.mode {
            FileViewMode::Icons => {
                let local_x = x - self.rect.x;
                let local_y = y - self.rect.y + self.scroll_y;
                if local_x < 0 || local_y < 0 {
                    return None;
                }
                let cols = self.icon_columns_for_width(self.content_width());
                let col = (local_x / ICON_CELL_W) as usize;
                if col >= cols {
                    return None;
                }
                let row = (local_y / ICON_CELL_H) as usize;
                let index = row * cols + col;
                (index < self.items.len()).then_some(index)
            }
            FileViewMode::List => {
                let local_y = y - self.rect.y + self.scroll_y;
                if local_y < 0 {
                    return None;
                }
                let index = (local_y / LIST_ROW_H) as usize;
                (index < self.items.len()).then_some(index)
            }
            FileViewMode::Details => {
                let local_y = y - self.rect.y + self.scroll_y - DETAILS_HEADER_H;
                if local_y < 0 {
                    return None;
                }
                let index = (local_y / DETAILS_ROW_H) as usize;
                (index < self.items.len()).then_some(index)
            }
        }
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
        if max <= 0 {
            return None;
        }
        let content_h = self.content_height_for_width(self.content_width()).max(1);
        let view_h = self.rect.h as i32;
        let thumb_h = ((view_h as i64 * view_h as i64) / content_h as i64)
            .max(14)
            .min(view_h as i64) as i32;
        let travel = (view_h - thumb_h).max(0);
        let top = if max == 0 {
            0
        } else {
            (self.scroll_y as i64 * travel as i64 / max as i64) as i32
        };
        Some(Rect::new(
            bar.x,
            bar.y + top,
            bar.w,
            thumb_h as u32,
        ))
    }

    fn scroll_from_thumb_top(&mut self, thumb_top: i32) {
        let Some(bar) = self.scrollbar_rect() else { return; };
        let Some(thumb) = self.scrollbar_thumb() else { return; };
        let travel = (bar.h as i32 - thumb.h as i32).max(0);
        if travel == 0 {
            self.scroll_to(0);
            return;
        }
        let local = (thumb_top - bar.y).max(0).min(travel);
        let max = self.max_scroll();
        self.scroll_to((local as i64 * max as i64 / travel as i64) as i32);
    }

    fn ensure_selected_visible(&mut self) {
        let Some(index) = self.selected else { return; };
        let Some(r) = self.item_rect(index) else { return; };
        let top_limit = self.rect.y + if self.mode == FileViewMode::Details { DETAILS_HEADER_H } else { 0 };
        let bottom_limit = self.rect.y + self.rect.h as i32;

        if r.y < top_limit {
            self.scroll_by(r.y - top_limit);
        } else if r.y + r.h as i32 > bottom_limit {
            self.scroll_by(r.y + r.h as i32 - bottom_limit);
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            self.select(None);
            return;
        }
        let current = self.selected.unwrap_or(0) as isize;
        let next = (current + delta).max(0).min(self.items.len() as isize - 1) as usize;
        self.select(Some(next));
    }

    fn handle_key(&mut self, scancode: u8) -> EventResult {
        if self.items.is_empty() {
            return EventResult::Consumed;
        }

        match scancode {
            SCAN_ENTER => {
                if let Some(i) = self.selected {
                    self.activated = Some(i);
                    self.dirty = true;
                    EventResult::Submitted
                } else {
                    EventResult::Consumed
                }
            }
            SCAN_HOME => {
                self.select(Some(0));
                EventResult::Changed
            }
            SCAN_END => {
                self.select(Some(self.items.len() - 1));
                EventResult::Changed
            }
            SCAN_LEFT => {
                self.move_selection(-1);
                EventResult::Changed
            }
            SCAN_RIGHT => {
                self.move_selection(1);
                EventResult::Changed
            }
            SCAN_UP => {
                let step = match self.mode {
                    FileViewMode::Icons => self.icon_columns_for_width(self.content_width()) as isize,
                    _ => 1,
                };
                self.move_selection(-step);
                EventResult::Changed
            }
            SCAN_DOWN => {
                let step = match self.mode {
                    FileViewMode::Icons => self.icon_columns_for_width(self.content_width()) as isize,
                    _ => 1,
                };
                self.move_selection(step);
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
            _ => EventResult::Ignored,
        }
    }

    fn draw_icon_at(&self, win: &mut crate::wm::Window, image: &IconImage, x: i32, y: i32, max_w: i32, max_h: i32) {
        if image.width == 0 || image.height == 0 || max_w <= 0 || max_h <= 0 {
            return;
        }

        let draw_w = (image.width as i32).min(max_w);
        let draw_h = (image.height as i32).min(max_h);
        let src_x0 = ((image.width as i32 - draw_w) / 2).max(0) as usize;
        let src_y0 = ((image.height as i32 - draw_h) / 2).max(0) as usize;
        let dst_x = x + (max_w - draw_w) / 2;
        let dst_y = y + (max_h - draw_h) / 2;
        let src_w = image.width as usize;
        let pixels = image.pixels();
        let mask = image.mask();

        let iter = (0..draw_h as usize).flat_map(|dy| {
            (0..draw_w as usize).filter_map(move |dx| {
                let sx = src_x0 + dx;
                let sy = src_y0 + dy;
                let index = sy.checked_mul(src_w)?.checked_add(sx)?;
                let raw = *pixels.get(index)?;
                if !mask_opaque(mask, index) {
                    return None;
                }
                Some(Pixel(
                    Point::new(dst_x + dx as i32, dst_y + dy as i32),
                    rgb565_to_rgb888(raw),
                ))
            })
        });
        let _ = win.draw_iter(iter);
    }

    fn draw_icons(&self, win: &mut crate::wm::Window) {
        let style = MonoTextStyle::new(font(), VIEW_TEXT);
        let selected_style = MonoTextStyle::new(font(), VIEW_SELECTED_TEXT);

        for (i, item) in self.items.iter().enumerate() {
            let Some(cell) = self.item_rect(i) else { continue; };
            if !rect_visible(cell, self.rect) {
                continue;
            }

            let selected = self.selected == Some(i);
            let hovered = self.hovered == Some(i);
            if selected || hovered {
                fill_rect(win, cell, if selected { VIEW_SELECTED } else { VIEW_HOVER });
                if selected && self.focused {
                    stroke_rect(win, inset_rect(cell, 1), VIEW_FOCUS);
                }
            }

            if let Some(icon) = self.icons.get(item.kind) {
                self.draw_icon_at(win, icon, cell.x + 8, cell.y + 5, ICON_CELL_W - 16, 42);
            }

            let name = truncate_text(&item.name, (ICON_CELL_W - 10) / FONT_W);
            let text_w = name.chars().count() as i32 * FONT_W;
            let tx = cell.x + ((ICON_CELL_W - text_w) / 2).max(3);
            let ty = cell.y + 53;
            let _ = Text::with_baseline(
                name,
                Point::new(tx, ty),
                if selected { selected_style } else { style },
                Baseline::Top,
            )
            .draw(win);
        }
    }

    fn draw_list(&self, win: &mut crate::wm::Window) {
        let style = MonoTextStyle::new(font(), VIEW_TEXT);
        let selected_style = MonoTextStyle::new(font(), VIEW_SELECTED_TEXT);

        for (i, item) in self.items.iter().enumerate() {
            let Some(row) = self.item_rect(i) else { continue; };
            if !rect_visible(row, self.rect) {
                continue;
            }
            let selected = self.selected == Some(i);
            let hovered = self.hovered == Some(i);
            if selected || hovered {
                fill_rect(win, row, if selected { VIEW_SELECTED } else { VIEW_HOVER });
            }
            if let Some(icon) = self.icons.get(item.kind) {
                self.draw_icon_at(win, icon, row.x + 4, row.y + 3, 24, 24);
            }
            let max_chars = ((self.content_width() - 38) / FONT_W).max(1);
            let name = truncate_text(&item.name, max_chars);
            let _ = Text::with_baseline(
                name,
                Point::new(row.x + 34, row.y + (LIST_ROW_H - FONT_H) / 2),
                if selected { selected_style } else { style },
                Baseline::Top,
            )
            .draw(win);
        }
    }

    fn draw_details(&self, win: &mut crate::wm::Window) {
        let width = self.content_width();
        let header = Rect::new(self.rect.x, self.rect.y, width as u32, DETAILS_HEADER_H as u32);
        fill_rect(win, header, HEADER_BG);
        stroke_rect(win, header, HEADER_BORDER);

        let name_w = (width * 55 / 100).max(110);
        let size_w = (width * 20 / 100).max(70);
        let kind_x = self.rect.x + name_w + size_w;
        let header_style = MonoTextStyle::new(font(), VIEW_MUTED);
        draw_text(win, "Name", self.rect.x + 6, self.rect.y + 4, header_style);
        draw_text(win, "Size", self.rect.x + name_w + 6, self.rect.y + 4, header_style);
        draw_text(win, "Type", kind_x + 6, self.rect.y + 4, header_style);

        let style = MonoTextStyle::new(font(), VIEW_TEXT);
        let selected_style = MonoTextStyle::new(font(), VIEW_SELECTED_TEXT);

        for (i, item) in self.items.iter().enumerate() {
            let Some(row) = self.item_rect(i) else { continue; };
            if !rect_visible(row, self.rect) || row.y < self.rect.y + DETAILS_HEADER_H {
                continue;
            }
            let selected = self.selected == Some(i);
            let hovered = self.hovered == Some(i);
            if selected || hovered {
                fill_rect(win, row, if selected { VIEW_SELECTED } else { VIEW_HOVER });
            }

            if let Some(icon) = self.icons.get(item.kind) {
                self.draw_icon_at(win, icon, row.x + 3, row.y + 3, 22, 22);
            }

            let text_style = if selected { selected_style } else { style };
            let max_name_chars = ((name_w - 34) / FONT_W).max(1);
            let name = truncate_text(&item.name, max_name_chars);
            draw_text(win, name, row.x + 30, row.y + 5, text_style);

            if item.kind != FileKind::Directory {
                let size = format_size(item.size);
                draw_text(win, &size, row.x + name_w + 6, row.y + 5, text_style);
            }
            draw_text(win, item.kind.label(), kind_x + 6, row.y + 5, text_style);
        }
    }

    fn draw_scrollbar(&self, win: &mut crate::wm::Window) {
        let Some(bar) = self.scrollbar_rect() else { return; };
        fill_rect(win, bar, SCROLL_BG);
        if let Some(thumb) = self.scrollbar_thumb() {
            fill_rect(win, thumb, SCROLL_THUMB);
        }
    }
}

impl Default for FileView {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for FileView {
    fn measure(&self, constraints: Constraints) -> TSize<f32> {
        constraints.clamp(TSize {
            width: 320.0,
            height: 220.0,
        })
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
        fill_rect(win, self.rect, VIEW_BG);
        match self.mode {
            FileViewMode::Icons => self.draw_icons(win),
            FileViewMode::List => self.draw_list(win),
            FileViewMode::Details => self.draw_details(win),
        }
        self.draw_scrollbar(win);
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

                let hit = self.item_at(x, y);
                self.pressed = hit;
                if self.selected != hit {
                    self.selected = hit;
                    self.dirty = true;
                    EventResult::Changed
                } else {
                    EventResult::Consumed
                }
            }
            UiEvent::Move { x, y } => {
                if self.dragging_scrollbar {
                    self.scroll_from_thumb_top(y - self.scrollbar_grab_y);
                    return EventResult::Changed;
                }

                let hover = self.item_at(x, y);
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
                let hit = self.item_at(x, y);
                if let (Some(a), Some(b)) = (pressed, hit) {
                    if a == b {
                        let now = monotonic_ms();
                        let double = now != 0
                            && self.last_click_ms != 0
                            && self.last_click_index == Some(a)
                            && now >= self.last_click_ms
                            && now.saturating_sub(self.last_click_ms) <= DOUBLE_CLICK_MS;
                        if double {
                            self.last_click_index = None;
                            self.last_click_ms = 0;
                            self.activated = Some(a);
                            self.dirty = true;
                            return EventResult::Clicked;
                        }
                        self.last_click_index = Some(a);
                        self.last_click_ms = now;
                        return EventResult::Consumed;
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

fn monotonic_ms() -> u64 {
    let mut ts = TimeSpec::default();
    let ok = unsafe { clock_gettime(CLOCK_MONOTONIC, &mut ts) };
    if ok != 0 {
        return 0;
    }
    let sec = ts.tv_sec.max(0) as u64;
    let nsec = ts.tv_nsec.max(0) as u64;
    sec.saturating_mul(1000).saturating_add(nsec / 1_000_000)
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
    let r5 = ((raw >> 11) & 0x1F) as u8;
    let g6 = ((raw >> 5) & 0x3F) as u8;
    let b5 = (raw & 0x1F) as u8;
    Rgb888::new(
        (r5 << 3) | (r5 >> 2),
        (g6 << 2) | (g6 >> 4),
        (b5 << 3) | (b5 >> 2),
    )
}

fn truncate_text(text: &str, max_chars: i32) -> &str {
    let max_chars = max_chars.max(1) as usize;
    match text.char_indices().nth(max_chars) {
        Some((byte, _)) => &text[..byte],
        None => text,
    }
}

fn file_kind_sort_key(kind: FileKind) -> u8 {
    match kind {
        FileKind::Mount => 0,
        FileKind::Directory => 1,
        FileKind::Executable => 2,
        FileKind::File => 3,
        FileKind::Device => 4,
        FileKind::Unknown => 5,
    }
}

fn normalized_view_path(path: &str) -> String {
    if path.is_empty() || path == "/" {
        return String::from("/");
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        String::from("/")
    } else {
        String::from(trimmed)
    }
}

fn join_view_path(parent: &str, name: &str) -> String {
    if parent.is_empty() || parent == "/" {
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

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{} KB", (bytes + 1023) / 1024)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{} MB", (bytes + 1024 * 1024 - 1) / (1024 * 1024))
    } else {
        format!("{} GB", (bytes + 1024 * 1024 * 1024 - 1) / (1024 * 1024 * 1024))
    }
}

fn fill_rect(win: &mut crate::wm::Window, rect: Rect, color: Rgb888) {
    if rect.w == 0 || rect.h == 0 {
        return;
    }
    let _ = Rectangle::new(Point::new(rect.x, rect.y), Size::new(rect.w, rect.h))
        .into_styled(PrimitiveStyle::with_fill(color))
        .draw(win);
}

fn stroke_rect(win: &mut crate::wm::Window, rect: Rect, color: Rgb888) {
    if rect.w == 0 || rect.h == 0 {
        return;
    }
    let _ = Rectangle::new(Point::new(rect.x, rect.y), Size::new(rect.w, rect.h))
        .into_styled(PrimitiveStyle::with_stroke(color, 1))
        .draw(win);
}

fn inset_rect(rect: Rect, amount: i32) -> Rect {
    let amount = amount.max(0);
    Rect::new(
        rect.x + amount,
        rect.y + amount,
        rect.w.saturating_sub((amount as u32).saturating_mul(2)),
        rect.h.saturating_sub((amount as u32).saturating_mul(2)),
    )
}

fn rect_visible(a: Rect, b: Rect) -> bool {
    a.intersect(b).is_some()
}

fn draw_text(win: &mut crate::wm::Window, text: &str, x: i32, y: i32, style: MonoTextStyle<'static, Rgb888>) {
    let _ = Text::with_baseline(text, Point::new(x, y), style, Baseline::Top).draw(win);
}
