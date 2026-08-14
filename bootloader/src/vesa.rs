#![allow(dead_code)]

use core::arch::asm;
use core::ptr;

/// Информация о фреймбуфере, которую мы передаём ядру
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct FramebufferInfo {
    pub address: u32,   // PhysBasePtr
    pub pitch: u16,     // bytes per scanline
    pub width: u16,
    pub height: u16,
    pub bpp: u8,        // bits per pixel
    pub reserved: [u8; 3],
}

/// Фиксированный адрес, куда кладём структуру (identity-mapped)
pub const FB_INFO_PHYS: u32 = 0x0000_5000;

#[repr(C, packed)]
struct VbeInfoBlock {
    signature: [u8; 4],     // "VESA"
    version: u16,
    oem_string_ptr: u32,
    capabilities: u32,
    video_mode_ptr: u32,    // far ptr
    total_memory: u16,
    reserved: [u8; 512 - 20],
}

#[repr(C, packed)]
struct ModeInfoBlock {
    attributes: u16,          // 0x00
    win_a: u8,                // 0x02
    win_b: u8,                // 0x03
    granularity: u16,         // 0x04
    winsize: u16,             // 0x06
    segment_a: u16,           // 0x08
    segment_b: u16,           // 0x0A
    win_func_ptr: u32,        // 0x0C
    pitch: u16,               // 0x10  BytesPerScanLine
    width: u16,               // 0x12
    height: u16,              // 0x14
    w_char: u8,               // 0x16
    y_char: u8,               // 0x17
    planes: u8,               // 0x18
    bpp: u8,                  // 0x19  ← BitsPerPixel
    banks: u8,                // 0x1A
    memory_model: u8,         // 0x1B
    bank_size: u8,            // 0x1C
    image_pages: u8,          // 0x1D
    reserved0: u8,            // 0x1E
    red_mask: u8,             // 0x1F
    red_position: u8,         // 0x20
    green_mask: u8,           // 0x21
    green_position: u8,       // 0x22
    blue_mask: u8,            // 0x23
    blue_position: u8,        // 0x24
    reserved_mask: u8,        // 0x25
    reserved_position: u8,    // 0x26
    direct_color_attributes: u8, // 0x27
    framebuffer: u32,         // 0x28  PhysBasePtr
    // дальше нам не нужно
    reserved1: [u8; 212],
}

pub unsafe fn init_vesa() -> bool {
    let vbe_info = 0x6000 as *mut VbeInfoBlock;
    let mode_info = 0x6200 as *mut ModeInfoBlock;

    // Обнуляем буферы
    core::ptr::write_bytes(vbe_info as *mut u8, 0, 512);
    core::ptr::write_bytes(mode_info as *mut u8, 0, 256);

    // Для полноценного VBE 2.0+ ставим сигнатуру
    core::ptr::copy_nonoverlapping(b"VBE2".as_ptr(), vbe_info as *mut u8, 4);

    // --- Get VBE Controller Info (4F00) ---
    let mut ret: u16;
    asm!(
    "push es",
    "mov es, {es:x}",
    "int 0x10",
    "pop es",
    es = in(reg) 0u16,
    inout("ax") 0x4F00u16 => ret,
    in("di") vbe_info as u16,
    options(nostack)
    );
    return try_hardcoded_modes(mode_info);
    if ret != 0x004F {
        // Даже если VBE Info не получен — пробуем жёсткие режимы
        return try_hardcoded_modes(mode_info);
    }

    // --- Перебор режимов ---
    let mode_list_seg = ((*vbe_info).video_mode_ptr >> 16) as u16;
    let mut mode_list_off = (*vbe_info).video_mode_ptr as u16;

    let mut best_mode: u16 = 0;
    let mut best_score: u32 = 0;
    let mut best = FramebufferInfo {
        address: 0,
        pitch: 0,
        width: 0,
        height: 0,
        bpp: 0,
        reserved: [0; 3],
    };

    loop {
        // Читаем следующий mode number из списка (far pointer)
        let mode: u16;
        asm!(
        "push es",
        "mov es, {seg:x}",
        "mov di, {off:x}",
        "mov ax, es:[di]",
        "pop es",
        seg = in(reg) mode_list_seg,
        off = in(reg) mode_list_off,
        lateout("ax") mode,
        options(nostack)
        );

        if mode == 0xFFFF {
            break;
        }

        // --- Get Mode Info (4F01) ---
        let mut status: u16;
        asm!(
        "push es",
        "mov es, {es:x}",
        "int 0x10",
        "pop es",
        es = in(reg) 0u16,
        inout("ax") 0x4F01u16 => status,
        in("cx") mode,
        in("di") mode_info as u16,
        options(nostack)
        );

        if status != 0x004F {
            mode_list_off = mode_list_off.wrapping_add(2);
            continue;
        }

        let attr = (*mode_info).attributes;
        let width = (*mode_info).width;
        let height = (*mode_info).height;
        let bpp = (*mode_info).bpp;
        let fb = (*mode_info).framebuffer;
        let pitch = (*mode_info).pitch;

        // Более мягкая проверка (многие карты не ставят все биты)
        let is_graphics = (attr & (1 << 4)) != 0;
        let is_color = (attr & (1 << 3)) != 0;
        // let has_lfb = (attr & (1 << 7)) != 0;

        if !is_graphics || !is_color {
            mode_list_off = mode_list_off.wrapping_add(2);
            continue;
        }

        // // Принимаем даже без LFB-бита, если адрес ненулевой
        // if fb == 0 || width < 640 || height < 480 || bpp < 16 {
        //     mode_list_off = mode_list_off.wrapping_add(2);
        //     continue;
        // }

        let score = (width as u32 * height as u32)
            + if bpp >= 32 { 5_000_000 } else { 0 }
            + if bpp == 24 { 2_000_000 } else { 0 }
            + if bpp == 16 { 500_000 } else { 0 };

        if score > best_score {
            best_score = score;
            best_mode = mode;
            best = FramebufferInfo {
                address: fb,
                pitch,
                width,
                height,
                bpp,
                reserved: [0; 3],
            };
        }

        mode_list_off = mode_list_off.wrapping_add(2);
    }

    // Если ничего не нашли — пробуем жёсткие режимы
    // if best_mode == 0 || best.address == 0 {
    //     return try_hardcoded_modes(mode_info);
    // }

    // --- Устанавливаем найденный режим ---
    if !set_mode(best_mode, mode_info, &mut best) {
        return try_hardcoded_modes(mode_info);
    }

    // Сохраняем
    core::ptr::write_volatile(FB_INFO_PHYS as *mut FramebufferInfo, best);
    true
}

