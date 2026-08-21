use core::arch::asm;

// Должны совпадать с kernel/src/syscalls/mod.rs
pub const SYS_EXIT:   u32 = 1;
pub const SYS_READ:   u32 = 3;
pub const SYS_WRITE:  u32 = 4;
pub const SYS_OPEN:   u32 = 5;
pub const SYS_CLOSE:  u32 = 6;

pub const SYS_MKDIR:  u32 = 7;
pub const SYS_RMDIR:  u32 = 8;
pub const SYS_UNLINK: u32 = 10;
pub const SYS_EXECVE: u32 = 11;
pub const SYS_KILL:   u32 = 37;
pub const SYS_SIGACTION: u32 = 67;
pub const SYS_WAIT:   u32 = 114;
pub const SYS_PIPE:   u32 = 42;
pub const SYS_DUP2:   u32 = 63;
pub const SYS_FCNTL:  u32 = 55;
pub const SYS_POLL:   u32 = 168;

// open flags
pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR:   u32 = 2;
pub const O_CREAT:  u32 = 0x40;
pub const O_TRUNC:  u32 = 0x200;
pub const O_APPEND: u32 = 0x400;
pub const O_NONBLOCK: u32 = 0x800;

pub const F_GETFL: u32 = 3;
pub const F_SETFL: u32 = 4;

pub const WNOHANG: u32 = 1;

pub const SIGHUP:  u32 = 1;
pub const SIGINT:  u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGKILL: u32 = 9;
pub const SIGTERM: u32 = 15;

pub const SIG_DFL: u32 = 0;
pub const SIG_IGN: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SigAction {
    pub sa_handler: u32,
    pub sa_mask: u32,
    pub sa_flags: u32,
}

pub const POLLIN: i16 = 0x0001;
pub const POLLOUT: i16 = 0x0004;
pub const POLLERR: i16 = 0x0008;
pub const POLLHUP: i16 = 0x0010;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PollFd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

pub const SYS_MALLOC: u32 = 200;
pub const SYS_FREE:   u32 = 201;
pub const SYS_REALLOC: u32 = 202;

pub const SYS_LS:     u32 = 302;

// ====================== WRAPPERS ======================

pub unsafe fn exit() -> ! {
    asm!("int 0x80", in("eax") SYS_EXIT, options(noreturn));
}

pub unsafe fn write(fd: u32, buf: *const u8, len: usize) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_WRITE => ret,
    in("ebx") fd,
    in("ecx") buf,
    in("edx") len,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn read(fd: u32, buf: *mut u8, len: usize) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_READ => ret,
    in("ebx") fd,
    in("ecx") buf,
    in("edx") len,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn open(path: *const u8, flags: u32) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_OPEN => ret,
    in("ebx") path,
    in("ecx") flags,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn close(fd: u32) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_CLOSE => ret,
    in("ebx") fd,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn mkdir(path: *const u8) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_MKDIR => ret,
    in("ebx") path,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn rmdir(path: *const u8) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_RMDIR => ret,
    in("ebx") path,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn unlink(path: *const u8) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_UNLINK => ret,
    in("ebx") path,
    options(nostack, preserves_flags)
    );
    ret
}

/// Parameters for `execve` (passed via edx).
#[repr(C)]
pub struct ExecParams {
    pub stdin: i32,
    pub stdout: i32,
    pub stderr: i32,
    pub argc: u32,
    /// Array of `argc` pointers to C strings in the caller's address space.
    pub argv: *const *const u8,
}

/// Spawn a new task from an in-memory ELF image.
/// `argv` is a slice of C-string pointers (like Unix argv), including argv[0].
/// Returns the new task's pid (slot), or usize::MAX on failure.
///
/// ABI: ebx=buf, ecx=len, edx=*const ExecParams.
pub unsafe fn execve(
    buf: *const u8,
    buf_size: usize,
    stdin_fd: i32,
    stdout_fd: i32,
    stderr_fd: i32,
    argv: &[*const u8],
) -> usize {
    let params = ExecParams {
        stdin: stdin_fd,
        stdout: stdout_fd,
        stderr: stderr_fd,
        argc: argv.len() as u32,
        argv: argv.as_ptr(),
    };
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_EXECVE => ret,
    in("ebx") buf,
    in("ecx") buf_size,
    in("edx") &params as *const ExecParams,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn pipe(pipefd: *mut u32) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_PIPE => ret,
    in("ebx") pipefd,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn dup2(oldfd: u32, newfd: u32) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_DUP2 => ret,
    in("ebx") oldfd,
    in("ecx") newfd,
    options(nostack, preserves_flags)
    );
    ret
}

