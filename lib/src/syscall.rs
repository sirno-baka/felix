use core::arch::asm;

pub const SYS_EXIT:  u32 = 1;
pub const SYS_WRITE: u32 = 4;
pub const SYS_MALLOC: u32 = 200;
pub const SYS_FREE:  u32 = 201;

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

pub unsafe fn exit() -> ! {
    asm!("int 0x80", in("eax") SYS_EXIT, options(noreturn));
    unreachable!()
}