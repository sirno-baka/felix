//! ATI Radeon Mobility M6 (RV100/M6) native LCD mode driver.
//!
//! Target panel from Sony VAIO PCG-C1MAH BIOS:
//!   1280x600 @ 65.00 MHz
//!   1280 1424 1536 1688
//!    600  630  636  640
//!
//! The BIOS timing block in the ROM gives:
//!   ref_div=8, post_div selector=2 (divide by 4), fb_div=77
//! which gives ~64.97 MHz from a 27 MHz reference.
//!
//! IMPORTANT: this driver intentionally preserves the BIOS-programmed
//! LVDS/FP state and only replaces the timing/PLL values needed for
//! the native panel mode.

use crate::memory::paging::{PAGING, PTEFlags};
use crate::pci::bar::Bar;
use crate::pci::find_device;
use core::ptr::{read_volatile, write_volatile};

const ATI_VENDOR: u16 = 0x1002;
const M6_LY_DEVICE: u16 = 0x4c59;

// Radeon MMIO registers.
const CLOCK_CNTL_INDEX: usize = 0x0008;
const CLOCK_CNTL_DATA: usize = 0x000c;
const CRTC_MORE_CNTL: usize = 0x027c;
const CRTC_GEN_CNTL: usize = 0x0050;
const CRTC_EXT_CNTL: usize = 0x0054;
const CRTC_H_TOTAL_DISP: usize = 0x0200;
const CRTC_H_SYNC_STRT_WID: usize = 0x0204;
const CRTC_V_TOTAL_DISP: usize = 0x0208;
const CRTC_V_SYNC_STRT_WID: usize = 0x020c;
const CRTC_OFFSET: usize = 0x0224;
const CRTC_OFFSET_CNTL: usize = 0x0228;
const CRTC_PITCH: usize = 0x022c;

const FP_CRTC_H_TOTAL_DISP: usize = 0x0250;
const FP_CRTC_V_TOTAL_DISP: usize = 0x0254;
const FP_HORZ_VERT_ACTIVE: usize = 0x0278;
const FP_GEN_CNTL: usize = 0x0284;
const FP_HORZ_STRETCH: usize = 0x028c;
const FP_VERT_STRETCH: usize = 0x0290;
const FP_H_SYNC_STRT_WID: usize = 0x02c4;
const FP_V_SYNC_STRT_WID: usize = 0x02c8;
const LVDS_GEN_CNTL: usize = 0x02d0;
const LVDS_PLL_CNTL: usize = 0x02d4;

// PLL indexed registers.
const PPLL_CNTL: u32 = 0x02;
const PPLL_REF_DIV: u32 = 0x03;
const PPLL_DIV_3: u32 = 0x07;
const VCLK_ECP_CNTL: u32 = 0x08;

// CRTC/FP bits.
const CRTC_HSYNC_DIS: u32 = 1 << 8;
const CRTC_VSYNC_DIS: u32 = 1 << 9;
const CRTC_DISPLAY_DIS: u32 = 1 << 10;

const CRTC_EXT_DISP_EN: u32 = 1 << 24;
const CRTC_EN: u32 = 1 << 25;

const CRTC_DBL_SCAN_EN: u32 = 1 << 0;
const CRTC_INTERLACE_EN: u32 = 1 << 1;

const FP_FPON: u32 = 1 << 0;
const FP_TMDS_EN: u32 = 1 << 2;
const FP_SEL_CRTC1: u32 = 0 << 13;
const FP_CRTC_DONT_SHADOW_VPAR: u32 = 1 << 16;
const FP_CRTC_DONT_SHADOW_HEND: u32 = 1 << 17;

const LVDS_ON: u32 = 1 << 0;
const LVDS_DISPLAY_DIS: u32 = 1 << 1;
const LVDS_EN: u32 = 1 << 7;
const LVDS_DIGON: u32 = 1 << 18;
const LVDS_BLON: u32 = 1 << 19;

