//! Minimal Intel Sandy Bridge (Gen6) display takeover for ThinkPad X220-class
//! machines.
//!
//! Goal: keep the BIOS-programmed clock/FDI/LVDS timing, but replace the
//! low-resolution VBE scanout with a native-size 32-bpp framebuffer backed by
//! normal RAM through the Gen6 GGTT.
//!
//! This is intentionally *not* a full i915/KMS driver.  It does not touch the
//! DPLL, FDI link training, PCH transcoder, LVDS power sequencing or backlight.
//! If the firmware did not already program a native panel timing, we refuse to
//! guess and leave the VBE framebuffer alone.

use crate::drivers::framebuffer::{
    FB_INFO_PHYS, FRAMEBUFFER, Framebuffer, FramebufferInfo, current_lfb_virt,
    map_framebuffer_resource,
};
use crate::memory::resources::{DmaBuffer, ResourceKind, dma_alloc_for, reserve_and_ioremap};
use crate::pci::bar::Bar;
use crate::pci::device::PciDevice;
use crate::sync::mutex::Mutex;
use core::ptr::{read_volatile, write_bytes, write_volatile};

const INTEL_VENDOR: u16 = 0x8086;

// Sandy Bridge PCI IDs from the upstream i915 PCI ID table.
const SNB_IDS: &[u16] = &[
    0x0102, 0x010a, // desktop GT1
    0x0112, 0x0122, // desktop GT2
    0x0106, // mobile GT1
    0x0116, 0x0126, // mobile GT2 (X220 normally 0x0126)
];

// Gen4+ PCI BAR layout, also used by Gen6:
//   BAR0 = GTTMMADR (MMIO first, GGTT PTE window starting at +2 MiB)
//   BAR2 = GMADR CPU-visible graphics aperture
const GTTMMADR_BAR: usize = 0;
const GMADR_BAR: usize = 2;
const GGTT_PTE_WINDOW: usize = 0x0020_0000;

// Display / GGTT registers (Sandy Bridge PRM / i915 register definitions).
const GFX_FLSH_CNTL_GEN6: usize = 0x101008;
const GFX_FLSH_CNTL_EN: u32 = 1;

const CPU_VGACNTRL: usize = 0x41000;
const VGA_DISP_DISABLE: u32 = 1 << 31;

const PFIT_CONTROL: usize = 0x61230;
const PFIT_ENABLE: u32 = 1 << 31;

const PIPEDSL_A: usize = 0x70000;
const PIPECONF_A: usize = 0x70008;
const HTOTAL_A: usize = 0x60000;
const VTOTAL_A: usize = 0x6000c;
const PIPESRC_A: usize = 0x6001c;

const DSPCNTR_A: usize = 0x70180;
const DSPLINOFF_A: usize = 0x70184;
const DSPSTRIDE_A: usize = 0x70188;
const DSPSURF_A: usize = 0x7019c;
const DSPTILEOFF_A: usize = 0x701a4;
const DSPSURFLIVE_A: usize = 0x701ac;

const PIPE_STRIDE: usize = 0x1000;

const PIPECONF_ENABLE: u32 = 1 << 31;
const PIPECONF_STATE_ENABLE: u32 = 1 << 30;

const DISP_ENABLE: u32 = 1 << 31;
const DISP_FORMAT_MASK: u32 = 0xf << 26;
const DISP_FORMAT_BGRX888: u32 = 6 << 26;
const DISP_ROTATE_180: u32 = 1 << 15;
const DISP_TILED: u32 = 1 << 10;

// Gen6 GGTT PTE bits.  The kernel currently keeps physical RAM below 1 GiB, so
// the high-address encoding is normally zero, but keep the proper encoding for
// completeness.
const GEN6_PTE_CACHE_LLC: u32 = 2 << 1;
const GEN6_PTE_VALID: u32 = 1;

static INTEL_FB_DMA: Mutex<Option<DmaBuffer>> = Mutex::new(None);

const PAGE_SIZE: u32 = 4096;
const LARGE_PAGE: u32 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pipe {
    A,
    B,
}

impl Pipe {
    #[inline]
    fn delta(self) -> usize {
        match self {
            Pipe::A => 0,
            Pipe::B => PIPE_STRIDE,
        }
    }

