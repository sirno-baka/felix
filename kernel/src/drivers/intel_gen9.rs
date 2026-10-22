//! Minimal Intel Gen9 (Skylake/Kaby Lake) framebuffer takeover.
//!
//! This is intentionally not a full i915/KMS driver. Firmware must already
//! have a working native display pipe and a full-screen linear XRGB8888 primary
//! plane. We keep clocks, DDI/eDP link state, scalers, watermarks/DBUF,
//! plane format and stride untouched. The only display change is switching
//! PLANE_SURF to a new GGTT mapping backed by normal RAM.

use crate::drivers::framebuffer::{
    FB_INFO_PHYS, FRAMEBUFFER, Framebuffer, FramebufferInfo, current_lfb_virt,
    map_framebuffer_resource,
};
use crate::memory::resources::{DmaBuffer, ResourceKind, dma_alloc_for, map_resource};
use crate::pci::bar::Bar;
use crate::pci::device::PciDevice;
use crate::sync::mutex::Mutex;
use core::ptr::{read_volatile, write_bytes, write_volatile};

const INTEL_VENDOR: u16 = 0x8086;

// Linux i915 Gen9 PCI IDs (Skylake + Kaby Lake families). Keep the class-code
// check too: 8086:1911, for example, is not a display controller.
const GEN9_IDS: &[u16] = &[
    0x1902, 0x1906, 0x190a, 0x190b, 0x190e, 0x1913, 0x1915, 0x1917, 0x1912, 0x1916, 0x191a, 0x191b,
    0x191d, 0x191e, 0x1921, 0x1923, 0x1926, 0x1927, 0x192a, 0x192b, 0x192d, 0x1932, 0x193a, 0x193b,
    0x193d, 0x5902, 0x5906, 0x5908, 0x590a, 0x590b, 0x590e, 0x5913, 0x5915, 0x5912, 0x5916, 0x5917,
    0x591a, 0x591b, 0x591d, 0x591e, 0x5921, 0x5923, 0x5926, 0x5927, 0x593b,
];

const GTTMMADR_BAR: usize = 0;
const GMADR_BAR: usize = 2;
const GTTMMADR_MIN_SIZE: u32 = 16 * 1024 * 1024;
const GGTT_PTE_WINDOW: u32 = 8 * 1024 * 1024;
// Only registers below this offset are touched by this minimal driver.
// Mapping BAR0 wholesale wastes the entire 16 MiB Felix MMIO VA window.
const REG_MMIO_SIZE: usize = 0x0010_2000;
const PAGE_SIZE: u32 = 4096;
const SLOT_ALIGN: u32 = 256 * 1024;
const GEN8_PAGE_PRESENT: u32 = 1;

const GFX_FLSH_CNTL_GEN6: usize = 0x101008;
const GFX_FLSH_CNTL_EN: u32 = 1;

const PIPE_STRIDE: usize = 0x1000;
const PIPECONF_A: usize = 0x70008;
const HTOTAL_A: usize = 0x60000;
const VTOTAL_A: usize = 0x6000c;
const PIPESRC_A: usize = 0x6001c;

const PLANE_CTL_1_A: usize = 0x70180;
const PLANE_STRIDE_1_A: usize = 0x70188;
const PLANE_POS_1_A: usize = 0x7018c;
const PLANE_SIZE_1_A: usize = 0x70190;
const PLANE_SURF_1_A: usize = 0x7019c;
const PLANE_OFFSET_1_A: usize = 0x701a4;
const PLANE_SURFLIVE_1_A: usize = 0x701ac;

const PIPECONF_ENABLE: u32 = 1 << 31;
const PLANE_CTL_ENABLE: u32 = 1 << 31;
const PLANE_CTL_FORMAT_MASK: u32 = 0x0f << 24;
const PLANE_CTL_FORMAT_XRGB_8888: u32 = 4 << 24;
const PLANE_CTL_ORDER_RGBX: u32 = 1 << 20;
const PLANE_CTL_RENDER_DECOMPRESSION: u32 = 1 << 15;
const PLANE_CTL_TILED_MASK: u32 = 0x7 << 10;
const PLANE_CTL_ROTATE_MASK: u32 = 0x3;

static INTEL_GEN9_FB_DMA: Mutex<Option<DmaBuffer>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pipe {
    A,
    B,
    C,
}

impl Pipe {
    fn index(self) -> usize {
        match self {
            Pipe::A => 0,
            Pipe::B => 1,
            Pipe::C => 2,
        }
    }

    fn delta(self) -> usize {
        self.index() * PIPE_STRIDE
    }

