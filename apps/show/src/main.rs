#![no_std]
#![no_main]

extern crate alloc;

use taffy::prelude::FromLength;
use libfelix::prelude::*;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let mut win = Window::create(1, 1, 400, 400, "Felix UI 2.0 Showcase").unwrap();
    let mut ui = Ui::with_size(400, 400);

    let root = ui.root();

    // ---------------------------------------------------------------------
    // Root layout
    // ---------------------------------------------------------------------

    ui.style(root, |s| {
        s.flex_direction = FlexDirection::Column;
        s.padding = taffy::geometry::Rect::length(12.0);
        s.gap = taffy::geometry::Size::from_length(8.0);
    });

    // ---------------------------------------------------------------------
    // Header
    // ---------------------------------------------------------------------

    let header = ui.row(root);

    ui.style(header, |s| {
        s.min_size.height = taffy::prelude::LengthPercentageAuto::length(48.0);
        s.align_items = Some(AlignItems::CENTER);
        s.gap = taffy::geometry::Size::from_length(8.0);
    });

    ui.label(header, "Felix UI 2.0");

    let version = ui.label(header, "new retained-mode API");

    ui.spacer(header);

    let refresh = ui.button(header, "Refresh");

    // ---------------------------------------------------------------------
    // Main horizontal split
    // ---------------------------------------------------------------------

    let body = ui.row(root);

    ui.style(body, |s| {
        s.flex_grow = 1.0;
        s.min_size.width =
            taffy::prelude::LengthPercentageAuto::length(0.0);
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(0.0);
        s.gap = taffy::geometry::Size::from_length(8.0);
    });

    // ---------------------------------------------------------------------
    // Sidebar / Panel
    // ---------------------------------------------------------------------

    let sidebar = ui.panel(body);

    ui.style(sidebar, |s| {
        s.size.width = taffy::prelude::Dimension::length(190.0);
        s.gap = taffy::geometry::Size::from_length(6.0);
    });

    ui.label(sidebar, "Navigation");

    let home = ui.button(sidebar, "Home");
    let components = ui.button(sidebar, "Components");
    let layout = ui.button(sidebar, "Layout");
    let settings = ui.button(sidebar, "Settings");

    ui.spacer(sidebar);

    let sidebar_info = ui.label(
        sidebar,
        "Containers are Taffy nodes.\nWidgets are leaves.",
    );

    // ---------------------------------------------------------------------
    // Main content column
    // ---------------------------------------------------------------------

    let main = ui.column(body);

    ui.style(main, |s| {
        s.flex_grow = 1.0;
        s.min_size.width =
            taffy::prelude::LengthPercentageAuto::length(0.0);
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(0.0);
        s.gap = taffy::geometry::Size::from_length(8.0);
    });

    // ---------------------------------------------------------------------
    // Content header
    // ---------------------------------------------------------------------

    let content_header = ui.row(main);

    ui.style(content_header, |s| {
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(38.0);
        s.align_items = Some(AlignItems::CENTER);
        s.gap = taffy::geometry::Size::from_length(8.0);
    });

    ui.label(content_header, "Widget & Layout Showcase");

    ui.spacer(content_header);

    let scroll_top = ui.button(content_header, "Top");
    let scroll_bottom = ui.button(content_header, "Bottom");

    // ---------------------------------------------------------------------
    // Information panel
    // ---------------------------------------------------------------------

    let info = ui.panel(main);

    ui.style(info, |s| {
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(86.0);
        s.gap = taffy::geometry::Size::from_length(4.0);
    });

    ui.label(info, "Taffy owns layout.");
    ui.label(info, "Widget owns measurement, drawing and events.");
    ui.label(info, "Constraints are passed to intrinsic measurement.");
    ui.label(info, "Ui owns the retained widget tree and event dispatch.");

    // ---------------------------------------------------------------------
    // ScrollView
    // ---------------------------------------------------------------------

    let scroll = ui.scroll_view(main);

    let scroll_content = ui
        .scroll_content(scroll)
        .expect("scroll view must have content");

    ui.style(scroll_content, |s| {
        s.gap = taffy::geometry::Size::from_length(4.0);
        s.padding = taffy::geometry::Rect::length(4.0);
    });

    // First row: normal label.
    let row = ui.row(scroll_content);

    ui.style(row, |s| {
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(32.0);
        s.align_items = Some(AlignItems::CENTER);
    });

    ui.label(row, "01  Label");

    // Second row: button.
    let row = ui.row(scroll_content);

    ui.style(row, |s| {
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(32.0);
        s.align_items = Some(AlignItems::CENTER);
        s.gap = taffy::geometry::Size::from_length(8.0);
    });

    ui.label(row, "02  Button");

    let demo_button = ui.button(row, "Click me");

    // Third row: text input.
    let row = ui.row(scroll_content);

    ui.style(row, |s| {
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(38.0);
        s.align_items = Some(AlignItems::CENTER);
        s.gap = taffy::geometry::Size::from_length(8.0);
    });

    ui.label(row, "03  TextInput");

    let input = ui.text_input_with(row, "Felix");

    ui.style(ui.node_of(input).unwrap(), |s| {
        s.flex_grow = 1.0;
        s.min_size.width =
            taffy::prelude::LengthPercentageAuto::length(100.0);
    });

    // Fourth row: dynamically styled controls.
    let row = ui.row(scroll_content);

    ui.style(row, |s| {
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(32.0);
        s.align_items = Some(AlignItems::CENTER);
        s.gap = taffy::geometry::Size::from_length(8.0);
    });

    ui.label(row, "04  Explicit NodeId styling");

    let dynamic_button = ui.button(row, "Dynamic");

    // ---------------------------------------------------------------------
    // Many rows to demonstrate ScrollView
    // ---------------------------------------------------------------------

    for i in 5..=30 {
        let row = ui.row(scroll_content);

        ui.style(row, |s| {
            s.min_size.height =
                taffy::prelude::LengthPercentageAuto::length(30.0);
            s.align_items = Some(AlignItems::CENTER);
            s.gap = taffy::geometry::Size::from_length(8.0);
        });

        ui.label(row, match i {
            5 => "05  Flex row + Spacer",
            6 => "06  Column container",
            7 => "07  Panel container",
            8 => "08  Widget measurement",
            9 => "09  Event dispatch",
            10 => "10  Focus support",
            11 => "11  Click callbacks",
            12 => "12  Dynamic text",
            13 => "13  Dynamic layout",
            14 => "14  Scroll offset",
            15 => "15  Scroll max offset",
            16 => "16  Scroll to top",
            17 => "17  Scroll to bottom",
            18 => "18  PageUp",
            19 => "19  PageDown",
            20 => "20  Taffy Style",
            21 => "21  Flex direction",
            22 => "22  Flex grow",
            23 => "23  Min/max size",
            24 => "24  Padding",
            25 => "25  Gap",
            26 => "26  Alignment",
            27 => "27  WidgetId",
            28 => "28  NodeId",
            29 => "29  Retained state",
            30 => "30  End of ScrollView",
            _ => "Scrollable row",
        });

        ui.spacer(row);
    }

    // ---------------------------------------------------------------------
    // Bottom input area
    // ---------------------------------------------------------------------

    let input_row = ui.row(main);

    ui.style(input_row, |s| {
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(40.0);
        s.align_items = Some(AlignItems::CENTER);
        s.gap = taffy::geometry::Size::from_length(8.0);
    });

    ui.label(input_row, "Name:");

    let name_input = ui.text_input_with(input_row, "Felix");

    ui.style(ui.node_of(name_input).unwrap(), |s| {
        s.flex_grow = 1.0;
        s.min_size.width =
            taffy::prelude::LengthPercentageAuto::length(100.0);
    });

    let apply = ui.button(input_row, "Apply");

    // ---------------------------------------------------------------------
    // Footer
    // ---------------------------------------------------------------------

    let footer = ui.row(root);

    ui.style(footer, |s| {
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(32.0);
        s.align_items = Some(AlignItems::CENTER);
        s.gap = taffy::geometry::Size::from_length(8.0);
    });

    let status = ui.label(footer, "Ready");

    ui.spacer(footer);

    ui.label(
        footer,
        "Taffy • Widget • Constraints • ScrollView",
    );

    // ---------------------------------------------------------------------
    // Events / callbacks
    // ---------------------------------------------------------------------

    ui.on_click(home, move| ui| {
        ui.set_label(status, "Home selected");
    });

    ui.on_click(components, move|ui| {
        ui.set_label(status, "Components selected");
    });

    ui.on_click(layout, move|ui| {
        ui.set_label(status, "Layout selected");
    });

    ui.on_click(settings, move|ui| {
        ui.set_label(status, "Settings selected");
    });

    ui.on_click(refresh, move|ui| {
        ui.set_label(status, "UI refreshed");
        ui.scroll_to_top(scroll);
    });

    ui.on_click(scroll_top, move|ui| {
        ui.scroll_to_top(scroll);
        ui.set_label(status, "Scrolled to top");
    });

    ui.on_click(scroll_bottom, move|ui| {
        ui.scroll_to_bottom(scroll);
        ui.set_label(status, "Scrolled to bottom");
    });

    ui.on_click(demo_button, move|ui| {
        ui.set_label(status, "Button clicked");
    });

    ui.on_click(dynamic_button, move|ui| {
        ui.set_label(status, "Dynamic button clicked");

        if let Some(button) = ui.button_mut(dynamic_button) {
            button.set_label("Clicked!");
        }
    });

    ui.on_click(apply, move|ui| {
        if let Some(text) = ui.text(name_input) {
            println!("Name: {}", text);
            ui.set_label(status, "Name applied");
        }
    });

    // ---------------------------------------------------------------------
    // Initial dynamic state
    // ---------------------------------------------------------------------

    ui.set_label(
        version,
        "Taffy layout + unified Widget trait",
    );

    ui.set_label(
        sidebar_info,
        "Scroll with PageUp/PageDown.\nClick controls to test events.",
    );

    // ---------------------------------------------------------------------
    // Main event/render loop
    // ---------------------------------------------------------------------

    loop {
        ui.process(&mut win);
    }
}