    #[inline]
    fn name(self) -> char {
        match self {
            Pipe::A => 'A',
            Pipe::B => 'B',
        }
    }
}

#[derive(Clone, Copy)]
struct Mmio {
    base: *mut u8,
}

impl Mmio {
    #[inline]
    unsafe fn read32(self, reg: usize) -> u32 {
        read_volatile(self.base.add(reg) as *const u32)
    }

    #[inline]
    unsafe fn write32(self, reg: usize, value: u32) {
        write_volatile(self.base.add(reg) as *mut u32, value);
    }
}

#[derive(Clone, Copy)]
struct PipeState {
    pipe: Pipe,
    enabled: bool,
    state_enabled: bool,
    plane_enabled: bool,
    active_width: u32,
    active_height: u32,
    source_width: u32,
    source_height: u32,
    plane_cntr: u32,
    plane_stride: u32,
    plane_surface: u32,
    plane_surface_live: u32,
}

#[inline]
fn align_up(value: u32, align: u32) -> u32 {
    value.wrapping_add(align - 1) & !(align - 1)
}

#[inline]
fn align_down(value: u32, align: u32) -> u32 {
    value & !(align - 1)
}

fn is_sandy_bridge(device_id: u16) -> bool {
    SNB_IDS.iter().any(|&id| id == device_id)
}

fn find_snb() -> Option<PciDevice> {
    crate::pci::enumerate()
        .into_iter()
        .find(|d| d.vendor_id == INTEL_VENDOR && is_sandy_bridge(d.device_id))
}

fn memory_bar(dev: &PciDevice, index: usize) -> Option<(u32, u32)> {
    match dev.bars[index] {
        Bar::Memory { address, size, .. } if address != 0 && size != 0 => Some((address, size)),
        _ => None,
    }
}

unsafe fn read_pipe_state(mmio: Mmio, pipe: Pipe) -> PipeState {
    let d = pipe.delta();
    let conf = mmio.read32(PIPECONF_A + d);
    let htotal = mmio.read32(HTOTAL_A + d);
    let vtotal = mmio.read32(VTOTAL_A + d);
    let src = mmio.read32(PIPESRC_A + d);
    let cntr = mmio.read32(DSPCNTR_A + d);

    PipeState {
        pipe,
        enabled: (conf & PIPECONF_ENABLE) != 0,
        state_enabled: (conf & PIPECONF_STATE_ENABLE) != 0,
        plane_enabled: (cntr & DISP_ENABLE) != 0,
        // HTOTAL/VTOTAL low 16 bits contain active-1.
        active_width: (htotal & 0xffff).wrapping_add(1),
        active_height: (vtotal & 0xffff).wrapping_add(1),
        // PIPESRC stores width-1 in the high half and height-1 in the low half.
        source_width: ((src >> 16) & 0xffff).wrapping_add(1),
        source_height: (src & 0xffff).wrapping_add(1),
        plane_cntr: cntr,
        plane_stride: mmio.read32(DSPSTRIDE_A + d),
        plane_surface: mmio.read32(DSPSURF_A + d),
        plane_surface_live: mmio.read32(DSPSURFLIVE_A + d),
    }
}

fn choose_pipe(a: PipeState, b: PipeState) -> Option<PipeState> {
    // Prefer a pipe where both the transcoder/pipe and the primary plane are
    // already active.  Fall back to any enabled pipe.
    if a.enabled && a.plane_enabled {
        Some(a)
    } else if b.enabled && b.plane_enabled {
        Some(b)
    } else if a.enabled {
        Some(a)
    } else if b.enabled {
        Some(b)
    } else {
        None
    }
}

fn gen6_pte(phys: u32) -> u32 {
    // Gen6 encodes physical address bits 39:32 into PTE bits 11:4.  Our 32-bit
    // allocator cannot currently return >4 GiB, so that field is zero today.
    let high = (((phys as u64) >> 28) as u32) & 0xff0;
    (phys & 0xffff_f000) | high | GEN6_PTE_CACHE_LLC | GEN6_PTE_VALID
}

unsafe fn wait_for_vblank(mmio: Mmio, pipe: Pipe, active_height: u32) {
    // Best-effort only.  Avoid hanging boot if the scanline counter is odd on a
    // particular firmware configuration.
    for _ in 0..1_000_000usize {
        let line = mmio.read32(PIPEDSL_A + pipe.delta()) & 0x000f_ffff;
        if line >= active_height {
            break;
        }
        core::hint::spin_loop();
    }
}

