#![feature(pointer_byte_offsets)]
#![feature(alloc_error_handler)]
#![no_std]

extern crate alloc;

pub mod mutex;
pub mod print;
pub mod sys_alloc;
pub mod syscall;
pub mod fs;

/// Userspace runtime: provides `_start` → `main` and the panic handler.
/// Applications should define `#[no_mangle] pub extern "C" fn main() -> i32`
/// and must NOT define their own `_start` or `#[panic_handler]`.
pub mod rt;

/// Convenient re-exports for application code.
pub mod prelude {
    pub use crate::print;
    pub use crate::println;
    pub use crate::fs::{File, IoError, IoResult};
    pub use alloc::string::String;
    pub use alloc::vec::Vec;
    pub use alloc::boxed::Box;
}
