use crate::memory::paging::{PAGING, PTEFlags, PageDirectory, PhysAddr, VirtAddr};
use crate::sync::mutex::Mutex;
use crate::{debugln, println};
use core::arch::asm;
use core::ptr;

/// Run `f` with interrupts disabled so a timer tick cannot switch CR3
/// away from the kernel PD (where the LFB is mapped) mid-draw.
#[inline]
fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    let eflags: u32;
    unsafe {
        asm!("pushfd; pop {}", out(reg) eflags);
        asm!("cli");
    }
    let r = f();
    if (eflags & (1 << 9)) != 0 {
        unsafe {
            asm!("sti");
        }
    }
    r
}

/// Структура, которую нам передал бутлоадер
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
pub struct FramebufferInfo {
    pub address: u32,
    pub pitch: u16,
    pub width: u16,
    pub height: u16,
    pub bpp: u8,
    pub reserved: [u8; 3],
}

pub const FB_INFO_PHYS: u32 = 0x0000_5000;

/// Виртуальный адрес, куда мы замапим LFB
pub const FB_VIRT_BASE: u32 = 0xD000_0000;

pub static mut LFB_PHYS: u32 = 0;
pub static mut LFB_SIZE: u32 = 0;

/// Map LFB as 4 MiB large pages — shared via copy_kernel_mappings, no fragile 4K PT.
pub fn map_lfb_large(dir: &mut crate::memory::paging::PageDirectory) {
    use crate::memory::paging::PDEFlags;
    let phys = unsafe { LFB_PHYS };
    let size = unsafe { LFB_SIZE };
    if phys == 0 || size == 0 {
        return;
    }
    let phys_al = phys & 0xFFC0_0000;
    let extra = phys - phys_al;
    let n = ((extra + size + 0x3F_FFFF) / 0x40_0000).max(1);
    for i in 0..n {
        dir.map_large(
            FB_VIRT_BASE + i * 0x400000,
            phys_al + i * 0x400000,
            PDEFlags::new().present().writable().accessed().large_page(),
        );
    }
}

pub fn lfb_pde() -> u32 {
    unsafe {
        let pd = crate::memory::paging::phys_to_virt(crate::memory::paging::KERNEL_PD_PHYS)
            as *const [u32; 1024];
        (*pd)[(FB_VIRT_BASE >> 22) as usize]
    }
}

pub struct Framebuffer {
    pub info: FramebufferInfo,
    pub virt_base: u32,
}

impl Framebuffer {
    pub fn init() -> Option<Self> {
        // Читаем информацию из low memory (identity mapped)
        let info = unsafe { ptr::read_volatile(FB_INFO_PHYS as *const FramebufferInfo) };

        if info.address == 0 || info.width == 0 || info.height == 0 {
            println!("[FB] No valid framebuffer info found");
            return None;
        }

        let width = info.width;
        let height = info.height;
        let bpp = info.bpp;
        let pitch = info.pitch;
        let address = info.address;

        debugln!(
            "[FB] {}x{} {}bpp  pitch={}  phys=0x{:08x}",
            width,
            height,
            bpp,
            pitch,
            address
        );

        let bytes_per_line = info.pitch as u32;
        let height_u = info.height as u32;
        let size = bytes_per_line * height_u;
        let start_offset = info.address & 0xFFF;
        let total_size = size + start_offset + 0x10000;
        let virt_base = FB_VIRT_BASE + start_offset;
        println!("[FB] mapping LFB large pages (≈ {} KB)", total_size / 1024);
        unsafe {
            LFB_PHYS = info.address;
            LFB_SIZE = total_size;
            let mut paging = PAGING.lock();
            map_lfb_large(&mut paging.dir);
        }
        crate::memory::paging::PageDirectory::flush_all();
        println!(
            "[FB] ready {}x{} {}bpp pitch={} virt={:#x} phys={:#x} PDE={:#x}",
            width,
            height,
            bpp,
            pitch,
            virt_base,
            address,
            lfb_pde()
        );
        Some(Framebuffer { info, virt_base })
    }

