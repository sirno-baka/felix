use alloc::vec::Vec;
use core::arch::asm;

// Должны совпадать с kernel/src/syscalls/mod.rs
pub const SYS_EXIT: u32 = 1;
pub const SYS_READ: u32 = 3;
pub const SYS_WRITE: u32 = 4;
pub const SYS_OPEN: u32 = 5;
pub const SYS_CLOSE: u32 = 6;

pub const SYS_MKDIR: u32 = 39;
pub const SYS_RMDIR: u32 = 40;
pub const SYS_UNLINK: u32 = 10;
pub const SYS_EXECVE: u32 = 11;
pub const SYS_CHDIR: u32 = 12;
pub const SYS_SPAWN: u32 = 0xF000;
pub const SYS_EXECVE_WASM: u32 = 0xF001;

pub const SYS_LSEEK: u32 = 19;
pub const SYS_GETPID: u32 = 20;
pub const SYS_MOUNT: u32 = 21;
pub const SYS_DUP: u32 = 41;
pub const SYS_UMOUNT2: u32 = 52;
pub const SYS_SETPGID: u32 = 57;
pub const SYS_GETPPID: u32 = 64;
pub const SYS_GETPGRP: u32 = 65;
pub const SYS_SETSID: u32 = 66;
pub const SYS_GETTIMEOFDAY: u32 = 78;
pub const SYS_GETPGID: u32 = 132;
pub const SYS_GETSID: u32 = 147;
pub const SYS_NANOSLEEP: u32 = 162;
pub const SYS_GETCWD: u32 = 183;
pub const SYS_CLOCK_GETTIME: u32 = 265;
pub const CLOCK_REALTIME: i32 = 0;
pub const CLOCK_MONOTONIC: i32 = 1;
pub const SYS_BRK: u32 = 45;
pub const SYS_MMAP: u32 = 90;
pub const SYS_MUNMAP: u32 = 91;
pub const SYS_MMAP2: u32 = 192;
pub const SYS_IOCTL: u32 = 54;

pub const TCGETS: u32 = 0x5401;
pub const TCSETS: u32 = 0x5402;
pub const TCSETSW: u32 = 0x5403;
pub const TCSETSF: u32 = 0x5404;
pub const TIOCGPGRP: u32 = 0x540F;
pub const TIOCSPGRP: u32 = 0x5410;
pub const IFLAG_ICRNL: u32 = 0x0100;
pub const OFLAG_OPOST: u32 = 0x0001;
pub const OFLAG_ONLCR: u32 = 0x0004;
pub const LFLAG_ISIG: u32 = 0x0001;
pub const LFLAG_ICANON: u32 = 0x0002;
pub const LFLAG_ECHO: u32 = 0x0008;
pub const LFLAG_TOSTOP: u32 = 0x0100;

pub const VINTR: usize = 0;
pub const VQUIT: usize = 1;
pub const VERASE: usize = 2;
pub const VKILL: usize = 3;
pub const VEOF: usize = 4;
pub const VTIME: usize = 5;
pub const VMIN: usize = 6;
pub const VSUSP: usize = 10;

pub const TCSANOW: u32 = 0;
pub const TCSADRAIN: u32 = 1;
pub const TCSAFLUSH: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_line: u8,
    pub c_cc: [u8; 19],
}

impl Default for Termios {
    fn default() -> Self {
        let mut c_cc = [0u8; 19];
        c_cc[VINTR] = 0x03;
        c_cc[VQUIT] = 0x1c;
        c_cc[VERASE] = 0x7f;
        c_cc[VKILL] = 0x15;
        c_cc[VEOF] = 0x04;
        c_cc[VTIME] = 0;
        c_cc[VMIN] = 1;
        c_cc[VSUSP] = 0x1a;
        Self {
            c_iflag: IFLAG_ICRNL,
            c_oflag: OFLAG_OPOST | OFLAG_ONLCR,
            c_cflag: 0,
            c_lflag: LFLAG_ISIG | LFLAG_ICANON | LFLAG_ECHO,
            c_line: 0,
            c_cc,
        }
    }
}

