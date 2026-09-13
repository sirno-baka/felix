#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use libfelix::prelude::*;
use taffy::prelude::FromLength;

const WINDOW_W: u32 = 600;
const WINDOW_H: u32 = 420;
const UNTITLED_PATH: &str = "/untitled.txt";

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = Args::parse();
    let initial_path = args.get(0).unwrap_or(UNTITLED_PATH);

    let mut win = Window::create(12, 12, WINDOW_W, WINDOW_H, "Felix Notepad").unwrap();
    let mut ui = Ui::with_size(WINDOW_W, WINDOW_H);
    let root = ui.root();

    ui.style(root, |s| {
        s.flex_direction = FlexDirection::Column;
    });

    // ---------------------------------------------------------------
    // Toolbar
    // ---------------------------------------------------------------

    let toolbar = ui.toolbar(root);
    let new_file = ui.toolbar_button(toolbar, "New");
    let open_file = ui.toolbar_button(toolbar, "Open");
    let save_file = ui.toolbar_button(toolbar, "Save");

    // ---------------------------------------------------------------
    // Path row. Editing this field effectively provides "Save As".
    // ---------------------------------------------------------------

    let path_row = ui.row(root);
    ui.style(path_row, |s| {
        s.min_size.height = taffy::prelude::LengthPercentageAuto::length(34.0);
        s.flex_shrink = 0.0;
        s.align_items = Some(AlignItems::CENTER);
        s.gap = taffy::geometry::Size::from_length(5.0);
        s.padding = taffy::geometry::Rect::length(3.0);
    });

    ui.label(path_row, "File:");
    let path_input = ui.text_input_with(path_row, initial_path);
    if let Some(input) = ui.text_input_mut(path_input) {
        input.set_max_len(255);
    }
    ui.style(ui.node_of(path_input).unwrap(), |s| {
        s.flex_grow = 1.0;
        s.flex_shrink = 1.0;
        s.min_size.width = taffy::prelude::LengthPercentageAuto::length(80.0);
    });

    let open_from_path = ui.toolbar_button(path_row, "Open");

    // ---------------------------------------------------------------
    // Main editor
    // ---------------------------------------------------------------

    let editor = ui.text_area(root);
    ui.style(ui.node_of(editor).unwrap(), |s| {
        s.flex_grow = 1.0;
        s.flex_shrink = 1.0;
        s.min_size.width = taffy::prelude::LengthPercentageAuto::length(0.0);
        s.min_size.height = taffy::prelude::LengthPercentageAuto::length(0.0);
    });
    if let Some(area) = ui.text_area_mut(editor) {
        area.set_max_len(512 * 1024);
    }

    // ---------------------------------------------------------------
    // Status bar
    // ---------------------------------------------------------------

    let status_row = ui.row(root);
    ui.style(status_row, |s| {
        s.min_size.height = taffy::prelude::LengthPercentageAuto::length(25.0);
        s.flex_shrink = 0.0;
        s.align_items = Some(AlignItems::CENTER);
        s.padding = taffy::geometry::Rect::length(3.0);
    });
    let status = ui.label(status_row, "Ready");

    // Open argv[1] when supplied. Without an argument we start with a blank
    // /untitled.txt document.
    if args.get(0).is_some() {
        match fs::read_to_string(initial_path) {
            Ok(text) => {
                if let Some(area) = ui.text_area_mut(editor) {
                    area.set_owned_text(text);
                }
                ui.set_label(status, "Opened");
            }
            Err(_) => {
                ui.set_label(status, "File not found - new document");
            }
        }
    }

    ui.set_focus(Some(editor));

    // ---------------------------------------------------------------
    // New
    // ---------------------------------------------------------------

    ui.on_click(new_file, move |ui| {
        if let Some(area) = ui.text_area_mut(editor) {
            area.clear();
        }
        ui.set_text(path_input, UNTITLED_PATH);
        ui.set_label(status, "New document");
        ui.set_focus(Some(editor));
    });

    // ---------------------------------------------------------------
    // Open. Both toolbar Open and the button beside the path use the same
    // helper. The path itself stays in the single-line TextInput.
    // ---------------------------------------------------------------

    ui.on_click(open_file, move |ui| {
        open_current_path(ui, path_input, editor, status);
    });

    ui.on_click(open_from_path, move |ui| {
        open_current_path(ui, path_input, editor, status);
    });

    // Enter in the path field opens the typed file.
    ui.on_click(path_input, move |ui| {
        open_current_path(ui, path_input, editor, status);
    });

    // ---------------------------------------------------------------
    // Save. To do "Save As", type another path and press Save.
    // ---------------------------------------------------------------

    ui.on_click(save_file, move |ui| {
        let result = {
            let path = ui.text(path_input).unwrap_or("");
            if path.is_empty() {
                false
            } else if let Some(area) = ui.text_area_ref(editor) {
                fs::write(path, area.text().as_bytes()).is_ok()
            } else {
                false
            }
        };

        if result {
            if let Some(area) = ui.text_area_mut(editor) {
                area.mark_saved();
            }
            ui.set_label(status, "Saved");
        } else {
            ui.set_label(status, "Save failed");
        }
        ui.set_focus(Some(editor));
    });

    loop {
        ui.process(&mut win);
    }
}

fn open_current_path(
    ui: &mut Ui,
    path_input: WidgetId,
    editor: WidgetId,
    status: WidgetId,
) {
    let path: Option<String> = ui.text(path_input).map(String::from);
    let Some(path) = path else {
        ui.set_label(status, "No file path");
        return;
    };

    if path.is_empty() {
        ui.set_label(status, "No file path");
        return;
    }

    match fs::read_to_string(&path) {
        Ok(text) => {
            if let Some(area) = ui.text_area_mut(editor) {
                area.set_owned_text(text);
            }
            ui.set_label(status, "Opened");
            ui.set_focus(Some(editor));
        }
        Err(_) => {
            ui.set_label(status, "Open failed");
        }
    }
}