    fn name(self) -> char {
        match self {
            Pipe::A => 'A',
            Pipe::B => 'B',
            Pipe::C => 'C',
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
    pipe_enabled: bool,
    plane_enabled: bool,
    timing_w: u32,
    timing_h: u32,
    source_w: u32,
    source_h: u32,
    plane_w: u32,
    plane_h: u32,
    ctl: u32,
    stride_reg: u32,
    pos: u32,
    offset: u32,
    surface: u32,
    live: u32,
}

#[inline]
fn align_up(value: u32, align: u32) -> Option<u32> {
    value.checked_add(align - 1).map(|v| v & !(align - 1))
}

#[inline]
fn align_down(value: u32, align: u32) -> u32 {
    value & !(align - 1)
}

fn is_gen9_gpu(dev: &PciDevice) -> bool {
    dev.vendor_id == INTEL_VENDOR
        && dev.class_code == 0x03
        && GEN9_IDS.iter().any(|&id| id == dev.device_id)
}

fn find_gen9() -> Option<PciDevice> {
    let devices = crate::pci::enumerate();
    if let Some(dev) = devices.iter().copied().find(is_gen9_gpu) {
        return Some(dev);
    }

    // Useful on new real hardware: distinguish "no Intel GPU" from
    // "Intel display controller exists but its PCI ID is not in our table".
    if let Some(dev) = devices
        .iter()
        .find(|d| d.vendor_id == INTEL_VENDOR && d.class_code == 0x03)
    {
        crate::println!(
            "[GEN9] unsupported Intel display PCI ID {:04x}:{:04x} class={:02x}:{:02x}",
            dev.vendor_id,
            dev.device_id,
            dev.class_code,
            dev.subclass
        );
    }

    None
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
    let ctl = mmio.read32(PLANE_CTL_1_A + d);
    let size = mmio.read32(PLANE_SIZE_1_A + d);

    PipeState {
        pipe,
        pipe_enabled: (conf & PIPECONF_ENABLE) != 0,
        plane_enabled: (ctl & PLANE_CTL_ENABLE) != 0,
        timing_w: (htotal & 0xffff).wrapping_add(1),
        timing_h: (vtotal & 0xffff).wrapping_add(1),
        source_w: ((src >> 16) & 0xffff).wrapping_add(1),
        source_h: (src & 0xffff).wrapping_add(1),
        plane_w: (size & 0xffff).wrapping_add(1),
        plane_h: ((size >> 16) & 0xffff).wrapping_add(1),
        ctl,
        stride_reg: mmio.read32(PLANE_STRIDE_1_A + d),
        pos: mmio.read32(PLANE_POS_1_A + d),
        offset: mmio.read32(PLANE_OFFSET_1_A + d),
        surface: mmio.read32(PLANE_SURF_1_A + d),
        live: mmio.read32(PLANE_SURFLIVE_1_A + d),
    }
}

fn choose_pipe(states: &[PipeState; 3]) -> Option<PipeState> {
    states
        .iter()
        .copied()
        .find(|s| s.pipe_enabled && s.plane_enabled)
}

fn safe_takeover_state(s: PipeState) -> Result<(u32, u32, u32), &'static str> {
    if s.timing_w < 1024 || s.timing_h < 600 || s.timing_w > 4096 || s.timing_h > 2160 {
        return Err("firmware pipe timing is not a sane laptop mode");
    }
    if s.plane_w != s.timing_w || s.plane_h != s.timing_h {
        return Err("firmware primary plane is scaled; Gen9 scaler takeover not implemented");
    }
    if s.source_w != s.timing_w || s.source_h != s.timing_h {
        return Err("firmware pipe source differs from timing; refusing live modeset");
    }
    if (s.ctl & PLANE_CTL_FORMAT_MASK) != PLANE_CTL_FORMAT_XRGB_8888 {
        return Err("firmware primary plane is not XRGB8888");
    }
    if (s.ctl & PLANE_CTL_ORDER_RGBX) != 0 {
        return Err("firmware primary plane uses RGBX byte order");
    }
    if (s.ctl & PLANE_CTL_RENDER_DECOMPRESSION) != 0 {
        return Err("firmware primary plane uses render compression");
    }
    if (s.ctl & PLANE_CTL_TILED_MASK) != 0 {
        return Err("firmware primary plane is tiled; linear-only takeover");
    }
    if (s.ctl & PLANE_CTL_ROTATE_MASK) != 0 {
        return Err("firmware primary plane is rotated");
    }
    if s.pos != 0 || s.offset != 0 {
        return Err("firmware primary plane has non-zero position/offset");
    }

    // For linear Gen9 planes PLANE_STRIDE is expressed in 64-byte units.
    let stride_units = s.stride_reg & 0x0fff;
    if stride_units == 0 {
        return Err("firmware primary plane stride is zero");
    }
    let pitch = stride_units
        .checked_mul(64)
        .ok_or("Gen9 framebuffer pitch overflow")?;
    if pitch < s.plane_w * 4 || pitch > 32 * 1024 {
        return Err("firmware primary plane pitch is invalid");
    }
    let bytes = pitch
        .checked_mul(s.plane_h)
        .ok_or("Gen9 framebuffer size overflow")?;
    let alloc = align_up(bytes, PAGE_SIZE).ok_or("Gen9 FB page alignment overflow")?;
    Ok((pitch, bytes, alloc))
}

unsafe fn write_gen8_pte(pte_base: *mut u8, relative_index: u32, phys: u32) {
    let p = pte_base.add(relative_index as usize * 8) as *mut u32;
    // Gen8+ GGTT entries are 64-bit. Felix's DMA allocator currently returns
    // memory below 4 GiB, so the high dword is zero. Set PRESENT last.
    write_volatile(p.add(1), 0);
    write_volatile(p, (phys & 0xffff_f000) | GEN8_PAGE_PRESENT);
}

unsafe fn install_ggtt_framebuffer(
    mmio: Mmio,
    gttmmadr_phys: u32,
    gmadr_phys: u32,
    gmadr_size: u32,
    state: PipeState,
    pitch: u32,
    fb_bytes: u32,
    alloc_bytes: u32,
) -> Result<(u32, u32), &'static str> {
    if gmadr_size < alloc_bytes.saturating_add(SLOT_ALIGN) {
        return Err("Gen9 GMADR aperture too small");
    }

