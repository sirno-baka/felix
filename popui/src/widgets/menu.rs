use std::{string::String, vec::Vec};

use popugos::window::Window;

use crate::{draw, Constraints, EventResult, Point, Rect, Size, Theme, UiEvent, Widget};

const BAR_H: i32 = 26;
const ITEM_H: i32 = 24;
const SEP_H: i32 = 7;
const FONT_W: i32 = 9;
const FONT_H: i32 = 18;
const PAD_X: i32 = 10;
const POPUP_MIN_W: i32 = 140;
const POPUP_PAD_X: i32 = 10;
const SCAN_ESC: u8 = 0x01;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MenuId(pub u32);

#[derive(Clone, Debug)]
pub struct MenuEntry {
    pub id: Option<MenuId>,
    pub label: String,
    pub enabled: bool,
    pub separator: bool,
    pub children: Vec<MenuEntry>,
}

impl MenuEntry {
    pub fn item(id: u32, label: impl Into<String>) -> Self {
        Self {
            id: Some(MenuId(id)),
            label: label.into(),
            enabled: true,
            separator: false,
            children: Vec::new(),
        }
    }

    pub fn submenu(label: impl Into<String>, children: Vec<MenuEntry>) -> Self {
        Self {
            id: None,
            label: label.into(),
            enabled: true,
            separator: false,
            children,
        }
    }

    pub fn separator() -> Self {
        Self {
            id: None,
            label: String::new(),
            enabled: false,
            separator: true,
            children: Vec::new(),
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.enabled = !disabled;
        self
    }
}

#[derive(Clone, Debug)]
struct ContextPopup {
    x: i32,
    y: i32,
    entries: Vec<MenuEntry>,
}

/// Classic desktop menu bar with dropdowns and a reusable context popup.
///
/// Top-level entries are normally submenus (`File`, `Edit`, `View`, ...).
/// The same widget can display a context menu at an arbitrary client position.
pub struct Menu {
    entries: Vec<MenuEntry>,
    rect: Rect,
    open_top: Option<usize>,
    context: Option<ContextPopup>,
    hot_top: Option<usize>,
    hot_item: Option<usize>,
    action: Option<MenuId>,
    dirty: bool,
}

impl Menu {
    pub fn new(entries: Vec<MenuEntry>) -> Self {
        Self {
            entries,
            rect: Rect::default(),
            open_top: None,
            context: None,
            hot_top: None,
            hot_item: None,
            action: None,
            dirty: true,
        }
    }

    pub fn entries(&self) -> &[MenuEntry] { &self.entries }

    pub fn set_entries(&mut self, entries: Vec<MenuEntry>) {
        self.entries = entries;
        self.dismiss();
        self.dirty = true;
    }

    pub fn show_context(&mut self, x: i32, y: i32, entries: Vec<MenuEntry>) {
        self.open_top = None;
        self.context = Some(ContextPopup { x, y, entries });
        self.hot_item = None;
        self.dirty = true;
    }

    pub fn take_action(&mut self) -> Option<MenuId> {
        self.action.take()
    }

    pub fn is_open(&self) -> bool {
        self.open_top.is_some() || self.context.is_some()
    }

    pub fn dismiss(&mut self) {
        if self.is_open() || self.hot_top.is_some() || self.hot_item.is_some() {
            self.open_top = None;
            self.context = None;
            self.hot_top = None;
            self.hot_item = None;
            self.dirty = true;
        }
    }

    fn heading_width(entry: &MenuEntry) -> i32 {
        entry.label.chars().count() as i32 * FONT_W + PAD_X * 2
    }

    fn heading_rect(&self, index: usize) -> Option<Rect> {
        if index >= self.entries.len() { return None; }
        let mut x = self.rect.x;
        for entry in self.entries.iter().take(index) {
            x += Self::heading_width(entry);
        }
        Some(Rect::new(x, self.rect.y, Self::heading_width(&self.entries[index]).max(1) as u32, self.rect.h))
    }

    fn heading_at(&self, x: i32, y: i32) -> Option<usize> {
        if !self.rect.contains(x, y) { return None; }
        for index in 0..self.entries.len() {
            if self.heading_rect(index).map(|r| r.contains(x, y)).unwrap_or(false) {
                return Some(index);
            }
        }
        None
    }

