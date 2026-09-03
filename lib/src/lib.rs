#![feature(pointer_byte_offsets)]
#![feature(alloc_error_handler)]
#![no_std]

extern crate alloc;

pub mod args;
pub mod async_rt;
pub mod fs;
pub mod mutex;
pub mod print;
pub mod signal;
pub mod sys_alloc;
pub mod syscall;
pub mod ui;
pub mod wm;

pub use embedded_graphics;

pub mod rt;
pub mod net;
pub mod flags;

pub mod prelude {
    pub use crate::async_rt::{self, block_on, wait_readable, yield_now, Executor};
    pub use crate::fs::{File, IoError, IoResult};
    pub use crate::args::Args;
    pub use crate::print;
    pub use crate::println;
    pub use crate::rt::{arg, argc, args};
    pub use crate::signal::{self, default, exit, exit_on_terminate, ignore, on, SIGINT, SIGKILL, SIGTERM};
    pub use crate::ui::{self, Button, Constraints, EventResult, Label, NodeId, Rect, ScrollViewId, TextInput, Ui, UiEvent, Widget, WidgetId};
    pub use crate::ui::layout::{self, LayoutApi};
    pub use taffy::prelude::{AlignContent, AlignItems, FlexDirection, JustifyContent, Position, Style};
    pub use crate::wm::{self, mouse, rgb, screen_size, MouseState, Window, WindowInfo, WmEvent};
    pub use crate::wm::{EV_CLOSE, EV_FOCUS_IN, EV_FOCUS_OUT, EV_KEY_DOWN, EV_KEY_UP, EV_MOUSE_DOWN, EV_MOUSE_MOVE, EV_MOUSE_UP, EV_RESIZE};
    pub use alloc::boxed::Box;
    pub use alloc::string::String;
    pub use alloc::vec::Vec;
}
