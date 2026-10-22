use std::{
    cell::Cell,
    process::Command,
    rc::Rc,
    thread,
    time::Duration,
};

use popugos::{poweroff, reboot, screen_size, WindowEvent, WindowFlags};
use popui::{Ui, UiEvent, Window, WindowError};
use taffy::prelude::{Dimension, LengthPercentageAuto};

const PANEL_H: u32 = 42;
const MENU_W: u32 = 230;
const MENU_H: u32 = 250;
const MENU_TITLE: &str = "Felix Start Menu";

#[derive(Clone, Copy, Debug)]
enum Action {
    FileManager,
    Terminal,
    RhaiIde,
    Reboot,
    Shutdown,
    Close,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    run()?;
    Ok(())
}

fn run() -> Result<(), WindowError> {
    let (_, screen_h) = screen_size();
    let y = screen_h.saturating_sub(PANEL_H + MENU_H) as i32;
    let mut window = Window::builder()
        .title(MENU_TITLE)
        .position(0, y)
        .size(MENU_W, MENU_H)
        .flags(WindowFlags::new(WindowFlags::FRAMELESS | WindowFlags::ALWAYS_ON_TOP))
        .build()?;

    let mut ui = Ui::with_size(window.client_width(), window.client_height());
    let root = ui.root();
    ui.style(root, |s| {
        s.flex_direction = popui::FlexDirection::Column;
        s.gap = taffy::geometry::Size::length(4.0);
        s.padding = taffy::geometry::Rect::length(7.0);
    });

    let title = ui.label(root, "Felix");
    if let Some(node) = ui.node_of(title) {
        ui.style(node, |s| {
            s.size.height = Dimension::length(24.0);
            s.flex_shrink = 0.0;
        });
    }

    let filemanager = menu_button(&mut ui, root, "File Manager");
    let terminal = menu_button(&mut ui, root, "Terminal");
    let rhai_ide = menu_button(&mut ui, root, "Rhai IDE");
    ui.spacer(root);
    let reboot_button = menu_button(&mut ui, root, "Reboot");
    let shutdown_button = menu_button(&mut ui, root, "Shutdown");
    let close_button = menu_button(&mut ui, root, "Close");

    let pending = Rc::new(Cell::new(None::<Action>));
    for (button, action) in [
        (filemanager, Action::FileManager),
        (terminal, Action::Terminal),
        (rhai_ide, Action::RhaiIde),
        (reboot_button, Action::Reboot),
        (shutdown_button, Action::Shutdown),
        (close_button, Action::Close),
    ] {
        let pending = pending.clone();
        ui.on_click(button, move |_| pending.set(Some(action)));
    }

    let _ = ui.frame(&mut window)?;
    loop {
        while let Some(event) = window.poll_event() {
            match event {
                WindowEvent::Close | WindowEvent::Focused(false) => return Ok(()),
                _ => {}
            }
            if let Some(event) = UiEvent::from_window(event) {
                ui.dispatch(&event);
            }
        }

        if let Some(action) = pending.take() {
            match action {
                Action::FileManager => { let _ = Command::new("/bin/filemanager").spawn(); return Ok(()); }
                Action::Terminal => { let _ = Command::new("/bin/terminal").spawn(); return Ok(()); }
                Action::RhaiIde => { let _ = Command::new("/bin/rhai-ide").spawn(); return Ok(()); }
                Action::Reboot => reboot(),
                Action::Shutdown => poweroff(),
                Action::Close => return Ok(()),
            }
        }

        let _ = ui.frame(&mut window)?;
        thread::sleep(Duration::from_millis(8));
    }
}

fn menu_button(ui: &mut Ui, parent: popui::NodeId, label: &str) -> popui::WidgetId {
    let id = ui.button(parent, label);
    if let Some(node) = ui.node_of(id) {
        ui.style(node, |s| {
            s.size.width = Dimension::percent(1.0);
            s.min_size.height = LengthPercentageAuto::length(30.0);
            s.flex_shrink = 0.0;
        });
    }
    id
}