/// Block until the child with the given pid exits (or any child if pid == -1).
/// Returns the reaped child's pid, or usize::MAX on error.
pub unsafe fn wait(pid: i32) -> usize {
    wait_options(pid, 0)
}

/// `options`: WNOHANG — return 0 if no child is ready.
pub unsafe fn wait_options(pid: i32, options: u32) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_WAIT => ret,
    in("ebx") pid,
    in("ecx") options,
    options(nostack, preserves_flags)
    );
    ret
}

/// Queue `sig` for task `pid`. Returns 0 on success.
pub unsafe fn kill(pid: i32, sig: u32) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_KILL => ret,
    in("ebx") pid,
    in("ecx") sig,
    options(nostack, preserves_flags)
    );
    ret
}

/// Set/get signal handler. act/oldact may be null.
pub unsafe fn sigaction(sig: u32, act: *const SigAction, oldact: *mut SigAction) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_SIGACTION => ret,
    in("ebx") sig,
    in("ecx") act,
    in("edx") oldact,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn fcntl(fd: u32, cmd: u32, arg: u32) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_FCNTL => ret,
    in("ebx") fd,
    in("ecx") cmd,
    in("edx") arg,
    options(nostack, preserves_flags)
    );
    ret
}

/// Returns number of ready fds. timeout_ms: -1 block, 0 nonblock, >0 ms.
pub unsafe fn poll(fds: *mut PollFd, nfds: usize, timeout_ms: i32) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_POLL => ret,
    in("ebx") fds,
    in("ecx") nfds,
    in("edx") timeout_ms,
    options(nostack, preserves_flags)
    );
    ret
}

/// Set O_NONBLOCK on fd.
pub unsafe fn set_nonblock(fd: u32) -> bool {
    let cur = fcntl(fd, F_GETFL, 0);
    if cur == usize::MAX {
        return false;
    }
    fcntl(fd, F_SETFL, (cur as u32) | O_NONBLOCK) == 0
}

/// Читает содержимое директории.
/// Записывает имена файлов (разделённые '\n') в `buf`.
/// Возвращает количество записанных байт или 0 при ошибке.
pub unsafe fn ls(path: *const u8, buf: *mut u8, buf_size: usize) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_LS => ret,
    in("ebx") path,
    in("ecx") buf,
    in("edx") buf_size,
    options(nostack, preserves_flags)
    );
    ret
}


pub const SYS_SOCKET:      u32 = 359;
pub const SYS_BIND:        u32 = 361;
pub const SYS_CONNECT:     u32 = 362;
pub const SYS_LISTEN:      u32 = 363;
pub const SYS_ACCEPT4:     u32 = 364;
pub const SYS_SENDTO:      u32 = 369;
pub const SYS_RECVFROM:    u32 = 371;
pub const SYS_SHUTDOWN:    u32 = 373;

// Window manager — must match kernel/src/syscalls/mod.rs
pub const SYS_WM_CREATE:  u32 = 400;
pub const SYS_WM_DESTROY: u32 = 401;
pub const SYS_WM_MOVE:    u32 = 402;
pub const SYS_WM_INFO:    u32 = 403;
pub const SYS_WM_FLIP:    u32 = 404;
pub const SYS_WM_FOCUS:   u32 = 405;
pub const SYS_WM_SCREEN:  u32 = 406;
pub const SYS_MOUSE_STATE: u32 = 407;
pub const SYS_WM_POLL: u32 = 408;

#[repr(C)]
pub struct WmCreateArgs {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub title: *const u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct WindowInfo {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub client_w: u32,
    pub client_h: u32,
    pub pitch: u32,
    pub focused: u32,
}

pub unsafe fn wm_create(x: i32, y: i32, w: u32, h: u32, title: *const u8) -> usize {
    let args = WmCreateArgs { x, y, w, h, title };
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_WM_CREATE => ret,
        in("ebx") &args as *const WmCreateArgs,
        options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn wm_destroy(id: u32) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_WM_DESTROY => ret,
        in("ebx") id,
        options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn wm_move(id: u32, x: i32, y: i32) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_WM_MOVE => ret,
        in("ebx") id,
        in("ecx") x,
        in("edx") y,
        options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn wm_info(id: u32, out: *mut WindowInfo) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_WM_INFO => ret,
        in("ebx") id,
        in("ecx") out,
        options(nostack, preserves_flags)
    );
    ret
}