pub const PROT_READ: u32 = 1;
pub const PROT_WRITE: u32 = 2;
pub const PROT_EXEC: u32 = 4;
pub const MAP_SHARED: u32 = 0x01;
pub const MAP_PRIVATE: u32 = 0x02;
pub const MAP_FIXED: u32 = 0x10;
pub const MAP_ANONYMOUS: u32 = 0x20;
pub const SYS_KILL: u32 = 37;
pub const SYS_RENAME: u32 = 38;
pub const SYS_SIGACTION: u32 = 67;
pub const SYS_WAIT: u32 = 7;
pub const SYS_PIPE: u32 = 42;
pub const SYS_DUP2: u32 = 63;
pub const SYS_FCNTL: u32 = 55;
pub const SYS_POLL: u32 = 168;
pub const SYS_STAT64: u32 = 195;
pub const SYS_FSTAT64: u32 = 197;

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Stat64 {
    pub st_dev: u64,
    pub __pad0: [u8; 4],
    pub __st_ino: u32,
    pub st_mode: u32,
    pub st_nlink: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: u64,
    pub __pad3: [u8; 4],
    pub st_size: i64,
    pub st_blksize: u32,
    pub st_blocks: u64,
    pub st_atime: u32,
    pub st_atime_nsec: u32,
    pub st_mtime: u32,
    pub st_mtime_nsec: u32,
    pub st_ctime: u32,
    pub st_ctime_nsec: u32,
    pub st_ino: u64,
}

pub const S_IFMT: u32 = 0o170000;
pub const S_IFIFO: u32 = 0o010000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFSOCK: u32 = 0o140000;
pub const SYS_GETDENTS64: u32 = 220;
pub const SYS_EXIT_GROUP: u32 = 252;

pub const SEEK_SET: u32 = 0;
pub const SEEK_CUR: u32 = 1;
pub const SEEK_END: u32 = 2;

// open flags
pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_CREAT: u32 = 0x40;
pub const O_TRUNC: u32 = 0x200;
pub const O_APPEND: u32 = 0x400;
pub const O_NONBLOCK: u32 = 0x800;

pub const F_GETFL: u32 = 3;
pub const F_SETFL: u32 = 4;

pub const WNOHANG: u32 = 1;
pub const WUNTRACED: u32 = 2;
pub const WCONTINUED: u32 = 8;

#[inline]
pub const fn wifexited(status: i32) -> bool {
    (status & 0x7f) == 0
}

#[inline]
pub const fn wexitstatus(status: i32) -> i32 {
    (status >> 8) & 0xff
}

#[inline]
pub const fn wifsignaled(status: i32) -> bool {
    let sig = status & 0x7f;
    sig != 0 && sig != 0x7f
}

#[inline]
pub const fn wtermsig(status: i32) -> u32 {
    (status & 0x7f) as u32
}

#[inline]
pub const fn wifstopped(status: i32) -> bool {
    (status & 0xff) == 0x7f
}

#[inline]
pub const fn wstopsig(status: i32) -> u32 {
    ((status >> 8) & 0xff) as u32
}

#[inline]
pub const fn wifcontinued(status: i32) -> bool {
    status == 0xffff
}

pub const SIGHUP: u32 = 1;
pub const SIGINT: u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGKILL: u32 = 9;
pub const SIGTERM: u32 = 15;
pub const SIGCONT: u32 = 18;
pub const SIGSTOP: u32 = 19;
pub const SIGTSTP: u32 = 20;
pub const SIGTTIN: u32 = 21;
pub const SIGTTOU: u32 = 22;

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

pub const SYS_MALLOC: u32 = 0xF010;
pub const SYS_FREE: u32 = 0xF011;
pub const SYS_REALLOC: u32 = 0xF012;

pub const SYS_LS: u32 = 0xF013;

// ====================== WRAPPERS ======================

pub unsafe fn exit() -> ! {
    asm!("int 0x80", in("eax") SYS_EXIT, options(noreturn));
    loop {}
}

