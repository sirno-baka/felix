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
pub const SYS_WAIT:   u32 = 114;
pub const SYS_PIPE:   u32 = 42;
pub const SYS_DUP2:   u32 = 63;

// open flags
pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR:   u32 = 2;
pub const O_CREAT:  u32 = 0x40;
pub const O_TRUNC:  u32 = 0x200;
pub const O_APPEND: u32 = 0x400;

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

/// Spawn a new task from an in-memory ELF image.
/// `stdin_fd`/`stdout_fd`/`stderr_fd`: parent fds to map as child's 0/1/2, or -1 for console.
/// Returns the new task's pid (slot), or usize::MAX on failure.
///
/// ABI: ebx=buf, ecx=len, edx=*const [i32; 3] (stdin, stdout, stderr).
/// esi/edi are not used — LLVM reserves them on this target.
pub unsafe fn execve(
    buf: *const u8,
    buf_size: usize,
    stdin_fd: i32,
    stdout_fd: i32,
    stderr_fd: i32,
) -> usize {
    let fdmap: [i32; 3] = [stdin_fd, stdout_fd, stderr_fd];
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_EXECVE => ret,
    in("ebx") buf,
    in("ecx") buf_size,
    in("edx") fdmap.as_ptr(),
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
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_WAIT => ret,
    in("ebx") pid,
    options(nostack, preserves_flags)
    );
    ret
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