/// Пробует известные рабочие режимы (особенно для QEMU)
unsafe fn try_hardcoded_modes(mode_info: *mut ModeInfoBlock) -> bool {
    // Порядок от более предпочтительных к менее
    let candidates: [u16; 7] = [
        0x114, // 800x600x16
        0x117, // 1024x768x16

        0x115, // 800x600x24

        0x118, // 1024x768x24/32  (самый частый в QEMU)
        0x11A, // 1280x1024x16
        0x11B, // 1280x1024x24
        0x112, // 640x480x24
    ];

    for &mode in &candidates {
        let mut info = FramebufferInfo {
            address: 0,
            pitch: 0,
            width: 0,
            height: 0,
            bpp: 0,
            reserved: [0; 3],
        };

        if set_mode(mode, mode_info, &mut info) && info.address != 0 {
            core::ptr::write_volatile(FB_INFO_PHYS as *mut FramebufferInfo, info);
            return true;
        }
    }

    false
}

/// Устанавливает режим и заполняет info
unsafe fn set_mode(mode: u16, mode_info: *mut ModeInfoBlock, out: &mut FramebufferInfo) -> bool {
    // --- Get Mode Info (4F01) ---
    let mut status: u16;
    asm!(
    "push es",
    "mov es, {es:x}",
    "int 0x10",
    "pop es",
    es = in(reg) 0u16,
    inout("ax") 0x4F01u16 => status,
    in("cx") mode,
    in("di") mode_info as u16,
    options(nostack)
    );

    if status != 0x004F {
        return false;
    }

    // Всегда пробуем с LFB-битом
    let mode_lfb = mode | 0x4000;

    // --- Set Mode (4F02) ---
    asm!(
    "int 0x10",
    inout("ax") 0x4F02u16 => status,
    in("bx") mode_lfb,
    options(nostack)
    );

    // if status != 0x004F {
    //     // Если с LFB не получилось — пробуем без него
    //     asm!(
    //         "int 0x10",
    //         inout("ax") 0x4F02u16 => status,
    //         in("bx") mode,
    //         options(nostack)
    //     );
    // }

    // Ещё раз читаем info после установки (на всякий случай)
    asm!(
    "push es",
    "mov es, {es:x}",
    "int 0x10",
    "pop es",
    es = in(reg) 0u16,
    inout("ax") 0x4F01u16 => status,
    in("cx") mode,
    in("di") mode_info as u16,
    options(nostack)
    );

    out.address = (*mode_info).framebuffer;
    out.pitch = (*mode_info).pitch;
    out.width = (*mode_info).width;
    out.height = (*mode_info).height;
    out.bpp = (*mode_info).bpp;

    // Иногда pitch = 0, тогда считаем сами
    if out.pitch == 0 && out.width != 0 && out.bpp != 0 {
        out.pitch = out.width * ((out.bpp as u16 + 7) / 8);
    }

    out.address != 0 && out.width != 0
}