/// Exit with an explicit status code. The kernel stores it for waitpid/$?.
pub unsafe fn exit_status(status: i32) -> ! {
    asm!(
        "int 0x80",
        in("eax") SYS_EXIT_GROUP,
        in("ebx") status,
        options(noreturn)
    );
    loop {}
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

pub unsafe fn rename(old_path: *const u8, new_path: *const u8) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_RENAME => ret,
        in("ebx") old_path,
        in("ecx") new_path,
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

pub unsafe fn chdir(path: *const u8) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_CHDIR => ret,
        in("ebx") path,
        options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn getcwd(buf: *mut u8, size: usize) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_GETCWD => ret,
        in("ebx") buf,
        in("ecx") size,
        options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn getpid() -> i32 {
    let ret: usize;
    asm!("int 0x80", inlateout("eax") SYS_GETPID => ret, options(nostack, preserves_flags));
    ret as i32
}

pub unsafe fn getppid() -> i32 {
    let ret: usize;
    asm!("int 0x80", inlateout("eax") SYS_GETPPID => ret, options(nostack, preserves_flags));
    ret as i32
}

pub unsafe fn getpgrp() -> i32 {
    let ret: usize;
    asm!("int 0x80", inlateout("eax") SYS_GETPGRP => ret, options(nostack, preserves_flags));
    ret as i32
}

pub unsafe fn getpgid(pid: i32) -> i32 {
    let ret: usize;
    asm!("int 0x80", inlateout("eax") SYS_GETPGID => ret, in("ebx") pid, options(nostack, preserves_flags));
    ret as i32
}

pub unsafe fn getsid(pid: i32) -> i32 {
    let ret: usize;
    asm!("int 0x80", inlateout("eax") SYS_GETSID => ret, in("ebx") pid, options(nostack, preserves_flags));
    ret as i32
}

pub unsafe fn setpgid(pid: i32, pgid: i32) -> usize {
    let ret: usize;
    asm!("int 0x80", inlateout("eax") SYS_SETPGID => ret, in("ebx") pid, in("ecx") pgid, options(nostack, preserves_flags));
    ret
}

pub unsafe fn setsid() -> i32 {
    let ret: usize;
    asm!("int 0x80", inlateout("eax") SYS_SETSID => ret, options(nostack, preserves_flags));
    ret as i32
}

pub unsafe fn dup(oldfd: u32) -> usize {
    let ret: usize;
    asm!("int 0x80", inlateout("eax") SYS_DUP => ret, in("ebx") oldfd, options(nostack, preserves_flags));
    ret
}

pub unsafe fn ioctl(fd: u32, request: u32, arg: u32) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_IOCTL => ret,
        in("ebx") fd,
        in("ecx") request,
        in("edx") arg,
        options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn tcgetattr(fd: u32, termios: *mut Termios) -> usize {
    ioctl(fd, TCGETS, termios as u32)
}

pub unsafe fn tcsetattr(fd: u32, termios: *const Termios) -> usize {
    tcsetattr_action(fd, TCSANOW, termios)
}

pub unsafe fn tcsetattr_action(fd: u32, optional_actions: u32, termios: *const Termios) -> usize {
    let request = match optional_actions {
        TCSANOW => TCSETS,
        TCSADRAIN => TCSETSW,
        TCSAFLUSH => TCSETSF,
        _ => return usize::MAX,
    };
    ioctl(fd, request, termios as u32)
}

pub unsafe fn tcgetpgrp(fd: u32) -> i32 {
    let mut pgid = -1i32;
    let ret = ioctl(fd, TIOCGPGRP, (&mut pgid as *mut i32) as u32);
    if ret == 0 { pgid } else { -1 }
}

pub unsafe fn tcsetpgrp(fd: u32, pgid: i32) -> usize {
    ioctl(fd, TIOCSPGRP, (&pgid as *const i32) as u32)
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TimeVal {
    pub tv_sec: i32,
    pub tv_usec: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TimeSpec {
    pub tv_sec: i32,
    pub tv_nsec: i32,
}

pub unsafe fn gettimeofday(tv: *mut TimeVal) -> usize {
    let ret: usize;
    asm!("int 0x80", inlateout("eax") SYS_GETTIMEOFDAY => ret, in("ebx") tv, options(nostack, preserves_flags));
    ret
}

pub unsafe fn clock_gettime(clock_id: i32, tp: *mut TimeSpec) -> usize {
    let ret: usize;
    asm!("int 0x80", inlateout("eax") SYS_CLOCK_GETTIME => ret, in("ebx") clock_id, in("ecx") tp, options(nostack, preserves_flags));
    ret
}

pub unsafe fn nanosleep(req: *const TimeSpec, rem: *mut TimeSpec) -> usize {
    let ret: usize;
    asm!("int 0x80", inlateout("eax") SYS_NANOSLEEP => ret, in("ebx") req, in("ecx") rem, options(nostack, preserves_flags));
    ret
}

pub unsafe fn tty_setfg(pgid: i32) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_TTY_SETFG => ret,
        in("ebx") pgid,
        options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn tty_getfg() -> i32 {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_TTY_GETFG => ret,
        options(nostack, preserves_flags)
    );
    ret as i32
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MountInfo {
    pub path: [u8; 96],
}

impl Default for MountInfo {
    fn default() -> Self { Self { path: [0; 96] } }
}

pub unsafe fn mount_list(out: *mut MountInfo, max: usize) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_MOUNT_LIST => ret,
        in("ebx") out,
        in("ecx") max,
        options(nostack, preserves_flags)
    );
    ret
}

#[repr(C)]
struct MountArgs {
    fstype: *const u8,
    flags: u32,
    data: *const u8,
}

pub unsafe fn mount(
    source: *const u8,
    target: *const u8,
    fstype: *const u8,
    flags: u32,
    data: *const u8,
) -> usize {
    // Keep the i386 syscall ABI to eax/ebx/ecx/edx. LLVM reserves ESI on
    // this target, so the less common mount arguments travel in one struct.
    let args = MountArgs { fstype, flags, data };
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_MOUNT => ret,
        in("ebx") source,
        in("ecx") target,
        in("edx") &args as *const MountArgs,
        options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn umount2(path: *const u8, flags: u32) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_UMOUNT2 => ret,
        in("ebx") path,
        in("ecx") flags,
        options(nostack, preserves_flags)
    );
    ret
}