    let old_surface = state.surface & 0xffff_f000;
    let slot_span = align_up(alloc_bytes, SLOT_ALIGN).ok_or("Gen9 aperture span overflow")?;
    let mut gtt_offset = align_down(gmadr_size - slot_span, SLOT_ALIGN);
    if old_surface >= gtt_offset && old_surface < gtt_offset.saturating_add(slot_span) {
        if gtt_offset < slot_span + SLOT_ALIGN {
            return Err("no safe Gen9 GGTT aperture slot");
        }
        gtt_offset = align_down(gtt_offset - slot_span, SLOT_ALIGN);
    }

    let first_pte = gtt_offset / PAGE_SIZE;
    let pages = alloc_bytes / PAGE_SIZE;
    let gmadr_pages = gmadr_size / PAGE_SIZE;
    if first_pte
        .checked_add(pages)
        .ok_or("Gen9 GGTT index overflow")?
        > gmadr_pages
    {
        return Err("Gen9 framebuffer exceeds GMADR aperture");
    }

    let fb_dma = dma_alloc_for(
        "intel-gen9 FB backing",
        alloc_bytes as usize,
        PAGE_SIZE as usize,
        u32::MAX as u64,
    )
    .map_err(|_| "Gen9 framebuffer DMA allocation failed")?;
    let first_phys = fb_dma.phys.0;
    let last_phys = first_phys
        .checked_add((pages - 1) * PAGE_SIZE)
        .ok_or("Gen9 framebuffer DMA range overflow")?;

    // The GGTT PTE array lives at BAR0 + 8 MiB. Map only the entries we
    // are about to modify instead of consuming another 8 MiB of kernel VA.
    let pte_phys = gttmmadr_phys
        .checked_add(GGTT_PTE_WINDOW)
        .and_then(|v| v.checked_add(first_pte.checked_mul(8)?))
        .ok_or("Gen9 GGTT PTE physical address overflow")?;
    let pte_bytes = pages
        .checked_mul(8)
        .ok_or("Gen9 GGTT PTE mapping size overflow")? as usize;
    {
        let pte_mapping = map_resource(
            pte_phys as u64,
            pte_bytes,
            ResourceKind::Mmio,
            "intel-gen9-ggtt-pte",
        )
        .map_err(|_| "failed to map Intel Gen9 GGTT PTE slice")?;
        let pte_base = pte_mapping.as_usize() as *mut u8;
        for i in 0..pages {
            write_gen8_pte(pte_base, i, first_phys + i * PAGE_SIZE);
        }
        // Drop here intentionally: the PTE contents remain in hardware MMIO,
        // but Felix gets the scarce virtual mapping back immediately.
    }
    *INTEL_GEN9_FB_DMA.lock() = Some(fb_dma);

    mmio.write32(GFX_FLSH_CNTL_GEN6, GFX_FLSH_CNTL_EN);
    let _ = mmio.read32(GFX_FLSH_CNTL_GEN6);

    let cpu_aperture_phys = gmadr_phys
        .checked_add(gtt_offset)
        .ok_or("Gen9 GMADR physical address overflow")?;
    let fb_virt = map_framebuffer_resource(cpu_aperture_phys, fb_bytes, "intel-gen9-framebuffer")?;
    write_bytes(fb_virt as *mut u8, 0, fb_bytes as usize);