unsafe fn install_ggtt_framebuffer(
    mmio: Mmio,
    gttmmadr_size: u32,
    gmadr_phys: u32,
    gmadr_size: u32,
    old_surface_a: u32,
    old_surface_b: u32,
    width: u32,
    height: u32,
) -> Result<(u32, u32, u32), &'static str> {
    if width == 0 || height == 0 || width > u16::MAX as u32 || height > u16::MAX as u32 {
        return Err("invalid panel dimensions");
    }

    // Intel linear scanout stride is safest at a 64-byte boundary.
    let pitch = align_up(
        width.checked_mul(4).ok_or("framebuffer pitch overflow")?,
        64,
    );
    if pitch > u16::MAX as u32 {
        return Err("framebuffer pitch does not fit FramebufferInfo");
    }

    let fb_bytes = pitch
        .checked_mul(height)
        .ok_or("framebuffer size overflow")?;
    let page_count = align_up(fb_bytes, PAGE_SIZE) / PAGE_SIZE;
    let aperture_span = align_up(fb_bytes, LARGE_PAGE);

    if gmadr_size < aperture_span + LARGE_PAGE {
        return Err("Intel GMADR aperture too small");
    }
    if gttmmadr_size <= GGTT_PTE_WINDOW as u32 {
        return Err("Intel GTTMMADR does not contain GGTT window");
    }

    // Put our scanout near the end of the CPU aperture.  Firmware/bootloader
    // allocations conventionally live near the beginning, so this avoids
    // overwriting the currently displayed VBE surface before the final flip.
    let mut gtt_offset = align_down(gmadr_size - aperture_span, LARGE_PAGE);

    let old_a = old_surface_a & !0xfff;
    let old_b = old_surface_b & !0xfff;
    let overlaps = |surf: u32, start: u32, len: u32| -> bool {
        surf >= start && surf < start.saturating_add(len)
    };

    if overlaps(old_a, gtt_offset, aperture_span) || overlaps(old_b, gtt_offset, aperture_span) {
        if gtt_offset < aperture_span + LARGE_PAGE {
            return Err("no safe GGTT aperture slot away from firmware scanout");
        }
        gtt_offset = align_down(gtt_offset - aperture_span, LARGE_PAGE);
    }

    let first_pte = gtt_offset / PAGE_SIZE;
    let ggtt_entries = (gttmmadr_size - GGTT_PTE_WINDOW as u32) / 4;
    if first_pte
        .checked_add(page_count)
        .ok_or("GGTT index overflow")?
        > ggtt_entries
    {
        return Err("framebuffer exceeds Gen6 GGTT PTE window");
    }

    let fb_dma = dma_alloc_for(
        "intel-snb FB backing",
        (page_count * PAGE_SIZE) as usize,
        PAGE_SIZE as usize,
        u32::MAX as u64,
    )
    .map_err(|_| "Intel framebuffer DMA allocation failed")?;
    let first_phys = fb_dma.phys.0;
    let last_phys = first_phys
        .checked_add((page_count - 1) * PAGE_SIZE)
        .ok_or("Intel framebuffer DMA range overflow")?;
    for i in 0..page_count {
        let phys = first_phys + i * PAGE_SIZE;
        let pte_addr = mmio
            .base
            .add(GGTT_PTE_WINDOW + ((first_pte + i) as usize * 4))
            as *mut u32;
        write_volatile(pte_addr, gen6_pte(phys));
    }
    *INTEL_FB_DMA.lock() = Some(fb_dma);

    crate::println!(
        "[SNB] FB backing RAM: {} pages, phys first={:#x} last={:#x}",
        page_count,
        first_phys,
        last_phys
    );

    // Same invalidate sequence used by i915 on Gen6: write bit 0, then posting
    // read.  The uncached MMIO access also drains posted GGTT PTE writes.
    mmio.write32(GFX_FLSH_CNTL_GEN6, GFX_FLSH_CNTL_EN);
    let _ = mmio.read32(GFX_FLSH_CNTL_GEN6);

    let cpu_aperture_phys = gmadr_phys
        .checked_add(gtt_offset)
        .ok_or("GMADR physical address overflow")?;

    let fb_virt = map_framebuffer_resource(
        cpu_aperture_phys,
        fb_bytes,
        "intel-snb-framebuffer",
    )?;

    // Clear before the display plane points at this surface.
    write_bytes(fb_virt as *mut u8, 0, fb_bytes as usize);

    Ok((gtt_offset, pitch, fb_bytes))
}

