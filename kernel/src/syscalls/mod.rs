pub mod handler;
mod alloc;

pub const SYS_EXIT: u32 = 1;
pub const SYS_READ: u32 = 3;
pub const SYS_WRITE: u32 = 4;
pub const SYS_OPEN:   u32 = 5;   // open(filename) → fd или -1
pub const SYS_CLOSE:  u32 = 6;   // close(fd)
pub const SYS_MKDIR: u32 = 7;   // mkdir
pub const SYS_RMDIR: u32 = 8;   // rmdir
pub const SYS_UNLINK: u32 = 10;  // delete/unlink(filename)

pub const SYS_EXECVE: u32 = 11;

pub const SYS_MALLOC: u32 = 200;
pub const SYS_FREE: u32 = 201;
pub const SYS_REALLOC: u32 = 202;   // ← добавь
pub const SYS_LS:     u32 = 302; // ls()


// Socket syscalls (Linux i386 numbers)
pub const SYS_SOCKET:      u32 = 359;
pub const SYS_SOCKETPAIR:  u32 = 360;
pub const SYS_BIND:        u32 = 361;
pub const SYS_CONNECT:     u32 = 362;
pub const SYS_LISTEN:      u32 = 363;
pub const SYS_ACCEPT4:     u32 = 364;
pub const SYS_GETSOCKOPT:  u32 = 365;
pub const SYS_SETSOCKOPT:  u32 = 366;
pub const SYS_GETSOCKNAME: u32 = 367;
pub const SYS_GETPEERNAME: u32 = 368;
pub const SYS_SENDTO:      u32 = 369;
pub const SYS_SENDMSG:     u32 = 370;
pub const SYS_RECVFROM:    u32 = 371;
pub const SYS_RECVMSG:     u32 = 372;
pub const SYS_SHUTDOWN:    u32 = 373;