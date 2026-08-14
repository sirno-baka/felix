use core::ptr;
use crate::memory::paging::{PhysAddr, VirtAddr, PTEFlags, PAGING, PageDirectory};
use crate::sync::mutex::Mutex;
use crate::{debugln, println};

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
        let height = info.height as u32;

        // Реальный размер фреймбуфера
        let size = bytes_per_line * height;

        // Учитываем возможное смещение физического адреса + запас
        let start_offset = info.address & 0xFFF;
        let total_size = size + start_offset + 0x20000; // +128 КБ запаса — надёжнее
        let pages = 2048; //((total_size + 4095) / 4096) as u32;
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
                let virt_base = FB_VIRT_BASE + start_offset;
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
        unsafe {
            let test_addr = (FB_VIRT_BASE + 0x300000) as *mut u32; // 3 МБ от начала
            *test_addr = 0xDEADBEEF;
            println!("write at +3MB ok");

            let test_addr2 = (FB_VIRT_BASE + 0x500000) as *mut u32; // 5 МБ
            *test_addr2 = 0xCAFEBABE;
            println!("write at +5MB ok");
        }
        Some(Framebuffer {
            info,
            virt_base,          // уже с учётом offset
        })
    }

    #[inline]
    pub fn put_pixel(&self, x: u32, y: u32, color: u32) {
        if x >= self.info.width as u32 || y >= self.info.height as u32 {
            return;
        }

        let bytes_per_pixel = ((self.info.bpp as u32 + 7) / 8) as usize;
        // pitch уже в байтах!
        let offset = (y * self.info.pitch as u32 + x * bytes_per_pixel as u32) as usize;
        let ptr = (self.virt_base as *mut u8).wrapping_add(offset);

        unsafe {
            match self.info.bpp {
                32 | 24 => {
                    // 32bpp или 24bpp
                    *ptr = (color & 0xFF) as u8;               // Blue
                    *ptr.add(1) = ((color >> 8) & 0xFF) as u8;  // Green
                    *ptr.add(2) = ((color >> 16) & 0xFF) as u8; // Red
                    if self.info.bpp == 32 {
                        *ptr.add(3) = 0; // padding / alpha
                    }
                }
                16 => {
                    // RGB565
                    let r = ((color >> 16) & 0xFF) as u16;
                    let g = ((color >> 8) & 0xFF) as u16;
                    let b = (color & 0xFF) as u16;
                    let pixel = ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3);
                    (ptr as *mut u16).write_unaligned(pixel);
                }
                8 => {
                    *ptr = (color & 0xFF) as u8;
                }
                _ => {
                    *ptr = (color & 0xFF) as u8;
                }
            }
        }
    }

    pub fn fill(&self, color: u32) {
        // Рисуем только верхнюю четверть экрана
        let h = self.info.height as u32 ;
        for y in 0..h {
            for x in 0..self.info.width as u32 {
                self.put_pixel(x, y, color);
            }
        }
    }

    // Более быстрый fill (если bpp == 32 и pitch кратен 4)
    pub fn fill_fast(&self, color: u32) {
        if self.info.bpp == 32 && (self.info.pitch as u32) % 4 == 0 {
            let words_per_line = (self.info.pitch as u32) / 4;
            let ptr = self.virt_base as *mut u32;

            unsafe {
                for y in 0..self.info.height as u32 {
                    let line = ptr.add((y * words_per_line) as usize);
                    for x in 0..self.info.width as u32 {
                        *line.add(x as usize) = color;
                    }
                }
            }
        } else {
            self.fill(color);
        }
    }

    pub fn fill_rect(&self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        for dy in 0..h {
            for dx in 0..w {
                self.put_pixel(x + dx, y + dy, color);
            }
        }
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
        for Pixel(coord, color) in pixels {
            if coord.x >= 0 && coord.y >= 0 {
                let x = coord.x as u32;
                let y = coord.y as u32;
                // твой put_pixel (нужно будет адаптировать цвет)
                let c = (color.r() as u32) << 16
                    | (color.g() as u32) << 8
                    | (color.b() as u32);
                self.put_pixel(x, y, c);
            }
        }
        Ok(())
    }
}