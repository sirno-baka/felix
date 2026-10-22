use std::{
    cell::RefCell,
    process::Command,
    rc::Rc,
    thread,
    time::{Duration, Instant},
};

use popugos::{focus_window, screen_size, window_list, WindowEvent, WindowFlags};
use popui::{ToolbarButton, Ui, UiEvent, WidgetId, Window, WindowError};
use taffy::prelude::{Dimension, LengthPercentageAuto};

const PANEL_H: u32 = 42;
const MAX_TASK_BUTTONS: usize = 8;
const PANEL_TITLE: &str = "Felix Task Panel";
const START_MENU_TITLE: &str = "Felix Start Menu";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    run()?;
    Ok(())
}

fn run() -> Result<(), WindowError> {
    let (screen_w, screen_h) = screen_size();
    let panel_y = screen_h.saturating_sub(PANEL_H) as i32;
    let mut window = Window::builder()
        .title(PANEL_TITLE)
        .position(0, panel_y)
        .size(screen_w, PANEL_H)
        .flags(WindowFlags::new(WindowFlags::FRAMELESS | WindowFlags::ALWAYS_ON_TOP))
        .build()?;
    let panel_id = window.id();

    let mut ui = Ui::with_size(window.client_width(), window.client_height());
    let root = ui.root();
    ui.style(root, |s| {
        s.flex_direction = popui::FlexDirection::Row;
        s.align_items = Some(popui::AlignItems::CENTER);
        s.gap = taffy::geometry::Size::length(3.0);
        s.padding = taffy::geometry::Rect::length(3.0);
    });

    let start = ui.toolbar_button(root, "Start");
    if let Some(node) = ui.node_of(start) {
        ui.style(node, |s| {
            s.flex_shrink = 0.0;
            s.size.width = Dimension::length(68.0);
        });
    }

    let mut task_buttons = Vec::with_capacity(MAX_TASK_BUTTONS);
    for _ in 0..MAX_TASK_BUTTONS {
        let button = ui.toolbar_button(root, "");
        if let Some(widget) = ui.widget_mut::<ToolbarButton>(button) {
            widget.set_enabled(false);
        }
        if let Some(node) = ui.node_of(button) {
            ui.style(node, |s| {
                s.flex_grow = 1.0;
                s.flex_shrink = 1.0;
                s.flex_basis = Dimension::length(0.0);
                s.min_size.width = LengthPercentageAuto::length(0.0);
                s.max_size.width = LengthPercentageAuto::length(190.0);
            });
        }
        task_buttons.push(button);
    }

    let slots = Rc::new(RefCell::new([None::<u32>; MAX_TASK_BUTTONS]));
    for (index, &button) in task_buttons.iter().enumerate() {
        let slots = slots.clone();
        ui.on_click(button, move |_| {
            if let Some(id) = slots.borrow()[index] {
                let _ = focus_window(id);
            }
        });
    }

    ui.on_click(start, move |_| {
        if let Some(existing) = window_list()
            .into_iter()
            .find(|w| w.visible && w.title == START_MENU_TITLE)
        {
            let _ = focus_window(existing.id);
            return;
        }
        let _ = Command::new("/bin/startmenu").spawn();
    });

    let buttons: [WidgetId; MAX_TASK_BUTTONS] = task_buttons.try_into().unwrap();
    refresh_tasks(&mut ui, &buttons, &slots, panel_id);
    let _ = ui.frame(&mut window)?;

    let mut last_refresh = Instant::now();
    loop {
        while let Some(event) = window.poll_event() {
            if matches!(event, WindowEvent::Close) {
                return Ok(());
            }
            if let Some(event) = UiEvent::from_window(event) {
                ui.dispatch(&event);
            }
        }

        if last_refresh.elapsed() >= Duration::from_millis(250) {
            refresh_tasks(&mut ui, &buttons, &slots, panel_id);
            last_refresh = Instant::now();
        }

        let _ = ui.frame(&mut window)?;
        thread::sleep(Duration::from_millis(8));
    }
}

fn refresh_tasks(
    ui: &mut Ui,
    buttons: &[WidgetId; MAX_TASK_BUTTONS],
    slots: &Rc<RefCell<[Option<u32>; MAX_TASK_BUTTONS]>>,
    panel_id: u32,
) {
    let windows: Vec<_> = window_list()
        .into_iter()
        .filter(|w| {
            w.visible
                && w.id != panel_id
                && w.title != PANEL_TITLE
                && w.title != START_MENU_TITLE
        })
        .take(MAX_TASK_BUTTONS)
        .collect();

    let mut slot_ids = slots.borrow_mut();
    for index in 0..MAX_TASK_BUTTONS {
        let item = windows.get(index);
        slot_ids[index] = item.map(|w| w.id);
        let Some(button) = ui.widget_mut::<ToolbarButton>(buttons[index]) else {
            continue;
        };
        match item {
            Some(window) => {
                let title = if window.title.trim().is_empty() {
                    format!("Window {}", window.id)
                } else {
                    shorten(&window.title, 18)
                };
                let label = if window.focused {
                    format!("> {title}")
                } else {
                    title
                };
                button.set_label(&label);
                button.set_enabled(true);
            }
            None => {
                button.set_label("");
                button.set_enabled(false);
            }
        }
    }
}

fn shorten(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(3)).collect();
    out.push_str("...");
    out
}
