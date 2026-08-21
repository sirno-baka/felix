#![feature(pointer_byte_offsets)]
#![feature(alloc_error_handler)]
#![no_std]

extern crate alloc;

pub mod mutex;
pub mod print;
pub mod sys_alloc;
pub mod syscall;
pub mod fs;
pub mod wm;
pub mod ui;
pub mod async_rt;

/// Re-export so apps can use embedded-graphics against our Window.
pub use embedded_graphics;

/// Userspace runtime: provides `_start` → `main` and the panic handler.
/// Applications should define `#[no_mangle] pub extern "C" fn main() -> i32`
/// and must NOT define their own `_start` or `#[panic_handler]`.
pub mod rt;

/// Convenient re-exports for application code.
pub mod prelude {
    pub use crate::print;
    pub use crate::println;
    pub use crate::fs::{File, IoError, IoResult};
    pub use crate::rt::{arg, argc, args};
    pub use crate::wm::{self, Window, WindowInfo, MouseState, WmEvent, rgb, screen_size, mouse};
    pub use crate::wm::{
        EV_MOUSE_MOVE, EV_MOUSE_DOWN, EV_MOUSE_UP, EV_KEY_DOWN, EV_KEY_UP,
        EV_CLOSE, EV_FOCUS_IN, EV_FOCUS_OUT,
    };
    pub use crate::ui::{self, Button, Label, TextInput, Ui, UiEvent, MouseTracker, WidgetId};
    pub use alloc::string::String;
    pub use alloc::vec::Vec;
    pub use alloc::boxed::Box;
    pub use crate::async_rt::{self, block_on, yield_now, Executor, wait_readable};
}