    /// Hot path — caller must hold interrupts off (or use `put_pixel`).
    #[inline]
    pub fn put_pixel_raw(&self, x: u32, y: u32, color: u32) {
        if x >= self.info.width as u32 || y >= self.info.height as u32 {
            return;
        }

        let bytes_per_pixel = ((self.info.bpp as u32 + 7) / 8) as usize;
        let offset = (y * self.info.pitch as u32 + x * bytes_per_pixel as u32) as usize;
        let ptr = (self.virt_base as *mut u8).wrapping_add(offset);

        unsafe {
            match self.info.bpp {
                32 | 24 => {
                    *ptr = (color & 0xFF) as u8; // Blue
                    *ptr.add(1) = ((color >> 8) & 0xFF) as u8; // Green
                    *ptr.add(2) = ((color >> 16) & 0xFF) as u8; // Red
                    if self.info.bpp == 32 {
                        *ptr.add(3) = 0;
                    }
                }
                16 => {
                    let r = ((color >> 16) & 0xFF) as u16;
                    let g = ((color >> 8) & 0xFF) as u16;
                    let b = (color & 0xFF) as u16;
                    let pixel = ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3);
                    (ptr as *mut u16).write_unaligned(pixel);
                }
                _ => {
                    *ptr = (color & 0xFF) as u8;
                }
            }
        }
    }

    #[inline]
    pub fn put_pixel(&self, x: u32, y: u32, color: u32) {
        without_interrupts(|| self.put_pixel_raw(x, y, color));
    }

    pub fn fill(&self, color: u32) {
        without_interrupts(|| {
            // Prefer word-fill when possible
            if self.info.bpp == 32 && (self.info.pitch as u32) % 4 == 0 {
                self.fill_fast_raw(color);
                return;
            }
            let h = self.info.height as u32;
            let w = self.info.width as u32;
            for y in 0..h {
                for x in 0..w {
                    self.put_pixel_raw(x, y, color);
                }
            }
        });
    }

    fn fill_fast_raw(&self, color: u32) {
        // Fill full scanlines by pitch (handles possible pitch > width*4).
        let pitch_words = (self.info.pitch as u32) / 4;
        let total = pitch_words * self.info.height as u32;
        let ptr = self.virt_base as *mut u32;
        unsafe {
            let mut i = 0u32;
            // Unroll a bit — much faster than per-pixel in debug builds.
            while i + 8 <= total {
                *ptr.add(i as usize) = color;
                *ptr.add(i as usize + 1) = color;
                *ptr.add(i as usize + 2) = color;
                *ptr.add(i as usize + 3) = color;
                *ptr.add(i as usize + 4) = color;
                *ptr.add(i as usize + 5) = color;
                *ptr.add(i as usize + 6) = color;
                *ptr.add(i as usize + 7) = color;
                i += 8;
            }
            while i < total {
                *ptr.add(i as usize) = color;
                i += 1;
            }
        }
    }

    pub fn fill_fast(&self, color: u32) {
        without_interrupts(|| {
            if self.info.bpp == 32 && (self.info.pitch as u32) % 4 == 0 {
                self.fill_fast_raw(color);
            } else {
                self.fill_rect_raw(0, 0, self.info.width as u32, self.info.height as u32, color);
            }
        });
    }

    /// Bulk fill without locking interrupts (caller must hold cli if needed).
    fn fill_rect_raw(&self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        if w == 0 || h == 0 {
            return;
        }
        let fb_w = self.info.width as u32;
        let fb_h = self.info.height as u32;
        if x >= fb_w || y >= fb_h {
            return;
        }
        let w = w.min(fb_w - x);
        let h = h.min(fb_h - y);

        // Fast path: 32bpp, word-aligned start
        if self.info.bpp == 32 && (self.info.pitch as u32) % 4 == 0 {
            let pitch_words = (self.info.pitch as u32) / 4;
            let ptr = self.virt_base as *mut u32;
            unsafe {
                for row in 0..h {
                    let line = ptr.add(((y + row) * pitch_words + x) as usize);
                    let mut dx = 0u32;
                    while dx + 4 <= w {
                        *line.add(dx as usize) = color;
                        *line.add(dx as usize + 1) = color;
                        *line.add(dx as usize + 2) = color;
                        *line.add(dx as usize + 3) = color;
                        dx += 4;
                    }
                    while dx < w {
                        *line.add(dx as usize) = color;
                        dx += 1;
                    }
                }
            }
            return;
        }

        for dy in 0..h {
            for dx in 0..w {
                self.put_pixel_raw(x + dx, y + dy, color);
            }
        }
    }

    pub fn fill_rect(&self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        without_interrupts(|| self.fill_rect_raw(x, y, w, h, color));
    }
}

pub static FRAMEBUFFER: Mutex<Option<Framebuffer>> = Mutex::new(None);

pub fn init() {
    if let Some(fb) = Framebuffer::init() {
        *FRAMEBUFFER.lock() = Some(fb);
    } else {
        println!("[FB] init err");
    }
}

use embedded_graphics::{Pixel, pixelcolor::Rgb888, prelude::*};

impl OriginDimensions for Framebuffer {
    fn size(&self) -> Size {
        Size::new(self.info.width as u32, self.info.height as u32)
    }
}

impl DrawTarget for Framebuffer {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        // One cli for the whole batch — much faster than per-pixel
        without_interrupts(|| {
            for Pixel(coord, color) in pixels {
                if coord.x >= 0 && coord.y >= 0 {
                    let c = (color.r() as u32) << 16 | (color.g() as u32) << 8 | (color.b() as u32);
                    self.put_pixel_raw(coord.x as u32, coord.y as u32, c);
                }
            }
        });
        Ok(())
    }
}