    fn active_entries(&self) -> Option<&[MenuEntry]> {
        if let Some(context) = self.context.as_ref() {
            return Some(context.entries.as_slice());
        }
        let index = self.open_top?;
        Some(self.entries.get(index)?.children.as_slice())
    }

    fn popup_origin(&self) -> Option<(i32, i32)> {
        if let Some(context) = self.context.as_ref() {
            return Some((context.x, context.y));
        }
        let index = self.open_top?;
        let heading = self.heading_rect(index)?;
        Some((heading.x, heading.y + heading.h as i32))
    }

    fn popup_width(entries: &[MenuEntry]) -> i32 {
        let text = entries
            .iter()
            .filter(|entry| !entry.separator)
            .map(|entry| entry.label.chars().count() as i32 * FONT_W + POPUP_PAD_X * 2 + 16)
            .max()
            .unwrap_or(POPUP_MIN_W);
        text.max(POPUP_MIN_W)
    }

    fn popup_height(entries: &[MenuEntry]) -> i32 {
        entries.iter().map(|entry| if entry.separator { SEP_H } else { ITEM_H }).sum::<i32>().max(1)
    }

    fn popup_rect(&self) -> Option<Rect> {
        let entries = self.active_entries()?;
        let (x, y) = self.popup_origin()?;
        Some(Rect::new(
            x,
            y,
            Self::popup_width(entries) as u32,
            Self::popup_height(entries) as u32,
        ))
    }

    fn item_rect(&self, wanted: usize) -> Option<Rect> {
        let entries = self.active_entries()?;
        let popup = self.popup_rect()?;
        let mut y = popup.y;
        for (index, entry) in entries.iter().enumerate() {
            let h = if entry.separator { SEP_H } else { ITEM_H };
            if index == wanted {
                return Some(Rect::new(popup.x, y, popup.w, h as u32));
            }
            y += h;
        }
        None
    }

    fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        let entries = self.active_entries()?;
        for index in 0..entries.len() {
            if self.item_rect(index).map(|r| r.contains(x, y)).unwrap_or(false) {
                return Some(index);
            }
        }
        None
    }

    fn activate_item(&mut self, index: usize) -> EventResult {
        let Some(entries) = self.active_entries() else { return EventResult::Ignored; };
        let Some(entry) = entries.get(index) else { return EventResult::Ignored; };
        if entry.separator || !entry.enabled {
            return EventResult::Consumed;
        }
        let id = entry.id;
        if let Some(id) = id {
            self.action = Some(id);
            self.open_top = None;
            self.context = None;
            self.hot_item = None;
            self.dirty = true;
            EventResult::Clicked
        } else {
            EventResult::Consumed
        }
    }

    fn draw_popup(&self, window: &mut Window, theme: &Theme) {
        let Some(entries) = self.active_entries() else { return; };
        let Some(popup) = self.popup_rect() else { return; };

        draw::fill_rect(window, popup, theme.panel_background);
        draw::stroke_rect(window, popup, theme.panel_border, 1);

        for (index, entry) in entries.iter().enumerate() {
            let Some(row) = self.item_rect(index) else { continue; };
            if entry.separator {
                let y = row.y + row.h as i32 / 2;
                draw::line(
                    window,
                    Point::new(row.x + 5, y),
                    Point::new(row.x + row.w as i32 - 6, y),
                    theme.panel_border,
                    1,
                );
                continue;
            }

            if self.hot_item == Some(index) && entry.enabled {
                draw::fill_rect(window, row, theme.button_hot);
            }
            let color = if entry.enabled { theme.text } else { theme.label };
            let y = row.y + (row.h as i32 - FONT_H) / 2;
            draw::text(window, Point::new(row.x + POPUP_PAD_X, y), &entry.label, color);
        }
    }
}

impl Widget for Menu {
    fn measure(&self, constraints: Constraints) -> Size {
        let width = self.entries.iter().map(Self::heading_width).sum::<i32>().max(1) as f32;
        constraints.clamp(Size::new(width, BAR_H as f32))
    }

