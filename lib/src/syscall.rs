use core::arch::asm;

pub const SYS_EXIT:   u32 = 1;
pub const SYS_OPEN:   u32 = 2;
pub const SYS_READ:   u32 = 3;
pub const SYS_WRITE:  u32 = 4;
pub const SYS_CLOSE:  u32 = 5;

pub const SYS_MKDIR:  u32 = 10;
pub const SYS_RMDIR:  u32 = 11;
pub const SYS_UNLINK: u32 = 12;
pub const SYS_EXECVE: u32 = 13;

pub const SYS_MALLOC: u32 = 200;
pub const SYS_FREE:   u32 = 201;
pub const SYS_REALLOC: u32 = 202;
// ====================== WRAPPERS ======================

pub unsafe fn exit() -> ! {
    asm!("int 0x80", in("eax") SYS_EXIT, options(noreturn));
    unreachable!()
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

pub unsafe fn execve(path: *const u8) -> usize {
    let ret: usize;
    asm!(
    "int 0x80",
    inlateout("eax") SYS_EXECVE => ret,
    in("ebx") path,
    options(nostack, preserves_flags)
    );
    ret
}
