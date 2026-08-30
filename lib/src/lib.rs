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

/// Re-export so apps can use embedded-graphics against our Window.
pub use embedded_graphics;

/// Userspace runtime: provides `_start` → `main` and the panic handler.
/// Applications should define `#[no_mangle] pub extern "C" fn main() -> i32`
/// and must NOT define their own `_start` or `#[panic_handler]`.
pub mod rt;
pub mod net;

/// Convenient re-exports for application code.
pub mod prelude {
    pub use crate::async_rt::{self, block_on, wait_readable, yield_now, Executor};
    pub use crate::fs::{File, IoError, IoResult};
    pub use crate::args::Args;
    pub use crate::print;
    pub use crate::println;
    pub use crate::rt::{arg, argc, args};
    pub use crate::signal::{
        self, default, exit, exit_on_terminate, ignore, on, SIGINT, SIGKILL, SIGTERM,
    };
    pub use crate::ui::{self, Button, Label, MouseTracker, TextInput, Ui, UiEvent, WidgetId};
    pub use crate::wm::{self, mouse, rgb, screen_size, MouseState, Window, WindowInfo, WmEvent};
    pub use crate::wm::{
        EV_CLOSE, EV_FOCUS_IN, EV_FOCUS_OUT, EV_KEY_DOWN, EV_KEY_UP, EV_MOUSE_DOWN, EV_MOUSE_MOVE,
        EV_MOUSE_UP, EV_RESIZE,
    };
    pub use alloc::boxed::Box;
    pub use alloc::string::String;
    pub use alloc::vec::Vec;
}