// PPLL control bits used on R100/RV100/M6.
const PPLL_RESET: u32 = 1 << 0;
const PPLL_ATOMIC_UPDATE_EN: u32 = 1 << 1;
const PPLL_VGA_ATOMIC_UPDATE_EN: u32 = 1 << 2;
const VCLK_SRC_SEL_CPUCLK: u32 = 3 << 16;

// Native mode.
const XRES: u32 = 1280;
const YRES: u32 = 600;
const HTOTAL: u32 = 1688;
const HSYNC_START: u32 = 1424;
const HSYNC_END: u32 = 1536;
const VTOTAL: u32 = 640;
const VSYNC_START: u32 = 602;
const VSYNC_END: u32 = VSYNC_START;

// BIOS sync polarity for the native panel is active-low on the timing
// block; Radeon uses bit 23 to request negative polarity.
const HSYNC_NEG: u32 = 1 << 23;
const VSYNC_NEG: u32 = 1 << 23;

// BIOS PLL dividers from this machine's ROM.
const REF_DIV: u32 = 8;
const FB_DIV: u32 = 77;
const POST_DIV_SELECTOR: u32 = 2; // /4

#[derive(Clone, Copy)]
struct Mmio {
    base: *mut u8,
}

impl Mmio {
    #[inline]
    unsafe fn read32(&self, reg: usize) -> u32 {
        read_volatile(self.base.add(reg) as *const u32)
    }

    #[inline]
    unsafe fn write32(&self, reg: usize, value: u32) {
        write_volatile(self.base.add(reg) as *mut u32, value);
    }

    #[inline]
    unsafe fn read8(&self, reg: usize) -> u8 {
        read_volatile(self.base.add(reg) as *const u8)
    }

    #[inline]
    unsafe fn write8(&self, reg: usize, value: u8) {
        write_volatile(self.base.add(reg) as *mut u8, value);
    }
}

#[inline]
unsafe fn delay(count: usize) {
    for _ in 0..count {
        core::hint::spin_loop();
    }
}

unsafe fn pll_read(mmio: Mmio, index: u32) -> u32 {
    mmio.write8(CLOCK_CNTL_INDEX, (index & 0x3f) as u8);
    mmio.read32(CLOCK_CNTL_DATA)
}

unsafe fn pll_write(mmio: Mmio, index: u32, value: u32) {
    // Bit 7 requests a PLL write on Radeon R100.
    mmio.write8(CLOCK_CNTL_INDEX, ((index & 0x3f) | 0x80) as u8);
    mmio.write32(CLOCK_CNTL_DATA, value);
}

unsafe fn pll_write_mask(mmio: Mmio, index: u32, value: u32, mask: u32) {
    let old = pll_read(mmio, index);
    pll_write(mmio, index, (old & mask) | value);
}

fn find_mmio_bar(dev: &crate::pci::device::PciDevice) -> Option<(u32, u32)> {
    // Mobility M6 normally exposes a 16 MiB MMIO aperture and a framebuffer BAR.
    // Prefer the first memory BAR that is not the framebuffer-sized one.
    let mut first: Option<(u32, u32)> = None;

    for i in 0..6 {
        if let Bar::Memory {
            address,
            size,
            ..
        } = dev.bars[i]
        {
            if address == 0 || size == 0 {
                continue;
            }

            if first.is_none() {
                first = Some((address, size));
            }

            // MMIO aperture is normally much smaller than VRAM BAR.
            if size <= 0x0100_0000 {
                return Some((address, size));
            }
        }
    }

    first
}

