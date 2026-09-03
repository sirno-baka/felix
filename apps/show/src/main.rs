#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use taffy::AlignItems;
use libfelix::prelude::*;
use libfelix::layout::{presets, UiLayoutExt};
use libfelix::ui::FlexDirection;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let mut win = Window::create(1, 1, 900, 600, "layouts").unwrap();

    let mut ui = Ui::with_size(900, 600);

    // ============================================================
    // ROOT
    // ============================================================

    let root = ui.root_node();

    ui.style(root, |s| {
        s.flex_direction = FlexDirection::Column;
        s.gap = taffy::geometry::Size {
            width: taffy::prelude::LengthPercentage::length(8.0),
            height: taffy::prelude::LengthPercentage::length(8.0),
        };
        s.padding = taffy::geometry::Rect {
            left: taffy::prelude::LengthPercentage::length(12.0),
            right: taffy::prelude::LengthPercentage::length(12.0),
            top: taffy::prelude::LengthPercentage::length(12.0),
            bottom: taffy::prelude::LengthPercentage::length(12.0),
        };
    });

    // ============================================================
    // HEADER
    // ============================================================

    let header = ui.row_in(root);

    ui.style(header, |s| {
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(52.0);

        s.padding = taffy::geometry::Rect {
            left: taffy::prelude::LengthPercentage::length(12.0),
            right: taffy::prelude::LengthPercentage::length(12.0),
            top: taffy::prelude::LengthPercentage::length(8.0),
            bottom: taffy::prelude::LengthPercentage::length(8.0),
        };

        s.align_items = Some(AlignItems::CENTER);
    });

    let title = ui.label_in(header, "Felix UI");
    let _ = title;

    let _header_spacer = ui.spacer_in(header);

    let status = ui.label_in(header, "Taffy layout");
    let _ = status;

    // ============================================================
    // BODY
    // ============================================================

    let body = ui.row_in(root);

    ui.style(body, |s| {
        s.flex_grow = 1.0;
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(0.0);

        s.gap = taffy::geometry::Size {
            width: taffy::prelude::LengthPercentage::length(8.0),
            height: taffy::prelude::LengthPercentage::length(8.0),
        };
    });

    // ============================================================
    // SIDEBAR
    // ============================================================

    let sidebar = ui.panel_in(body);

    ui.style(sidebar, |s| {
        s.size.width =
            taffy::prelude::Dimension::length(190.0);

        s.padding = taffy::geometry::Rect {
            left: taffy::prelude::LengthPercentage::length(10.0),
            right: taffy::prelude::LengthPercentage::length(10.0),
            top: taffy::prelude::LengthPercentage::length(10.0),
            bottom: taffy::prelude::LengthPercentage::length(10.0),
        };

        s.gap = taffy::geometry::Size {
            width: taffy::prelude::LengthPercentage::length(6.0),
            height: taffy::prelude::LengthPercentage::length(6.0),
        };
    });

    let _sidebar_title = ui.label_in(sidebar, "Navigation");

    let home = ui.button_in(sidebar, "Home");
    let files = ui.button_in(sidebar, "Files");
    let settings = ui.button_in(sidebar, "Settings");

    let _sidebar_spacer = ui.spacer_in(sidebar);

    let _version = ui.label_in(sidebar, "Felix OS");

    // ============================================================
    // MAIN CONTENT
    // ============================================================

    let content = ui.column_in(body);

    ui.style(content, |s| {
        s.flex_grow = 1.0;
        s.min_size.width =
            taffy::prelude::LengthPercentageAuto::length(0.0);
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(0.0);

        s.gap = taffy::geometry::Size {
            width: taffy::prelude::LengthPercentage::length(8.0),
            height: taffy::prelude::LengthPercentage::length(8.0),
        };
    });

    // ------------------------------------------------------------
    // CONTENT HEADER
    // ------------------------------------------------------------

    let content_header = ui.row_in(content);

    ui.style(content_header, |s| {
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(40.0);

        s.align_items = Some(AlignItems::CENTER);
    });

    let page_title = ui.label_in(content_header, "Dashboard");

    let _ = page_title;

    let _ = ui.spacer_in(content_header);

    let refresh = ui.button_in(content_header, "Refresh");

    // ------------------------------------------------------------
    // INFORMATION PANEL
    // ------------------------------------------------------------

    let info = ui.panel_in(content);

    ui.style(info, |s| {
        s.flex_grow = 1.0;
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(0.0);

        s.gap = taffy::geometry::Size {
            width: taffy::prelude::LengthPercentage::length(6.0),
            height: taffy::prelude::LengthPercentage::length(6.0),
        };
    });

    let _info_title = ui.label_in(info, "Layout information");

    let _info1 = ui.label_in(
        info,
        "This application demonstrates Taffy flexbox layouts.",
    );

    let _info2 = ui.label_in(
        info,
        "Every widget belongs to an explicit parent node.",
    );

    let _info3 = ui.label_in(
        info,
        "The UI tree is independent from application state.",
    );

    // ------------------------------------------------------------
    // INPUT ROW
    // ------------------------------------------------------------

    let input_row = ui.row_in(content);

    ui.style(input_row, |s| {
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(38.0);

        s.gap = taffy::geometry::Size {
            width: taffy::prelude::LengthPercentage::length(8.0),
            height: taffy::prelude::LengthPercentage::length(8.0),
        };

        s.align_items = Some(AlignItems::CENTER);
    });

    let _input_label = ui.label_in(input_row, "Name:");

    let input = ui.text_input_with_in(input_row, "Felix");

    ui.style(
        ui.node_of(input).unwrap(),
        |s| {
            s.flex_grow = 1.0;
            s.min_size.width =
                taffy::prelude::LengthPercentageAuto::length(100.0);
        },
    );

    let submit = ui.button_in(input_row, "Apply");

    // ------------------------------------------------------------
    // FOOTER
    // ------------------------------------------------------------

    let footer = ui.row_in(root);

    ui.style(footer, |s| {
        s.min_size.height =
            taffy::prelude::LengthPercentageAuto::length(34.0);

        s.align_items = Some(AlignItems::CENTER);
    });

    let footer_text = ui.label_in(
        footer,
        "Ready",
    );

    let _ = footer_text;

    let _ = ui.spacer_in(footer);

    let _footer_status = ui.label_in(
        footer,
        "900 x 600",
    );

    // ============================================================
    // EVENTS
    // ============================================================

    ui.on_click(home, |_ui| {
        println!("Home clicked");
    });

    ui.on_click(files, |_ui| {
        println!("Files clicked");
    });

    ui.on_click(settings, |_ui| {
        println!("Settings clicked");
    });

    ui.on_click(refresh, |_ui| {
        println!("Refresh clicked");
    });

    ui.on_click(submit, move |ui| {
        if let Some(text) = ui.text(input) {
            println!("Name: {}", text);
        }
    });

    // ============================================================
    // MAIN LOOP
    // ============================================================

    loop {
        ui.process(&mut win);
    }
}

