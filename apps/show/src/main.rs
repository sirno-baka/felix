#![no_std]
#![no_main]

extern crate alloc;

use libfelix::prelude::*;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let mut win = Window::create(1, 1, 900, 600, "Felix UI 2.0").unwrap();
    let mut ui = Ui::with_size(900, 600);
    let root = ui.root();

    ui.style(root, |s| {
        s.flex_direction = FlexDirection::Column;
        s.padding = taffy::geometry::Rect::from_length(12.0);
        s.gap = taffy::geometry::Size::from_length(8.0);
    });

    let header = ui.row(root);
    ui.style(header, |s| {
        s.min_size.height = taffy::prelude::LengthPercentageAuto::length(48.0);
        s.align_items = Some(AlignItems::CENTER);
        s.gap = taffy::geometry::Size::from_length(8.0);
    });
    ui.label(header, "Felix UI 2.0");
    ui.spacer(header);
    let refresh = ui.button(header, "Refresh");

    let body = ui.row(root);
    ui.style(body, |s| {
        s.flex_grow = 1.0;
        s.min_size.width = taffy::prelude::LengthPercentageAuto::length(0.0);
        s.min_size.height = taffy::prelude::LengthPercentageAuto::length(0.0);
        s.gap = taffy::geometry::Size::from_length(8.0);
    });

    let sidebar = ui.panel(body);
    ui.style(sidebar, |s| {
        s.size.width = taffy::prelude::Dimension::length(190.0);
        s.gap = taffy::geometry::Size::from_length(6.0);
    });
    ui.label(sidebar, "Navigation");
    let home = ui.button(sidebar, "Home");
    let files = ui.button(sidebar, "Files");
    let settings = ui.button(sidebar, "Settings");
    ui.spacer(sidebar);
    ui.label(sidebar, "PageUp/PageDown scroll");

    let main = ui.column(body);
    ui.style(main, |s| {
        s.flex_grow = 1.0;
        s.min_size.width = taffy::prelude::LengthPercentageAuto::length(0.0);
        s.min_size.height = taffy::prelude::LengthPercentageAuto::length(0.0);
        s.gap = taffy::geometry::Size::from_length(8.0);
    });

    let content_header = ui.row(main);
    ui.style(content_header, |s| {
        s.min_size.height = taffy::prelude::LengthPercentageAuto::length(38.0);
        s.align_items = Some(AlignItems::CENTER);
    });
    ui.label(content_header, "Dashboard");
    ui.spacer(content_header);
    let clear = ui.button(content_header, "Top");

    let info = ui.panel(main);
    ui.style(info, |s| {
        s.min_size.height = taffy::prelude::LengthPercentageAuto::length(72.0);
        s.gap = taffy::geometry::Size::from_length(5.0);
    });
    ui.label(info, "Layout and widgets are separate.");
    ui.label(info, "Every leaf implements the common Widget trait.");
    ui.label(info, "Intrinsic measurement receives explicit Constraints.");

    let scroll = ui.scroll_view(main);
    let scroll_content = ui.scroll_content(scroll).unwrap();
    ui.style(scroll_content, |s| {
        s.gap = taffy::geometry::Size::from_length(4.0);
        s.padding = taffy::geometry::Rect::from_length(4.0);
    });
    for label in [
        "01  Scrollable row", "02  Scrollable row", "03  Scrollable row", "04  Scrollable row",
        "05  Scrollable row", "06  Scrollable row", "07  Scrollable row", "08  Scrollable row",
        "09  Scrollable row", "10  Scrollable row", "11  Scrollable row", "12  Scrollable row",
        "13  Scrollable row", "14  Scrollable row", "15  Scrollable row", "16  Scrollable row",
        "17  Scrollable row", "18  Scrollable row", "19  Scrollable row", "20  Scrollable row",
        "21  Scrollable row", "22  Scrollable row", "23  Scrollable row", "24  Scrollable row",
    ] {
        let row = ui.row(scroll_content);
        ui.style(row, |s| {
            s.min_size.height = taffy::prelude::LengthPercentageAuto::length(30.0);
            s.align_items = Some(AlignItems::CENTER);
            s.gap = taffy::geometry::Size::from_length(8.0);
        });
        ui.label(row, label);
        ui.spacer(row);
    }

    let input_row = ui.row(main);
    ui.style(input_row, |s| {
        s.min_size.height = taffy::prelude::LengthPercentageAuto::length(38.0);
        s.align_items = Some(AlignItems::CENTER);
        s.gap = taffy::geometry::Size::from_length(8.0);
    });
    ui.label(input_row, "Name:");
    let input = ui.text_input_with(input_row, "Felix");
    ui.style(ui.node_of(input).unwrap(), |s| {
        s.flex_grow = 1.0;
        s.min_size.width = taffy::prelude::LengthPercentageAuto::length(100.0);
    });
    let apply = ui.button(input_row, "Apply");

    let footer = ui.row(root);
    ui.style(footer, |s| {
        s.min_size.height = taffy::prelude::LengthPercentageAuto::length(32.0);
        s.align_items = Some(AlignItems::CENTER);
    });
    ui.label(footer, "Ready");
    ui.spacer(footer);
    ui.label(footer, "Taffy + Widget + Constraints + ScrollView");

    ui.on_click(home, |_ui| println!("Home clicked"));
    ui.on_click(files, |_ui| println!("Files clicked"));
    ui.on_click(settings, |_ui| println!("Settings clicked"));
    ui.on_click(refresh, |_ui| println!("Refresh clicked"));
    ui.on_click(clear, move |ui| { ui.scroll_to_top(scroll); });
    ui.on_click(apply, move |ui| {
        if let Some(text) = ui.text(input) { println!("Name: {}", text); }
    });

    loop { ui.process(&mut win); }
}