/// Program the Sony PCG-C1MAH native panel mode.
///
/// This is intended to run after `drivers::framebuffer::init()`, while the
/// bootloader's VBE/LFB mode is still active. It keeps the existing LFB
/// physical address and existing pixel format.
pub fn init_native_lcd() -> Result<(), &'static str> {
    let dev = find_device(ATI_VENDOR, M6_LY_DEVICE).ok_or("ATI M6 LY not found")?;
    dev.enable_memory_space();

    let (mmio_phys, mmio_size) = find_mmio_bar(&dev).ok_or("ATI M6 MMIO BAR not found")?;

    // Map the Radeon MMIO aperture into the kernel's higher-half.
    // Keep this outside the normal framebuffer VA window.
    const MMIO_VIRT: u32 = 0xD100_0000;

    unsafe {
        let mut paging = PAGING.lock();
        paging
            .map_physical_range(
                mmio_phys,
                mmio_size.max(0x1000),
                MMIO_VIRT,
                PTEFlags::new().present().writable(),
            )
            .map_err(|_| "failed to map ATI MMIO")?;
        crate::memory::paging::PageDirectory::flush_all();
    }

    let mmio = Mmio {
        base: MMIO_VIRT as *mut u8,
    };

    unsafe {
        let crtc_gen_before = mmio.read32(CRTC_GEN_CNTL);
        let crtc_ext_before = mmio.read32(CRTC_EXT_CNTL);
        let fp_gen_before = mmio.read32(FP_GEN_CNTL);
        let lvds_before = mmio.read32(LVDS_GEN_CNTL);
        let pitch_before = mmio.read32(CRTC_PITCH);

        crate::println!(
            "[M6] MMIO={:#x} size={:#x} CRTC_GEN={:#x} CRTC_EXT={:#x} FP_GEN={:#x} LVDS={:#x} PITCH={:#x}",
            mmio_phys, mmio_size, crtc_gen_before, crtc_ext_before,
            fp_gen_before, lvds_before, pitch_before
        );

        // Keep the display engine enabled, but blank the output while changing
        // timings. This mirrors the old radeonfb mode-switch ordering.
        mmio.write32(
            CRTC_EXT_CNTL,
            crtc_ext_before | CRTC_DISPLAY_DIS | CRTC_HSYNC_DIS | CRTC_VSYNC_DIS,
        );
        mmio.write32(
            LVDS_GEN_CNTL,
            lvds_before | LVDS_DISPLAY_DIS,
        );
        delay(50_000);

        // Disable double-scan/interlace, enable display + CRTC.
        let mut gen2 = crtc_gen_before;
        gen2 &= !(CRTC_DBL_SCAN_EN | CRTC_INTERLACE_EN);
        gen2 |= CRTC_EXT_DISP_EN | CRTC_EN;
        mmio.write32(CRTC_GEN_CNTL, gen2);

        // CRTC timings.
        let crtc_h_total_disp =
            (((HTOTAL / 8 - 1) & 0x3ff) << 0) |
                (((XRES / 8 - 1) & 0x1ff) << 16);

        let hsync_wid = ((HSYNC_END - HSYNC_START) / 8).max(1).min(0x3f);
        let crtc_h_sync_strt_wid =
            ((HSYNC_START - 8) & 0x1fff) |
                (hsync_wid << 16) |
                HSYNC_NEG;

        let crtc_v_total_disp =
            ((VTOTAL - 1) & 0xffff) |
                ((YRES - 1) << 16);

        let vsync_wid = (VSYNC_END - VSYNC_START).max(1).min(0x1f);
        let crtc_v_sync_strt_wid =
            ((VSYNC_START - 1) & 0xfff) |
                (vsync_wid << 16) |
                VSYNC_NEG;

        mmio.write32(CRTC_H_TOTAL_DISP, crtc_h_total_disp);
        mmio.write32(CRTC_H_SYNC_STRT_WID, crtc_h_sync_strt_wid);
        mmio.write32(CRTC_V_TOTAL_DISP, crtc_v_total_disp);
        mmio.write32(CRTC_V_SYNC_STRT_WID, crtc_v_sync_strt_wid);

        // Keep the existing framebuffer base. The native ATI mode gets its own
        // display pitch below; the VBE pitch is not reused for CRTC_PITCH.
        let bytespp = match crate::drivers::framebuffer::FRAMEBUFFER
            .lock()
            .as_ref()
            .map(|fb| fb.info.bpp)
        {
            Some(16) => 2u32,
            Some(24) => 3u32,
            Some(32) => 4u32,
            Some(15) => 2u32,
            _ => 4u32,
        };

        // CRTC_PITCH is NOT a byte/64 value on Radeon.
        // Bits 0:9 are the display pitch in units of 8 PIXELS.
        //
        // For the native 1280-wide panel:
        //   1280 / 8 = 160
        //
        // The old VBE mode may have a smaller pitch (for example 640*4=2560),
        // so do not reuse that value for the native ATI mode.
        //
        // Only do this after we found the ATI M6 device above; machines without
        // this GPU never enter this function and keep their VBE pitch untouched.
        let pitch_pixels = XRES;
        let pitch_units = ((pitch_pixels + 7) / 8).min(0x3ff);

        // R100/M6 uses the same pitch value in both 16-bit halves.
        let pitch_reg = pitch_units | (pitch_units << 16);

        mmio.write32(CRTC_PITCH, pitch_reg);

        let pitch_bytes = pitch_pixels * bytespp;

        // Start from framebuffer offset 0.
        mmio.write32(CRTC_OFFSET, 0);
        mmio.write32(CRTC_OFFSET_CNTL, 0);

        // Native panel means no scaling. Program panel timings from the BIOS
        // timing record itself. For a native mode the panel and CRTC totals
        // are the same.
        mmio.write32(
            FP_CRTC_H_TOTAL_DISP,
            (((HTOTAL / 8) & 0x3ff) << 0) |
                (((XRES / 8 - 1) & 0x1ff) << 16),
        );
        mmio.write32(
            FP_CRTC_V_TOTAL_DISP,
            ((VTOTAL - 1) & 0xffff) |
                ((YRES - 1) << 16),
        );

        let fp_h =
            (HSYNC_START & 0x1fff) |
                (hsync_wid << 16) |
                HSYNC_NEG;
        let fp_v =
            (VSYNC_START & 0xfff) |
                (vsync_wid << 16) |
                VSYNC_NEG;

        mmio.write32(FP_H_SYNC_STRT_WID, fp_h);
        mmio.write32(FP_V_SYNC_STRT_WID, fp_v);
        mmio.write32(FP_HORZ_VERT_ACTIVE, (XRES - 1) | ((YRES - 1) << 16));

        // Native 1:1 panel: keep panel-size fields and disable RMX stretch.
        let mut fp_gen = fp_gen_before;
        fp_gen &= !(FP_TMDS_EN | (3 << 10));
        fp_gen |= FP_SEL_CRTC1 | FP_CRTC_DONT_SHADOW_VPAR | FP_CRTC_DONT_SHADOW_HEND;
        fp_gen |= FP_FPON;
        mmio.write32(FP_HORZ_STRETCH, ((XRES / 8 - 1) << 16) & 0x01ff0000);
        mmio.write32(FP_VERT_STRETCH, ((YRES - 1) << 12) & 0x00fff000);
        mmio.write32(FP_GEN_CNTL, fp_gen);

        // The ROM specifies the exact PPLL values for 65 MHz on this machine:
        // ref=27 MHz, ref_div=8, fb=77, post=2 (/4).
        //
        // Preserve the current PLL when already matching; Mobility parts can
        // momentarily blank if the divider is needlessly rewritten.
        let cur_ref = pll_read(mmio, PPLL_REF_DIV);
        let cur_div3 = pll_read(mmio, PPLL_DIV_3);
        let cur_fb = cur_div3 & 0x7ff;
        let cur_post = (cur_div3 >> 16) & 0x7;

        if (cur_ref & 0x3ff != REF_DIV || cur_fb != FB_DIV || cur_post != POST_DIV_SELECTOR)
        {
            // Feed VCLK from CPU clock while touching PPLL.
            pll_write_mask(
                mmio,
                VCLK_ECP_CNTL,
                VCLK_SRC_SEL_CPUCLK,
                !((3u32) << 16),
            );

            pll_write_mask(
                mmio,
                PPLL_CNTL,
                PPLL_RESET | PPLL_ATOMIC_UPDATE_EN | PPLL_VGA_ATOMIC_UPDATE_EN,
                !(PPLL_RESET | PPLL_ATOMIC_UPDATE_EN | PPLL_VGA_ATOMIC_UPDATE_EN),
            );

            pll_write_mask(mmio, PPLL_REF_DIV, REF_DIV, !0x3ff);
            pll_write_mask(
                mmio,
                PPLL_DIV_3,
                FB_DIV,
                !0x7ff,
            );
            pll_write_mask(
                mmio,
                PPLL_DIV_3,
                POST_DIV_SELECTOR << 16,
                !(0x7u32 << 16),
            );

            delay(20_000);

            // Release PPLL reset / keep atomic update enabled long enough for
            // the new dividers to latch.
            pll_write_mask(
                mmio,
                PPLL_CNTL,
                PPLL_ATOMIC_UPDATE_EN | PPLL_VGA_ATOMIC_UPDATE_EN,
                !PPLL_RESET & !(PPLL_ATOMIC_UPDATE_EN | PPLL_VGA_ATOMIC_UPDATE_EN),
            );
            delay(20_000);
        }

        // Restore output state while retaining the BIOS LVDS state. Always turn
        // the essential LVDS power/data path on, but do not clobber brightness
        // or panel-specific bits that the ROM configured.
        let mut lvds = lvds_before;
        lvds &= !LVDS_DISPLAY_DIS;
        lvds |= LVDS_ON | LVDS_EN | LVDS_DIGON | LVDS_BLON;
        mmio.write32(LVDS_GEN_CNTL, lvds);

        let mut ext = crtc_ext_before;
        ext &= !(CRTC_DISPLAY_DIS | CRTC_HSYNC_DIS | CRTC_VSYNC_DIS);
        mmio.write32(CRTC_EXT_CNTL, ext);

        delay(100_000);

        let h = mmio.read32(CRTC_H_TOTAL_DISP);
        let hs = mmio.read32(CRTC_H_SYNC_STRT_WID);
        let v = mmio.read32(CRTC_V_TOTAL_DISP);
        let vs = mmio.read32(CRTC_V_SYNC_STRT_WID);
        let pll_ref = pll_read(mmio, PPLL_REF_DIV);
        let pll_div3 = pll_read(mmio, PPLL_DIV_3);

        crate::println!(
            "[M6] native 1280x600 set: H={:#x} HS={:#x} V={:#x} VS={:#x} PLL(ref={:#x},div3={:#x}) pitch={}",
            h, hs, v, vs, pll_ref, pll_div3, pitch_bytes
        );

        // Sanity: if CRTC was not accepted, report instead of silently
        // continuing.
        if (h & 0x3ff) != ((HTOTAL / 8 - 1) & 0x3ff)
            || ((h >> 16) & 0x1ff) != ((XRES / 8 - 1) & 0x1ff)
        {
            crate::println!(
                "[M6] WARNING: CRTC_H_TOTAL_DISP mismatch (got {:#x})",
                h
            );
        }
        {
            let mut fb_guard = crate::drivers::framebuffer::FRAMEBUFFER.lock();

            if let Some(fb) = fb_guard.as_mut() {
                fb.info.width = XRES as u16;
                fb.info.height = YRES as u16;
                fb.info.pitch = pitch_bytes as u16;
            }
        }

        // Синхронизируем и копию framebuffer info в low memory.
        let info_ptr =
            crate::drivers::framebuffer::FB_INFO_PHYS
                as *mut crate::drivers::framebuffer::FramebufferInfo;

        let mut info = read_volatile(
            info_ptr as *const crate::drivers::framebuffer::FramebufferInfo
        );

        info.width = XRES as u16;
        info.height = YRES as u16;
        info.pitch = pitch_bytes as u16;

        write_volatile(info_ptr, info);

        crate::println!(
            "[M6] framebuffer updated: {}x{} {}bpp pitch={}",
            XRES,
            YRES,
            bytespp * 8,
            pitch_bytes
        );

    }
    Ok(())
}
