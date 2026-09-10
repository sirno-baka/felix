mod alloc;
pub mod handler;
pub mod wasm;

pub const SYS_EXIT: u32 = 1;
pub const SYS_READ: u32 = 3;
pub const SYS_WRITE: u32 = 4;
pub const SYS_OPEN: u32 = 5; // open(filename) → fd или -1
pub const SYS_CLOSE: u32 = 6; // close(fd)
pub const SYS_MKDIR: u32 = 39;
pub const SYS_RMDIR: u32 = 40;
pub const SYS_UNLINK: u32 = 10; // delete/unlink(filename)
pub const SYS_CHDIR: u32 = 12;

pub const SYS_EXECVE: u32 = 11;
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
pub const SYS_RENAME: u32 = 38;
/// sigaction(sig, act, oldact) — set/get signal handler. 0 on success.
pub const SYS_SIGACTION: u32 = 67;
/// Linux i386 waitpid(pid, status, options). options: WNOHANG=1.
/// Returns pid of the reaped child, 0 if WNOHANG and none ready, or usize::MAX on error.
pub const SYS_WAIT: u32 = 7;
/// pipe(pipefd: *mut u32) — writes [read_fd, write_fd], returns 0 or usize::MAX
pub const SYS_PIPE: u32 = 42;
/// dup2(oldfd, newfd) → newfd or usize::MAX
pub const SYS_DUP2: u32 = 63;
/// fcntl(fd, cmd, arg) — F_GETFL=3, F_SETFL=4
pub const SYS_FCNTL: u32 = 55;
/// poll(fds, nfds, timeout_ms) — timeout -1 = block, 0 = nonblock
pub const SYS_POLL: u32 = 168;

// Felix-private ABI. Keep it away from the Linux i386 syscall namespace.
pub const SYS_SPAWN: u32 = 0xF000;
pub const SYS_EXECVE_WASM: u32 = 0xF001;
pub const SYS_MALLOC: u32 = 0xF010;
pub const SYS_FREE: u32 = 0xF011;
pub const SYS_REALLOC: u32 = 0xF012;
pub const SYS_LS: u32 = 0xF013;

// Socket syscalls (Linux i386 numbers)
pub const SYS_SOCKET: u32 = 359;
pub const SYS_SOCKETPAIR: u32 = 360;
pub const SYS_BIND: u32 = 361;
pub const SYS_CONNECT: u32 = 362;
pub const SYS_LISTEN: u32 = 363;
pub const SYS_ACCEPT4: u32 = 364;
pub const SYS_GETSOCKOPT: u32 = 365;
pub const SYS_SETSOCKOPT: u32 = 366;
pub const SYS_GETSOCKNAME: u32 = 367;
pub const SYS_GETPEERNAME: u32 = 368;
pub const SYS_SENDTO: u32 = 369;
pub const SYS_SENDMSG: u32 = 370;
pub const SYS_RECVFROM: u32 = 371;
pub const SYS_RECVMSG: u32 = 372;
pub const SYS_SHUTDOWN: u32 = 373;

// Window manager (kernel compositor)
/// create(x, y, w, h, title_ptr) → window_id or usize::MAX
pub const SYS_WM_CREATE: u32 = 0xF100;
/// destroy(id) → 0 / usize::MAX
pub const SYS_WM_DESTROY: u32 = 0xF101;
/// move(id, x, y) → 0 / usize::MAX
pub const SYS_WM_MOVE: u32 = 0xF102;
/// info(id, *mut WindowInfo) → 0 / usize::MAX
pub const SYS_WM_INFO: u32 = 0xF103;
/// flip(id, user_pixels, len) → 0 / usize::MAX  (copy + compose)
pub const SYS_WM_FLIP: u32 = 0xF104;
/// focus(id) → 0 / usize::MAX
pub const SYS_WM_FOCUS: u32 = 0xF105;
/// screen_size(*mut u32 /*w,h*/) → 0
pub const SYS_WM_SCREEN: u32 = 0xF106;
/// mouse_state(*mut MouseState) → 0 / usize::MAX
pub const SYS_MOUSE_STATE: u32 = 0xF107;
/// wm_poll(id, *mut WmEvent, max) → number of events copied
pub const SYS_WM_POLL: u32 = 0xF108;
pub const SYS_WM_WINDOWS: u32 = 0xF109;
/// pci_list(*mut PciInfoUser, max) → number of devices written (or needed if max=0)
pub const SYS_PCI_LIST: u32 = 0xF110;
/// ifconfig(cmd, *mut IfConfigUser) → 0 / usize::MAX
/// cmd: 0=get, 1=static, 2=dhcp
pub const SYS_IFCONFIG: u32 = 0xF111;
/// fb_info(*mut FbInfoUser) → 0 / usize::MAX
pub const SYS_FB_INFO: u32 = 0xF112;
/// fb_blit(*const FbBlit) → 0 / usize::MAX — copy a rect from user shadow to LFB
pub const SYS_FB_BLIT: u32 = 0xF113;
/// task_list(*mut TaskInfoUser, max) -> number of task records written/required
pub const SYS_TASK_LIST: u32 = 0xF115;
/// openpty(*mut [master,slave]) -> pty id / usize::MAX
pub const SYS_OPENPTY: u32 = 0xF116;
/// pty_set_fg(pgid) / pty_get_fg(), keyed by the caller's controlling tty.
pub const SYS_TTY_SETFG: u32 = 0xF117;
pub const SYS_TTY_GETFG: u32 = 0xF118;
/// mount_list(*mut MountInfoUser,max) -> count
pub const SYS_MOUNT_LIST: u32 = 0xF119;
