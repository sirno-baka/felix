//! PopugOS-specific userspace API.
//!
//! This crate intentionally contains only functionality that Rust `std` does
//! not provide. Filesystem, networking, time, threads, and ordinary I/O should
//! use `std`/Tokio directly.

pub mod window;

pub use window::{
    screen_size, Event as WindowEvent, KeyEvent, MouseButton, MouseEvent, Window, WindowBuilder,
    WindowError, WindowFlags, WindowInfo,
};

mod sys;