/// Parameters for Felix-private in-memory spawn (passed via edx).
#[repr(C)]
pub struct ExecParams {
    pub stdin: i32,
    pub stdout: i32,
    pub stderr: i32,
    pub argc: u32,
    /// Array of `argc` pointers to C strings in the caller's address space.
    pub argv: *const *const u8,
    pub envc: u32,
    /// Array of `envc` pointers to `KEY=VALUE\0` strings.
    pub envp: *const *const u8,
    /// -1 = inherit parent's PGID, 0 = create group with child PID,
    /// >0 = atomically join that existing process group.
    pub pgid: i32,
    /// Non-zero asks the kernel to hand the controlling TTY to this PGID
    /// before the new task becomes observable to the scheduler.
    pub foreground: u32,
}

/// Replace the current process with an ELF loaded from `path`.
/// Returns only on error, as usize::MAX.
pub unsafe fn execve(path: *const u8, argv: *const *const u8, envp: *const *const u8) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_EXECVE => ret,
        in("ebx") path,
        in("ecx") argv,
        in("edx") envp,
        options(nostack, preserves_flags)
    );
    ret
}

/// Spawn a new task from an in-memory ELF image.
/// `argv` is a slice of C-string pointers (like Unix argv), including argv[0].
/// Returns the new task's pid (slot), or usize::MAX on failure.
///
/// ABI: ebx=buf, ecx=len, edx=*const ExecParams.
pub unsafe fn spawn(
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
        envc: 0,
        envp: core::ptr::null(),
        pgid: -1,
        foreground: 0,
    };
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_SPAWN => ret,
    in("ebx") buf,
    in("ecx") buf_size,
    in("edx") &params as *const ExecParams,
    options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn spawn_wasm(
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
        envc: 0,
        envp: core::ptr::null(),
        pgid: -1,
        foreground: 0,
    };
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_EXECVE_WASM => ret,
    in("ebx") buf,
    in("ecx") buf_size,
    in("edx") &params as *const ExecParams,
    options(nostack, preserves_flags)
    );
    ret
}