    crate::println!(
        "[GEN9] FB RAM {} pages phys={:#x}..{:#x} pitch={} GGTT={:#x} CPU={:#x}",
        pages,
        first_phys,
        last_phys,
        pitch,
        gtt_offset,
        cpu_aperture_phys
    );

    Ok((gtt_offset, cpu_aperture_phys))
}

pub fn init_native_framebuffer() -> Result<bool, &'static str> {
    let dev = match find_gen9() {
        Some(dev) => dev,
        None => return Ok(false),
    };

    dev.enable_memory_space();
    let (gttmmadr_phys, gttmmadr_size) =
        memory_bar(&dev, GTTMMADR_BAR).ok_or("Gen9 GTTMMADR BAR0 missing")?;
    let (gmadr_phys, gmadr_size) = memory_bar(&dev, GMADR_BAR).ok_or("Gen9 GMADR BAR2 missing")?;

    if gttmmadr_size < GTTMMADR_MIN_SIZE {
        return Err("Gen9 GTTMMADR BAR0 smaller than 16 MiB");
    }

    // Sparse BAR0 mapping. Display registers and GFX_FLSH_CNTL all fit in
    // the first ~1 MiB; mapping the full 16 MiB BAR0 used to exhaust Felix's
    // entire MMIO virtual window once the VESA framebuffer was already mapped.
    let regs_mapping = map_resource(
        gttmmadr_phys as u64,
        REG_MMIO_SIZE,
        ResourceKind::Mmio,
        "intel-gen9-regs",
    )
    .map_err(|_| "failed to map Intel Gen9 register window")?;
    let mmio = Mmio {
        base: regs_mapping.as_usize() as *mut u8,
    };

    unsafe {
        let states = [
            read_pipe_state(mmio, Pipe::A),
            read_pipe_state(mmio, Pipe::B),
            read_pipe_state(mmio, Pipe::C),
        ];

        crate::println!(
            "[GEN9] {:02x}:{:02x}.{} {:04x}:{:04x} rev={:02x} BAR0={:#x}/{}M BAR2={:#x}/{}M",
            dev.bus,
            dev.device,
            dev.function,
            dev.vendor_id,
            dev.device_id,
            dev.revision_id,
            gttmmadr_phys,
            gttmmadr_size / (1024 * 1024),
            gmadr_phys,
            gmadr_size / (1024 * 1024)
        );

        for s in states {
            crate::println!(
                "[GEN9] pipe {} pipe={} plane={} timing={}x{} src={}x{} size={}x{} ctl={:#010x} stride={:#x} pos={:#x} off={:#x} surf={:#x} live={:#x}",
                s.pipe.name(),
                s.pipe_enabled,
                s.plane_enabled,
                s.timing_w,
                s.timing_h,
                s.source_w,
                s.source_h,
                s.plane_w,
                s.plane_h,
                s.ctl,
                s.stride_reg,
                s.pos,
                s.offset,
                s.surface,
                s.live
            );
        }

        let state = choose_pipe(&states).ok_or("firmware left no active Gen9 primary plane")?;
        let (pitch, fb_bytes, alloc_bytes) = safe_takeover_state(state)?;
        let (gtt_offset, cpu_aperture_phys) = install_ggtt_framebuffer(
            mmio,
            gttmmadr_phys,
            gmadr_phys,
            gmadr_size,
            state,
            pitch,
            fb_bytes,
            alloc_bytes,
        )?;

        let surf_reg = PLANE_SURF_1_A + state.pipe.delta();
        mmio.write32(surf_reg, gtt_offset);
        let _ = mmio.read32(surf_reg);

        let live_reg = PLANE_SURFLIVE_1_A + state.pipe.delta();
        for _ in 0..2_000_000usize {
            if (mmio.read32(live_reg) & 0xffff_f000) == gtt_offset {
                break;
            }
            core::hint::spin_loop();
        }

        let live_after = mmio.read32(live_reg);
        if (live_after & 0xffff_f000) != gtt_offset {
            return Err("Gen9 PLANE_SURF did not latch");
        }

        let info = FramebufferInfo {
            address: cpu_aperture_phys,
            pitch: pitch as u16,
            width: state.plane_w as u16,
            height: state.plane_h as u16,
            bpp: 32,
            reserved: [0; 3],
        };
        write_volatile(FB_INFO_PHYS as *mut FramebufferInfo, info);

        let fb_virt = current_lfb_virt();
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

        crate::println!(
            "[GEN9] native FB {}x{}x32 pipe={} pitch={} GGTT={:#x} live={:#x}",
            state.plane_w,
            state.plane_h,
            state.pipe.name(),
            pitch,
            gtt_offset,
            live_after
        );
    }

    Ok(true)
}