    fn set_rect(&mut self, rect: Rect) {
        if self.rect != rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    fn rect(&self) -> Rect { self.rect }

    fn draw(&self, window: &mut Window, theme: &Theme) {
        if self.rect.w == 0 || self.rect.h == 0 { return; }
        draw::fill_rect(window, self.rect, theme.panel_background);
        draw::line(
            window,
            Point::new(self.rect.x, self.rect.y + self.rect.h as i32 - 1),
            Point::new(self.rect.x + self.rect.w as i32 - 1, self.rect.y + self.rect.h as i32 - 1),
            theme.panel_border,
            1,
        );

        for (index, entry) in self.entries.iter().enumerate() {
            let Some(rect) = self.heading_rect(index) else { continue; };
            if self.open_top == Some(index) || self.hot_top == Some(index) {
                draw::fill_rect(window, rect, theme.button_hot);
            }
            let y = rect.y + (rect.h as i32 - FONT_H) / 2;
            draw::text(window, Point::new(rect.x + PAD_X, y), &entry.label, theme.text);
        }
    }

    fn event(&mut self, event: &UiEvent, _focused: bool) -> EventResult {
        match *event {
            UiEvent::Down { x, y } => {
                if let Some(index) = self.heading_at(x, y) {
                    self.context = None;
                    self.hot_item = None;
                    if self.entries[index].children.is_empty() {
                        if let Some(id) = self.entries[index].id {
                            if self.entries[index].enabled {
                                self.action = Some(id);
                                self.dirty = true;
                                return EventResult::Clicked;
                            }
                        }
                    } else {
                        self.open_top = if self.open_top == Some(index) { None } else { Some(index) };
                        self.hot_top = Some(index);
                        self.dirty = true;
                    }
                    return EventResult::Consumed;
                }

                if self.overlay_contains(x, y) {
                    if let Some(index) = self.item_at(x, y) {
                        return self.activate_item(index);
                    }
                    return EventResult::Consumed;
                }
                EventResult::Ignored
            }
            UiEvent::Move { x, y } => {
                let hot_top = self.heading_at(x, y);
                let hot_item = if self.overlay_contains(x, y) { self.item_at(x, y) } else { None };
                let switch_top = self.open_top.is_some()
                    && hot_top.is_some()
                    && hot_top != self.open_top
                    && hot_top
                        .and_then(|index| self.entries.get(index))
                        .map(|entry| !entry.children.is_empty())
                        .unwrap_or(false);
                if switch_top {
                    self.open_top = hot_top;
                }
                if hot_top != self.hot_top || hot_item != self.hot_item || switch_top {
                    self.hot_top = hot_top;
                    self.hot_item = hot_item;
                    self.dirty = true;
                    EventResult::Changed
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::Up { x, y } => {
                if self.rect.contains(x, y) || self.overlay_contains(x, y) {
                    EventResult::Consumed
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::Context { x, y } => {
                if self.rect.contains(x, y) || self.overlay_contains(x, y) {
                    EventResult::Consumed
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::KeyDown { scancode, .. } if scancode == SCAN_ESC && self.is_open() => {
                self.dismiss();
                EventResult::Consumed
            }
            UiEvent::Leave => {
                if self.hot_top.take().is_some() || self.hot_item.take().is_some() {
                    self.dirty = true;
                    EventResult::Changed
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::Wheel { .. }
            | UiEvent::KeyDown { .. }
            | UiEvent::KeyUp { .. }
            | UiEvent::Resize { .. } => EventResult::Ignored,
        }
    }

    fn dirty(&self) -> bool { self.dirty }
    fn clear_dirty(&mut self) { self.dirty = false; }
    fn overlay_contains(&self, x: i32, y: i32) -> bool {
        self.popup_rect().map(|rect| rect.contains(x, y)).unwrap_or(false)
    }
    fn overlay_active(&self) -> bool { self.is_open() }
    fn draw_overlay(&self, window: &mut Window, theme: &Theme) { self.draw_popup(window, theme); }
    fn dismiss_overlay(&mut self) { self.dismiss(); }
    fn full_redraw_when_dirty(&self) -> bool { true }
    fn as_any(&self) -> &dyn core::any::Any { self }
    fn as_any_mut(&mut self) -> &mut dyn core::any::Any { self }
}
