pub mod handler;
mod alloc;

pub const SYS_EXIT: u32 = 1;
pub const SYS_WRITE: u32 = 4;
pub const SYS_OPEN:   u32 = 5;   // open(filename) → fd или -1
pub const SYS_CLOSE:  u32 = 6;   // close(fd)
pub const SYS_UNLINK: u32 = 10;  // delete/unlink(filename)
pub const SYS_MALLOC: u32 = 200;
pub const SYS_FREE: u32 = 201;
pub const SYS_LS:     u32 = 302; // ls()