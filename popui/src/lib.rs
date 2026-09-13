//! PopugOS UI for the new `std` userspace.
//!
//! `popui` is intentionally built for PopugOS applications. It uses normal
//! Rust `std` for ordinary services and the small `popugos` crate only for OS
//! features that do not exist in `std`, such as windows and WM events.
//!
//! Widgets/layout stay synchronous. Waiting for window events, timers, network
//! tasks and application messages is asynchronous and driven by Tokio.

pub mod color;
pub mod event;
pub mod geometry;
pub mod image;
pub mod layout;
pub mod runtime;
pub mod theme;
pub mod ui;
pub mod widget;
pub mod widgets;

mod draw;

pub use color::Color;
pub use event::UiEvent;
pub use geometry::{Constraints, Point, Rect, Size};
pub use image::{Image, ImageError};
pub use runtime::{Control, UiRuntime, UiSender};
pub use theme::{DARK_THEME, Theme};
pub use ui::{Ui, WidgetId};
pub use widget::{EventResult, Widget};
pub use widgets::{
    Button, FileItem, FileKind, FileView, FileViewIcons, FileViewMode, Icon, Label, Menu,
    MenuEntry, MenuId, TextArea, TextInput, ToolbarButton, TreeNode, TreeView, TreeViewIcons,
};

pub use popugos::window::{Event as WindowEvent, Window, WindowBuilder, WindowError};
pub use taffy::prelude::{
    AlignContent, AlignItems, FlexDirection, JustifyContent, Position, Style,
};
pub use taffy::tree::NodeId;
