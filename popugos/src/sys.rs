#[cfg(target_os = "popugos")]
use core::arch::asm;

pub const SYS_WM_CREATE: u32 = 0xF100;
pub const SYS_WM_DESTROY: u32 = 0xF101;
pub const SYS_WM_MOVE: u32 = 0xF102;
pub const SYS_WM_INFO: u32 = 0xF103;
pub const SYS_WM_FLIP: u32 = 0xF104;
pub const SYS_WM_FOCUS: u32 = 0xF105;
pub const SYS_WM_SCREEN: u32 = 0xF106;
pub const SYS_WM_POLL: u32 = 0xF108;

#[repr(C)]
pub struct WmCreateArgs {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub title: *const u8,
    pub flags: *const u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RawWindowInfo {
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

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WmFlipRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub pitch: u32,
    pub pixels: *const u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RawWmEvent {
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
pub const EV_MOUSE_LEAVE: u32 = 10;
pub const EV_MOUSE_WHEEL: u32 = 11;

#[cfg(target_os = "popugos")]
pub unsafe fn wm_create(args: *const WmCreateArgs) -> usize {
    let ret: usize;
    asm!(
        "int 0x80",
        inlateout("eax") SYS_WM_CREATE => ret,
        in("ebx") args,
        options(nostack, preserves_flags)
    );
    ret
}

#[cfg(not(target_os = "popugos"))]
pub unsafe fn wm_create(_args: *const WmCreateArgs) -> usize { usize::MAX }

macro_rules! syscall_id {
    ($name:ident, $nr:ident) => {
        #[cfg(target_os = "popugos")]
        pub unsafe fn $name(id: u32) -> usize {
            let ret: usize;
            asm!(
                "int 0x80",
                inlateout("eax") $nr => ret,
                in("ebx") id,
                options(nostack, preserves_flags)
            );
            ret
        }
        #[cfg(not(target_os = "popugos"))]
        pub unsafe fn $name(_id: u32) -> usize { usize::MAX }
    };
}

syscall_id!(wm_destroy, SYS_WM_DESTROY);
syscall_id!(wm_focus, SYS_WM_FOCUS);

#[cfg(target_os = "popugos")]
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

#[cfg(not(target_os = "popugos"))]
pub unsafe fn wm_move(_id: u32, _x: i32, _y: i32) -> usize { usize::MAX }

#[cfg(target_os = "popugos")]
pub unsafe fn wm_info(id: u32, out: *mut RawWindowInfo) -> usize {
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

#[cfg(not(target_os = "popugos"))]
pub unsafe fn wm_info(_id: u32, _out: *mut RawWindowInfo) -> usize { usize::MAX }

#[cfg(target_os = "popugos")]
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

#[cfg(not(target_os = "popugos"))]
pub unsafe fn wm_flip(_id: u32, _pixels: *const u8, _len: usize) -> usize { usize::MAX }

#[cfg(target_os = "popugos")]
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

#[cfg(not(target_os = "popugos"))]
pub unsafe fn wm_screen_size(_out: *mut u32) -> usize { usize::MAX }

#[cfg(target_os = "popugos")]
pub unsafe fn wm_poll(id: u32, out: *mut RawWmEvent, max: usize) -> usize {
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

#[cfg(not(target_os = "popugos"))]
pub unsafe fn wm_poll(_id: u32, _out: *mut RawWmEvent, _max: usize) -> usize { 0 }