/// ELF exec with an explicit exported environment (`KEY=VALUE\0` strings).
pub unsafe fn spawn_env(
    buf: *const u8,
    buf_size: usize,
    stdin_fd: i32,
    stdout_fd: i32,
    stderr_fd: i32,
    argv: &[*const u8],
    envp: &[*const u8],
) -> usize {
    let params = ExecParams {
        stdin: stdin_fd,
        stdout: stdout_fd,
        stderr: stderr_fd,
        argc: argv.len() as u32,
        argv: argv.as_ptr(),
        envc: envp.len() as u32,
        envp: envp.as_ptr(),
        pgid: -1,
        foreground: 0,
    };
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_SPAWN => ret,
        in("ebx") buf,
        in("ecx") buf_size,
        in("edx") &params as *const ExecParams,
        options(nostack, preserves_flags)
    );
    ret
}

/// ELF exec with environment and atomic process-group placement.
pub unsafe fn spawn_env_pgid(
    buf: *const u8,
    buf_size: usize,
    stdin_fd: i32,
    stdout_fd: i32,
    stderr_fd: i32,
    argv: &[*const u8],
    envp: &[*const u8],
    pgid: i32,
    foreground: bool,
) -> usize {
    let params = ExecParams {
        stdin: stdin_fd,
        stdout: stdout_fd,
        stderr: stderr_fd,
        argc: argv.len() as u32,
        argv: argv.as_ptr(),
        envc: envp.len() as u32,
        envp: envp.as_ptr(),
        pgid,
        foreground: foreground as u32,
    };
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_SPAWN => ret,
        in("ebx") buf,
        in("ecx") buf_size,
        in("edx") &params as *const ExecParams,
        options(nostack, preserves_flags)
    );
    ret
}

/// WASM exec with an explicit exported environment.
pub unsafe fn spawn_wasm_env(
    buf: *const u8,
    buf_size: usize,
    stdin_fd: i32,
    stdout_fd: i32,
    stderr_fd: i32,
    argv: &[*const u8],
    envp: &[*const u8],
) -> usize {
    let params = ExecParams {
        stdin: stdin_fd,
        stdout: stdout_fd,
        stderr: stderr_fd,
        argc: argv.len() as u32,
        argv: argv.as_ptr(),
        envc: envp.len() as u32,
        envp: envp.as_ptr(),
        pgid: -1,
        foreground: 0,
    };
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_EXECVE_WASM => ret,
        in("ebx") buf,
        in("ecx") buf_size,
        in("edx") &params as *const ExecParams,
        options(nostack, preserves_flags)
    );
    ret
}

/// WASM exec with environment and atomic process-group placement.
pub unsafe fn spawn_wasm_env_pgid(
    buf: *const u8,
    buf_size: usize,
    stdin_fd: i32,
    stdout_fd: i32,
    stderr_fd: i32,
    argv: &[*const u8],
    envp: &[*const u8],
    pgid: i32,
    foreground: bool,
) -> usize {
    let params = ExecParams {
        stdin: stdin_fd,
        stdout: stdout_fd,
        stderr: stderr_fd,
        argc: argv.len() as u32,
        argv: argv.as_ptr(),
        envc: envp.len() as u32,
        envp: envp.as_ptr(),
        pgid,
        foreground: foreground as u32,
    };
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_EXECVE_WASM => ret,
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

/// Create a pseudo terminal pair. Writes [master_fd, slave_fd] to `fds` and
/// returns the PTY id, or usize::MAX on error.
pub unsafe fn openpty(fds: *mut u32) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_OPENPTY => ret,
        in("ebx") fds,
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
    in("ecx") core::ptr::null_mut::<i32>(),
    in("edx") options,
    options(nostack, preserves_flags)
    );
    ret
}

