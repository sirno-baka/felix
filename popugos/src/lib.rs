//! PopugOS-specific userspace API.
//!
//! This crate intentionally contains only functionality that Rust `std` does
//! not provide. Filesystem, networking, time, threads, and ordinary I/O should
//! use `std`/Tokio directly.

pub mod window;

pub use window::{
    focus_window, screen_size, window_list, Event as WindowEvent, KeyEvent, MouseButton, MouseEvent,
    Window, WindowBuilder, WindowError, WindowFlags, WindowInfo, WindowListItem,
};

mod sys;

pub fn reboot() -> ! {
    unsafe { sys::reboot() }
}

pub fn poweroff() -> ! {
    unsafe { sys::poweroff() }
}
