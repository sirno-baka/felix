use core::arch::asm;
use core::ptr;
use crate::memory::paging::{PhysAddr, VirtAddr, PTEFlags, PAGING, PageDirectory};
use crate::sync::mutex::Mutex;
use crate::{debugln, println};

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

pub struct Framebuffer {
    pub info: FramebufferInfo,
    pub virt_base: u32,
}

impl Framebuffer {
    pub fn init() -> Option<Self> {
        // Читаем информацию из low memory (identity mapped)
        let info = unsafe {
            ptr::read_volatile(FB_INFO_PHYS as *const FramebufferInfo)
        };

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
            width, height, bpp, pitch, address
        );

        let bytes_per_line = info.pitch as u32;
        let height_u = info.height as u32;

        // Реальный размер фреймбуфера
        let size = bytes_per_line * height_u;

        // Учитываем возможное смещение физического адреса + запас
        let start_offset = info.address & 0xFFF;
        let total_size = size + start_offset + 0x10000; // +64 KiB slack
        let pages = ((total_size + 4095) / 4096).max(1);
        let virt_base = FB_VIRT_BASE + start_offset;
        println!("[FB] mapping {} pages (≈ {} KB)", pages, total_size / 1024);
        unsafe {
            let mut paging = PAGING.lock();

            for i in 0..pages {
                let phys = (info.address & !0xFFF) + i * 4096;
                let virt = FB_VIRT_BASE + i * 4096;

                let vpage = virt >> 12;
                let ppage = phys >> 12;

                let pd_index = (vpage >> 10) as usize;

                // 1. Убеждаемся, что page table существует
                //    (выделяем фрейм для PT если нужно)
                let need_table = {
                    let pde = paging.dir.entries[pd_index];
                    pde == 0 || (pde & 1) == 0   // PRESENT bit
                };
                if need_table {
                    let pt_frame = paging.alloc_frame();          // номер фрейма
                    let pt_phys  = pt_frame << 12;

                    // 1. Ставим PDE
                    paging.dir.entries[pd_index] = pt_phys | 0x3; // Present + Writable

                    // 2. Обязательно сбрасываем TLB именно рекурсивного адреса новой таблицы
                    let pt_recursive = 0xFFC00000u32 + (pd_index as u32) * 4096;
                    crate::memory::paging::PageDirectory::flush_page(pt_recursive);

                    // 3. Теперь рекурсивный указатель безопасен
                    let pt_ptr = crate::memory::paging::PageDirectory::get_page_table_ptr(pd_index);
                    unsafe {
                        core::ptr::write_bytes(pt_ptr as *mut u8, 0, 4096);
                    }
                    PageDirectory::flush_page((pd_index as u32) << 22);
                    PageDirectory::flush_page(pt_recursive);
                }

                // 2. Теперь page table точно есть → используем map_existing
                paging.dir.map_existing(
                    vpage,
                    ppage,
                    PTEFlags::new().present().writable(),
                );

            }

        }
        crate::memory::paging::PageDirectory::flush_all();
        let height = info.height;
        println!(
            "[FB] ready {}x{} {}bpp pitch={} virt={:#x} phys={:#x}",
            width, height, bpp, pitch, virt_base, address
        );
        Some(Framebuffer {
            info,
            virt_base,          // уже с учётом offset
        })
    }

    /// Hot path — caller must hold interrupts off (or use `put_pixel`).
    #[inline]
    fn put_pixel_raw(&self, x: u32, y: u32, color: u32) {
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


use embedded_graphics::{
    pixelcolor::Rgb888,
    prelude::*,
    Pixel,
};

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
                    let c = (color.r() as u32) << 16
                        | (color.g() as u32) << 8
                        | (color.b() as u32);
                    self.put_pixel_raw(coord.x as u32, coord.y as u32, c);
                }
            }
        });
        Ok(())
    }
}