/// Wait for a child and receive its actual exit status.
pub unsafe fn waitpid_status(pid: i32, status: *mut i32, options: u32) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_WAIT => ret,
        in("ebx") pid,
        in("ecx") status,
        in("edx") options,
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

pub unsafe fn stat64(path: *const u8, st: *mut Stat64) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_STAT64 => ret,
        in("ebx") path,
        in("ecx") st,
        options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn fstat64(fd: u32, st: *mut Stat64) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_FSTAT64 => ret,
        in("ebx") fd,
        in("ecx") st,
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

pub const SYS_SOCKET: u32 = 359;
pub const SYS_BIND: u32 = 361;
pub const SYS_CONNECT: u32 = 362;
pub const SYS_LISTEN: u32 = 363;
pub const SYS_ACCEPT4: u32 = 364;
pub const SYS_SENDTO: u32 = 369;
pub const SYS_RECVFROM: u32 = 371;
pub const SYS_SHUTDOWN: u32 = 373;

// Window manager — must match kernel/src/syscalls/mod.rs
pub const SYS_WM_CREATE: u32 = 0xF100;
pub const SYS_WM_DESTROY: u32 = 0xF101;
pub const SYS_WM_MOVE: u32 = 0xF102;
pub const SYS_WM_INFO: u32 = 0xF103;
pub const SYS_WM_FLIP: u32 = 0xF104;
pub const SYS_WM_FOCUS: u32 = 0xF105;
pub const SYS_WM_SCREEN: u32 = 0xF106;
pub const SYS_MOUSE_STATE: u32 = 0xF107;
pub const SYS_WM_POLL: u32 = 0xF108;
pub const SYS_WM_WINDOWS: u32 = 0xF109;

/// pci_list(*mut PciInfo, max) → count written (or total if max=0)
pub const SYS_PCI_LIST: u32 = 0xF110;
pub const SYS_IFCONFIG: u32 = 0xF111;
pub const SYS_FB_INFO: u32 = 0xF112;
pub const SYS_FB_BLIT: u32 = 0xF113;
pub const SYS_TASK_LIST: u32 = 0xF115;
pub const SYS_OPENPTY: u32 = 0xF116;
pub const SYS_TTY_SETFG: u32 = 0xF117;
pub const SYS_TTY_GETFG: u32 = 0xF118;
pub const SYS_MOUNT_LIST: u32 = 0xF119;

pub const TASK_RUNNING: u8 = 0;
pub const TASK_STOPPED: u8 = 1;
pub const TASK_ZOMBIE: u8 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TaskInfo {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    pub sid: i32,
    pub state: u8,
    pub _pad: [u8; 3],
    pub exit_code: i32,
    pub tty_id: i16,
    pub _pad2: [u8; 2],
    pub name: [u8; 32],
    pub cwd: [u8; 128],
}

impl Default for TaskInfo {
    fn default() -> Self {
        Self {
            pid: 0,
            ppid: 0,
            pgid: 0,
            sid: 0,
            state: 0,
            _pad: [0; 3],
            exit_code: 0,
            tty_id: -1,
            _pad2: [0; 2],
            name: [0; 32],
            cwd: [0; 128],
        }
    }
}

pub unsafe fn task_list(out: *mut TaskInfo, max: usize) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_TASK_LIST => ret,
        in("ebx") out,
        in("ecx") max,
        options(nostack, preserves_flags)
    );
    ret
}

pub const IFCFG_GET: u32 = 0;
pub const IFCFG_STATIC: u32 = 1;
pub const IFCFG_DHCP: u32 = 2;

pub const IF_MODE_NONE: u32 = 0;
pub const IF_MODE_STATIC: u32 = 1;
pub const IF_MODE_DHCP: u32 = 2;

