#![allow(dead_code)]

use core::arch::asm;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct FramebufferInfo {
    pub address: u32,
    pub pitch: u16,
    pub width: u16,
    pub height: u16,
    pub bpp: u8,
    pub reserved: [u8; 3],
}

pub const FB_INFO_PHYS: u32 = 0x0000_5000;

#[repr(C, packed)]
struct VbeInfoBlock {
    signature: [u8; 4],
    version: u16,
    oem_string_ptr: u32,
    capabilities: u32,
    video_mode_ptr: u32,
    total_memory: u16,
    reserved: [u8; 512 - 20],
}

#[repr(C, packed)]
struct ModeInfoBlock {
    attributes: u16,
    win_a: u8,
    win_b: u8,
    granularity: u16,
    winsize: u16,
    segment_a: u16,
    segment_b: u16,
    win_func_ptr: u32,
    pitch: u16,
    width: u16,
    height: u16,
    w_char: u8,
    y_char: u8,
    planes: u8,
    bpp: u8,
    banks: u8,
    memory_model: u8,
    bank_size: u8,
    image_pages: u8,
    reserved0: u8,
    red_mask: u8,
    red_position: u8,
    green_mask: u8,
    green_position: u8,
    blue_mask: u8,
    blue_position: u8,
    reserved_mask: u8,
    reserved_position: u8,
    direct_color_attributes: u8,
    framebuffer: u32,
    offscreen_offset: u32,
    offscreen_size: u16,
    lin_pitch: u16,
    reserved1: [u8; 204],
}

fn min_pitch(w: u16, bpp: u8) -> u16 {
    w.saturating_mul(((bpp as u16) + 7) / 8)
}

fn take_pitch(banked: u16, linear: u16, w: u16, bpp: u8) -> u16 {
    let min = min_pitch(w, bpp);
    let mut p = banked;
    if linear >= min {
        p = p.max(linear);
    }
    if p < min {
        min
    } else {
        p
    }
}

pub unsafe fn init_vesa() -> bool {
    let vbe = 0x6000 as *mut VbeInfoBlock;
    let mi = 0x6200 as *mut ModeInfoBlock;
    core::ptr::write_bytes(vbe as *mut u8, 0, 512);
    core::ptr::write_bytes(mi as *mut u8, 0, 256);
    core::ptr::copy_nonoverlapping(b"VBE2".as_ptr(), vbe as *mut u8, 4);

    let mut ax: u16;
    asm!(
        "push es",
        "mov es, {es:x}",
        "int 0x10",
        "pop es",
        es = in(reg) 0u16,
        inout("ax") 0x4F00u16 => ax,
        in("di") vbe as u16,
        options(nostack)
    );

    let mut wide_mode: u16 = 0;
    let mut wide_score: u32 = 0;
    let mut safe_mode: u16 = 0;
    let mut safe_score: u32 = 0;
    let mut best = FramebufferInfo {
        address: 0,
        pitch: 0,
        width: 0,
        height: 0,
        bpp: 0,
        reserved: [0; 3],
    };

    if ax == 0x004F {
        let seg = ((*vbe).video_mode_ptr >> 16) as u16;
        let mut off = (*vbe).video_mode_ptr as u16;
        loop {
            let mode: u16;
            asm!(
                "push es",
                "mov es, {seg:x}",
                "mov di, {off:x}",
                "mov ax, es:[di]",
                "pop es",
                seg = in(reg) seg,
                off = in(reg) off,
                lateout("ax") mode,
                options(nostack)
            );
            if mode == 0xFFFF {
                break;
            }
            off = off.wrapping_add(2);

            let mut st: u16;
            asm!(
                "push es",
                "mov es, {es:x}",
                "int 0x10",
                "pop es",
                es = in(reg) 0u16,
                inout("ax") 0x4F01u16 => st,
                in("cx") mode,
                in("di") mi as u16,
                options(nostack)
            );
            if st != 0x004F {
                continue;
            }

            let attr = (*mi).attributes;
            let w = (*mi).width;
            let h = (*mi).height;
            let bpp = (*mi).bpp;
            let fb = (*mi).framebuffer;
            if (attr & (1 << 4)) == 0 {
                continue;
            }
            if bpp != 16 && bpp != 24 && bpp != 32 {
                continue;
            }
            if w < 640 || h < 400 || w > 1600 || h > 900 {
                continue;
            }

            let mut s = w as u32 * h as u32;
            match bpp {
                32 => s += 3_000_000,
                24 => s += 1_000_000,
                _ => s += 400_000,
            }
            if fb != 0 {
                s += 10_000;
            }
            // Pitch is often 0 until 4F02 on OEM ATI. Still try native-wide modes.
            let wide = w >= 1280 && h >= 560 && h <= 800;
            if wide && s > wide_score {
                wide_score = s;
                wide_mode = mode;
            }
            if w == 800 && h == 600 && s > safe_score {
                safe_score = s;
                safe_mode = mode;
            }
        }
    }

    if wide_mode != 0 && set_mode(wide_mode, mi, &mut best) && best.width >= 1280
    {
        core::ptr::write_volatile(FB_INFO_PHYS as *mut FramebufferInfo, best);
        return true;
    }
    if safe_mode != 0 && set_mode(safe_mode, mi, &mut best) && best.width >= 800 {
        core::ptr::write_volatile(FB_INFO_PHYS as *mut FramebufferInfo, best);
        return true;
    }

    let hard: [u16; 4] = [0x115, 0x114, 0x118, 0x112];
    for m in hard {
        if set_mode(m, mi, &mut best) {
            core::ptr::write_volatile(FB_INFO_PHYS as *mut FramebufferInfo, best);
            return true;
        }
    }
    false
}


unsafe fn set_mode(mode: u16, mi: *mut ModeInfoBlock, out: &mut FramebufferInfo) -> bool {
    let mut st: u16;
    asm!(
        "push es",
        "mov es, {es:x}",
        "int 0x10",
        "pop es",
        es = in(reg) 0u16,
        inout("ax") 0x4F01u16 => st,
        in("cx") mode,
        in("di") mi as u16,
        options(nostack)
    );
    if st != 0x004F {
        return false;
    }

    // Same as the working tree: always request LFB, ignore AH.
    let bx = mode | 0x4000;
    asm!(
        "int 0x10",
        inout("ax") 0x4F02u16 => st,
        in("bx") bx,
        options(nostack)
    );

    asm!(
        "push es",
        "mov es, {es:x}",
        "int 0x10",
        "pop es",
        es = in(reg) 0u16,
        inout("ax") 0x4F01u16 => st,
        in("cx") mode,
        in("di") mi as u16,
        options(nostack)
    );

    let w = (*mi).width;
    let h = (*mi).height;
    let bpp = (*mi).bpp;
    let fb = (*mi).framebuffer;
    let p = take_pitch((*mi).pitch, (*mi).lin_pitch, w, bpp);
    if w == 0 || h == 0 {
        return false;
    }
    if fb == 0 || w == 0 || h == 0 {
        return false;
    }
    // Reject only an explicitly too-small nonzero pitch (stripes).
    // Zero pitch is filled by take_pitch() as width*bpp.
    let reported = if (*mi).lin_pitch != 0 {
        (*mi).lin_pitch
    } else {
        (*mi).pitch
    };
    if w >= 1280 && reported != 0 && reported < min_pitch(w, bpp) {
        return false;
    }
    out.address = fb;
    out.pitch = p;
    out.width = w;
    out.height = h;
    out.bpp = bpp;
    true
}