/// Copy `pixels` (BGRx 32bpp, pitch from WindowInfo) into the window surface and compose.
pub unsafe fn wm_flip(id: u32, pixels: *const u8, len: usize) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_WM_FLIP => ret,
        in("ebx") id,
        in("ecx") pixels,
        in("edx") len,
        options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn wm_focus(id: u32) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_WM_FOCUS => ret,
        in("ebx") id,
        options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn wm_screen_size(out: *mut u32) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_WM_SCREEN => ret,
        in("ebx") out,
        options(nostack, preserves_flags)
    );
    ret
}

/// Mouse snapshot (screen coordinates).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MouseState {
    pub x: i32,
    pub y: i32,
    /// bit0=left, bit1=right, bit2=middle
    pub buttons: u8,
    pub _pad: [u8; 3],
}

pub unsafe fn mouse_state(out: *mut MouseState) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_MOUSE_STATE => ret,
        in("ebx") out,
        options(nostack, preserves_flags)
    );
    ret
}

/// Window event — must match kernel `drivers::wm::WmEvent`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct WmEvent {
    pub kind: u32,
    pub a: i32,
    pub b: i32,
    pub c: i32,
    pub d: i32,
}

pub const EV_NONE: u32 = 0;
pub const EV_MOUSE_MOVE: u32 = 1;
pub const EV_MOUSE_DOWN: u32 = 2;
pub const EV_MOUSE_UP: u32 = 3;
pub const EV_KEY_DOWN: u32 = 4;
pub const EV_KEY_UP: u32 = 5;
pub const EV_CLOSE: u32 = 6;
pub const EV_FOCUS_IN: u32 = 7;
pub const EV_FOCUS_OUT: u32 = 8;

/// Non-blocking: copy up to `max` events for window `id` into `out`.
pub unsafe fn wm_poll(id: u32, out: *mut WmEvent, max: usize) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_WM_POLL => ret,
        in("ebx") id,
        in("ecx") out,
        in("edx") max,
        options(nostack, preserves_flags)
    );
    ret
}


pub const AF_INET:     u32 = 2;
pub const SOCK_STREAM: u32 = 1;
pub const SOCK_DGRAM:  u32 = 2;
pub const IPPROTO_IP:  u32 = 0;
pub const IPPROTO_TCP: u32 = 6;
pub const IPPROTO_UDP: u32 = 17;
pub unsafe fn socket(domain: u32, ty: u32, protocol: u32) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_SOCKET => ret,
    in("ebx") domain,
    in("ecx") ty,
    in("edx") protocol,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn bind(sockfd: u32, addr: *const u8, addrlen: u32) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_BIND => ret,
    in("ebx") sockfd,
    in("ecx") addr,
    in("edx") addrlen,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn listen(sockfd: u32, backlog: u32) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_LISTEN => ret,
    in("ebx") sockfd,
    in("ecx") backlog,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn accept4(sockfd: u32, addr: *mut u8, addrlen: *mut u32, _flags: u32) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_ACCEPT4 => ret,
    in("ebx") sockfd,
    in("ecx") addr,
    in("edx") addrlen,
    // flags пока не передаём (в kernel stub он всё равно игнорируется)
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn connect(sockfd: u32, addr: *const u8, addrlen: u32) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_CONNECT => ret,
    in("ebx") sockfd,
    in("ecx") addr,
    in("edx") addrlen,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn recvfrom(sockfd: u32, buf: *mut u8, len: usize) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_RECVFROM => ret,
    in("ebx") sockfd,
    in("ecx") buf,
    in("edx") len,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn sendto(sockfd: u32, buf: *const u8, len: usize) -> usize {
    let mut ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_SENDTO => ret,
    in("ebx") sockfd,
    in("ecx") buf,
    in("edx") len,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn shutdown(sockfd: u32, how: u32) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_SHUTDOWN => ret,
    in("ebx") sockfd,
    in("ecx") how,
    options(nostack, preserves_flags)
    );
    ret
}