pub const IF_STATE_DOWN: u32 = 0;
pub const IF_STATE_CONFIGURING: u32 = 1;
pub const IF_STATE_UP: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct IfConfig {
    pub mode: u32,
    pub state: u32,
    pub ip: u32,
    pub prefix: u32,
    pub gateway: u32,
    pub dns: u32,
    pub mac: [u8; 6],
    pub _pad: [u8; 2],
}

/// One PCI function as returned by the kernel. Must match kernel `PciInfoUser`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PciInfo {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub _pad: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub interrupt_line: u8,
}

/// Enumerate PCI devices into `out` (up to `out.len()`). Returns number written.
/// If `out` is empty, returns total device count without writing.
pub unsafe fn ifconfig(cmd: u32, cfg: *mut IfConfig) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_IFCONFIG => ret,
        in("ebx") cmd,
        in("ecx") cfg,
        options(nostack, preserves_flags)
    );
    ret
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FbInfo {
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bpp: u32,
    pub virt: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FbBlit {
    pub src: *const u32,
    pub stride: u32,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

pub unsafe fn fb_info(out: *mut FbInfo) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_FB_INFO => ret,
        in("ebx") out,
        options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn fb_blit(arg: *const FbBlit) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_FB_BLIT => ret,
        in("ebx") arg,
        options(nostack, preserves_flags)
    );
    ret
}

pub unsafe fn pci_list(out: *mut PciInfo, max: usize) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_PCI_LIST => ret,
        in("ebx") out as u32,
        in("ecx") max,
        options(nostack, preserves_flags)
    );
    ret
}

#[repr(C)]
pub struct WmCreateArgs {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub title: *const u8,
    pub flags: *const u8
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

pub unsafe fn wm_create(x: i32, y: i32, w: u32, h: u32, title: *const u8, flags: *const u8) -> usize {
    let args = WmCreateArgs { x, y, w, h, title, flags};
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

/// Partial-flip descriptor. Pass its pointer to `wm_flip` with `len == usize::MAX`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WmFlipRect {
    pub x: u32, pub y: u32, pub w: u32, pub h: u32, pub pitch: u32,
    pub pixels: *const u8,
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
pub const EV_RESIZE: u32 = 9;

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

// Эта структура должна байт-в-байт совпадать с ядерной
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowListItem {
    pub id: u8,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub focused: u8,
    pub visible: u8,
    pub owner_slot: i8,
    pub title: [u8; 32],
}

/// Небезопасный системный вызов для получения списка окон.
/// Заполняет буфер `out` максимум `max` элементами. Возвращает реальное количество.
pub unsafe fn wm_get_window_list(out: *mut WindowListItem, max: usize) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_WM_WINDOWS => ret,
    in("ebx") out,
    in("ecx") max,
    options(nostack, preserves_flags)
    );
    ret
}
/// Безопасная обёртка. Возвращает Vec с актуальным списком окон.
pub fn get_window_list() -> Vec<WindowListItem> {
    // MAX_WINDOWS обычно равен 8, выделяем с запасом
    let mut list = Vec::with_capacity(8);

    unsafe {
        // Передаём сырой указатель на буфер и его вместимость
        let count = wm_get_window_list(list.as_mut_ptr(), list.capacity());

        // SAFETY: syscall гарантированно инициализировал `count` элементов,
        // и count <= capacity. Мы можем безопасно изменить длину вектора.
        list.set_len(count);
    }

    list
}


pub const AF_INET: u32 = 2;
pub const SOCK_STREAM: u32 = 1;
pub const SOCK_DGRAM: u32 = 2;
pub const IPPROTO_IP: u32 = 0;
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


pub unsafe fn sys_sleep(ms: u32) {
    let req = TimeSpec {
        tv_sec: (ms / 1000) as i32,
        tv_nsec: ((ms % 1000) * 1_000_000) as i32,
    };
    let _ = nanosleep(&req as *const TimeSpec, core::ptr::null_mut());
}
