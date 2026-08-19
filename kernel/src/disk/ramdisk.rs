use crate::disk::interface::BlockDevice;
use crate::memory::paging::{PAGING, PTEFlags, PDEFlags, PageDirectory, phys_to_virt};
use core::ptr;

pub const SECTOR_SIZE: u32 = 512;
pub const RAMDISK_SIZE: usize = 5 * 1024 * 1024; // 5MB
pub const RAMFS_LBA: u64 = 2114;
pub const RAMFS_TARGET: u32 = 0x0060_0000;

#[derive(Clone, Copy)]
pub struct RamDisk {
    ptr: *mut u8,
    size: usize,
}
unsafe impl Send for RamDisk {}
unsafe impl Sync for RamDisk {}
impl RamDisk {
    pub fn new() -> Self {
        let size = RAMDISK_SIZE;
        let pages = (size + 4095) / 4096;

        // Динамический адрес сразу после кучи ядра (0xC200_0000)
        static mut KERNEL_RAMDISK_VIRT: u32 = 0xC200_0000;
        let virt_start = unsafe { KERNEL_RAMDISK_VIRT };

        let mut paging = unsafe { PAGING.lock() };

        for i in 0..pages {
            let virt = virt_start + (i as u32) * 4096;
            let vpage = virt >> 12;
            let pd_idx = (vpage >> 10) as usize;
            let pt_idx = (vpage & 0x3FF) as usize;

            // Создаем Page Table, если её нет
            let pde = paging.dir.entries[pd_idx];
            if pde == 0 || (pde & PDEFlags::PRESENT) == 0 {
                let pt_phys = paging.alloc_frame();
                // БЕЗ флага USER -> защита от user-space
                let pde_flags = PDEFlags::new().present().writable().bits();
                paging.dir.entries[pd_idx] = (pt_phys << 12) | pde_flags;
                PageDirectory::flush_page((pd_idx as u32) << 22);
                let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
                unsafe { ptr::write_bytes(pt_ptr as *mut u8, 0, 4096); }
            }

            // Выделяем физический фрейм и маппим его
            let phys_frame = paging.alloc_frame();
            let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
            unsafe {
                // БЕЗ флага USER -> защита от user-space
                (*pt_ptr)[pt_idx] = (phys_frame << 12)
                    | PTEFlags::PRESENT
                    | PTEFlags::WRITABLE
                    | PTEFlags::DIRTY;
            }
            PageDirectory::flush_page(virt);
        }

        unsafe { KERNEL_RAMDISK_VIRT += (pages as u32) * 4096; }
        drop(paging);

        let ptr = virt_start as *mut u8;

        // Копируем данные из памяти загрузчика
        unsafe {
            ptr::copy_nonoverlapping(
                phys_to_virt(RAMFS_TARGET) as *const u8,
                ptr,
                (766 * SECTOR_SIZE) as usize,
            );
        }

        RamDisk { ptr, size }
    }
}

impl BlockDevice for RamDisk {
    fn read_sectors(&self, numsects: u8, lba: u32, buf: u32) -> Result<(), u8> {
        let start_offset = (lba as usize) * (SECTOR_SIZE as usize);
        let bytes_to_read = (numsects as usize) * (SECTOR_SIZE as usize);

        if start_offset + bytes_to_read > self.size || buf == 0 {
            return Err(1);
        }

        unsafe {
            ptr::copy_nonoverlapping(self.ptr.add(start_offset), buf as *mut u8, bytes_to_read);
        }
        Ok(())
    }

    fn write_sectors(&mut self, numsects: u8, lba: u32, buf: u32) -> Result<(), u8> {
        let start_offset = (lba as usize) * (SECTOR_SIZE as usize);
        let bytes_to_write = (numsects as usize) * (SECTOR_SIZE as usize);

        if start_offset + bytes_to_write > self.size || buf == 0 {
            return Err(1);
        }

        unsafe {
            ptr::copy_nonoverlapping(buf as *const u8, self.ptr.add(start_offset), bytes_to_write);
        }
        Ok(())
    }

    fn sector_size(&self) -> u32 {
        SECTOR_SIZE
    }
}