/// Try to replace the bootloader VBE framebuffer with a native-size Intel Gen6
/// primary-plane framebuffer.
///
/// Return values:
/// - `Ok(false)`: no Sandy Bridge GPU found; caller may try another GPU driver.
/// - `Ok(true)`: Intel framebuffer takeover completed.
/// - `Err(..)`: Sandy Bridge exists, but the safe takeover preconditions failed.
pub fn init_native_framebuffer() -> Result<bool, &'static str> {
    let dev = match find_snb() {
        Some(dev) => dev,
        None => return Ok(false),
    };

    dev.enable_memory_space();

    let (gttmmadr_phys, gttmmadr_size) =
        memory_bar(&dev, GTTMMADR_BAR).ok_or("Intel GTTMMADR BAR0 missing")?;
    let (gmadr_phys, gmadr_size) = memory_bar(&dev, GMADR_BAR).ok_or("Intel GMADR BAR2 missing")?;

    if gttmmadr_size < 0x0040_0000 {
        return Err("Intel GTTMMADR BAR0 smaller than expected Gen6 4 MiB");
    }

    let mmio_virt = reserve_and_ioremap(
        gttmmadr_phys as u64,
        gttmmadr_size as usize,
        ResourceKind::Mmio,
        "intel-snb-gttmmadr",
    )
    .map_err(|_| "failed to reserve/map Intel Gen6 GTTMMADR")?;

    let mmio = Mmio {
        base: mmio_virt.0 as *mut u8,
    };

    unsafe {
        let a = read_pipe_state(mmio, Pipe::A);
        let b = read_pipe_state(mmio, Pipe::B);
        let pfit = mmio.read32(PFIT_CONTROL);
        let vga = mmio.read32(CPU_VGACNTRL);

        crate::println!(
            "[SNB] {:04x}:{:04x} BAR0={:#x}/{}K BAR2={:#x}/{}M",
            dev.vendor_id,
            dev.device_id,
            gttmmadr_phys,
            gttmmadr_size / 1024,
            gmadr_phys,
            gmadr_size / (1024 * 1024)
        );
        crate::println!(
            "[SNB] pipe A en={}/{} plane={} timing={}x{} src={}x{} stride={} surf={:#x} live={:#x}",
            a.enabled,
            a.state_enabled,
            a.plane_enabled,
            a.active_width,
            a.active_height,
            a.source_width,
            a.source_height,
            a.plane_stride,
            a.plane_surface,
            a.plane_surface_live
        );
        crate::println!(
            "[SNB] pipe B en={}/{} plane={} timing={}x{} src={}x{} stride={} surf={:#x} live={:#x}",
            b.enabled,
            b.state_enabled,
            b.plane_enabled,
            b.active_width,
            b.active_height,
            b.source_width,
            b.source_height,
            b.plane_stride,
            b.plane_surface,
            b.plane_surface_live
        );
        crate::println!("[SNB] PFIT={:#x} VGA={:#x}", pfit, vga);

        let state = choose_pipe(a, b).ok_or("BIOS left no Intel display pipe enabled")?;

        // We deliberately reuse firmware's native timing.  If timing itself is
        // only 800x600, changing framebuffer dimensions cannot create a native
        // 1366x768 signal; that case needs the later full DPLL/FDI/PCH modeset.
        if state.active_width <= 800 && state.active_height <= 600 {
            return Err("firmware pipe timing is only 800x600; full Intel modeset required");
        }

        // X220 is 1366x768, but keep this generic enough for other SNB laptops
        // whose firmware already initialized a sane native panel timing.
        if state.active_width < 1024
            || state.active_height < 600
            || state.active_width > 2560
            || state.active_height > 1600
        {
            return Err("firmware pipe timing is not a sane native laptop mode");
        }

        let (gtt_offset, pitch, fb_bytes) = install_ggtt_framebuffer(
            mmio,
            gttmmadr_size,
            gmadr_phys,
            gmadr_size,
            a.plane_surface,
            b.plane_surface,
            state.active_width,
            state.active_height,
        )?;

        let d = state.pipe.delta();

        wait_for_vblank(mmio, state.pipe, state.active_height);

        // Stop legacy VGA from competing with the primary plane.  Sandy Bridge
        // has the same sequencer workaround used by i915: blank VGA through
        // SR01 first, then disable the VGA plane in CPU_VGACNTRL.
        if (vga & VGA_DISP_DISABLE) == 0 {
            crate::io::outb(0x3c4, 0x01);
            let sr1 = crate::io::inb(0x3c5);
            crate::io::outb(0x3c5, sr1 | 0x20);
            mmio.write32(CPU_VGACNTRL, vga | VGA_DISP_DISABLE);
            let _ = mmio.read32(CPU_VGACNTRL);
        }

        // Make the pipe source match the already-programmed active timing.  This
        // removes the 800x600 source that the firmware panel fitter was scaling.
        mmio.write32(
            PIPESRC_A + d,
            ((state.active_width - 1) << 16) | (state.active_height - 1),
        );

        // Leave the Sandy Bridge panel fitter control untouched while the pipe
        // is live.  On gen4+ the fitter ratios are hardware-computed; with
        // PIPESRC now equal to the native timing this becomes a 1:1 path without
        // a risky live PFIT enable/disable transition.
        let _pfit_was_enabled = (pfit & PFIT_ENABLE) != 0;

        // Linear XRGB8888/BGRX888 scanout.  `framebuffer.rs` stores bytes as
        // B,G,R,0, which matches Intel DISP_FORMAT_BGRX888.
        let mut cntr = state.plane_cntr;
        cntr &= !(DISP_FORMAT_MASK | DISP_TILED | DISP_ROTATE_180);
        cntr |= DISP_ENABLE | DISP_FORMAT_BGRX888;

        mmio.write32(DSPLINOFF_A + d, 0);
        mmio.write32(DSPTILEOFF_A + d, 0);
        mmio.write32(DSPSTRIDE_A + d, pitch);
        mmio.write32(DSPCNTR_A + d, cntr);

        // DSPSURF arms the double-buffered control/stride update at vblank.
        mmio.write32(DSPSURF_A + d, gtt_offset);
        let _ = mmio.read32(DSPSURF_A + d);

        // Update the kernel's generic framebuffer only after the hardware flip
        // has been armed. The framebuffer VA is owned by the central mapper.
        let info = FramebufferInfo {
            address: gmadr_phys + gtt_offset,
            pitch: pitch as u16,
            width: state.active_width as u16,
            height: state.active_height as u16,
            bpp: 32,
            reserved: [0; 3],
        };

        write_volatile(FB_INFO_PHYS as *mut FramebufferInfo, info);
        let fb_virt = current_lfb_virt();
        {
            let mut fb = FRAMEBUFFER.lock();
            match fb.as_mut() {
                Some(current) => {
                    current.info = info;
                    current.virt_base = fb_virt;
                }
                None => {
                    *fb = Some(Framebuffer {
                        info,
                        virt_base: fb_virt,
                    });
                }
            }
        }

        wait_for_vblank(mmio, state.pipe, state.active_height);

        let src_after = mmio.read32(PIPESRC_A + d);
        let cntr_after = mmio.read32(DSPCNTR_A + d);
        let stride_after = mmio.read32(DSPSTRIDE_A + d);
        let surf_after = mmio.read32(DSPSURF_A + d);
        let live_after = mmio.read32(DSPSURFLIVE_A + d);
        let pfit_after = mmio.read32(PFIT_CONTROL);

        crate::println!(
            "[SNB] native FB {}x{}x32 pipe={} pitch={} bytes={} GGTT={:#x} CPU={:#x}",
            state.active_width,
            state.active_height,
            state.pipe.name(),
            pitch,
            fb_bytes,
            gtt_offset,
            gmadr_phys + gtt_offset
        );
        crate::println!(
            "[SNB] after: PIPESRC={:#x} DSPCNTR={:#x} STRIDE={} SURF={:#x} LIVE={:#x} PFIT={:#x}",
            src_after,
            cntr_after,
            stride_after,
            surf_after,
            live_after,
            pfit_after
        );
    }

    Ok(true)
}
