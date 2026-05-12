pub mod handler;
mod alloc;

pub const SYS_EXIT: u32 = 1;
pub const SYS_WRITE: u32 = 4;
pub const SYS_MALLOC: u32 = 200;
pub const SYS_FREE: u32 = 201;