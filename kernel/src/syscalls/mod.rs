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
/// lseek(fd, offset, whence) — Linux i386 #19
pub const SYS_LSEEK: u32 = 19;
/// brk(addr) — Linux i386 #45; brk(0) returns current break
pub const SYS_BRK: u32 = 45;
/// old mmap(struct mmap_arg_struct*) — Linux i386 #90
pub const SYS_MMAP: u32 = 90;
/// munmap(addr, len) — Linux i386 #91
pub const SYS_MUNMAP: u32 = 91;
/// mmap2(addr,len,prot,flags,fd,pgoff) — Linux i386 #192
pub const SYS_MMAP2: u32 = 192;
/// ioctl(fd, request, arg) — Linux i386 #54 (stub)
pub const SYS_IOCTL: u32 = 54;
/// fstat64(fd, statbuf) — Linux i386 #197
pub const SYS_FSTAT64: u32 = 197;
/// stat64(path, statbuf) — Linux i386 #195
pub const SYS_STAT64: u32 = 195;
/// getdents64(fd, dirp, count) — Linux i386 #220
pub const SYS_GETDENTS64: u32 = 220;
/// exit_group(status) — Linux i386 #252 (= exit)
pub const SYS_EXIT_GROUP: u32 = 252;
/// kill(pid, sig) — queue signal for task. 0 on success, usize::MAX on error.
pub const SYS_KILL: u32 = 37;
/// sigaction(sig, act, oldact) — set/get signal handler. 0 on success.
pub const SYS_SIGACTION: u32 = 67;
/// wait(pid, options) — block until child exits (-1 = any). options: WNOHANG=1
/// Returns pid of the reaped child, 0 if WNOHANG and none ready, or usize::MAX on error.
pub const SYS_WAIT: u32 = 114;
/// pipe(pipefd: *mut u32) — writes [read_fd, write_fd], returns 0 or usize::MAX
pub const SYS_PIPE: u32 = 42;
/// dup2(oldfd, newfd) → newfd or usize::MAX
pub const SYS_DUP2: u32 = 63;
/// fcntl(fd, cmd, arg) — F_GETFL=3, F_SETFL=4
pub const SYS_FCNTL: u32 = 55;
/// poll(fds, nfds, timeout_ms) — timeout -1 = block, 0 = nonblock
pub const SYS_POLL: u32 = 168;

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

// Window manager (kernel compositor)
/// create(x, y, w, h, title_ptr) → window_id or usize::MAX
pub const SYS_WM_CREATE:  u32 = 400;
/// destroy(id) → 0 / usize::MAX
pub const SYS_WM_DESTROY: u32 = 401;
/// move(id, x, y) → 0 / usize::MAX
pub const SYS_WM_MOVE:    u32 = 402;
/// info(id, *mut WindowInfo) → 0 / usize::MAX
pub const SYS_WM_INFO:    u32 = 403;
/// flip(id, user_pixels, len) → 0 / usize::MAX  (copy + compose)
pub const SYS_WM_FLIP:    u32 = 404;
/// focus(id) → 0 / usize::MAX
pub const SYS_WM_FOCUS:   u32 = 405;
/// screen_size(*mut u32 /*w,h*/) → 0
pub const SYS_WM_SCREEN:  u32 = 406;
/// mouse_state(*mut MouseState) → 0 / usize::MAX
pub const SYS_MOUSE_STATE: u32 = 407;
/// wm_poll(id, *mut WmEvent, max) → number of events copied
pub const SYS_WM_POLL: u32 = 408;