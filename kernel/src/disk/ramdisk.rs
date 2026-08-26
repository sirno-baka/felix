//! In-memory block device.
//!
//! Two construction paths:
//! - `from_phys` — bootloader already copied the disk (PXE / BIOS INT 13h)
//! - `alloc` — allocate fresh frames (rare; not used for root anymore)

use core::ptr;

use crate::disk::interface::BlockDevice;
use crate::memory::paging::{PAGE_SIZE, PAGING, phys_to_virt};
use crate::println;

pub const SECTOR_SIZE: u32 = 512;

#[derive(Clone, Copy)]
pub struct RamDisk {
    ptr: *mut u8,
    size: usize,
}

unsafe impl Send for RamDisk {}
unsafe impl Sync for RamDisk {}

impl RamDisk {
    /// Wrap an existing physical image (placed by the bootloader).
    pub fn from_phys(phys: u32, sectors: u32) -> Self {
        let size = sectors as usize * SECTOR_SIZE as usize;
        let ptr = phys_to_virt(phys) as *mut u8;
        println!(
            "[ramdisk] from_phys 0x{:08x}  {} sectors ({} KiB)",
            phys,
            sectors,
            size / 1024
        );
        RamDisk { ptr, size }
    }

    /// Allocate `size` bytes of consecutive physical frames and zero them.
    pub fn alloc(size: usize) -> Result<Self, ()> {
        if size == 0 {
            return Err(());
        }
        let pages = (size + PAGE_SIZE - 1) / PAGE_SIZE;
        let total = pages * PAGE_SIZE;

        let first = interrupt_sync::without_interrupts(|| unsafe {
            let mut pm = PAGING.lock();
            let f = pm.alloc_frame();
            for _ in 1..pages {
                let _ = pm.alloc_frame();
            }
            f
        });

        let ptr = phys_to_virt(first << 12) as *mut u8;
        unsafe {
            ptr::write_bytes(ptr, 0, total);
        }

        println!(
            "[ramdisk] allocated {} KiB at phys 0x{:08x}",
            total / 1024,
            first << 12
        );

        Ok(RamDisk { ptr, size: total })
    }

    pub fn size_bytes(&self) -> usize {
        self.size
    }

    pub fn size_sectors(&self) -> u32 {
        (self.size / SECTOR_SIZE as usize) as u32
    }
}

impl BlockDevice for RamDisk {
    fn read_sectors(&self, numsects: u8, lba: u32, buf: u32) -> Result<(), u8> {
        let start = (lba as usize) * (SECTOR_SIZE as usize);
        let nbytes = (numsects as usize) * (SECTOR_SIZE as usize);

        if start + nbytes > self.size || buf == 0 {
            return Err(1);
        }

        unsafe {
            ptr::copy_nonoverlapping(self.ptr.add(start), buf as *mut u8, nbytes);
        }
        Ok(())
    }

    fn write_sectors(&mut self, numsects: u8, lba: u32, buf: u32) -> Result<(), u8> {
        let start = (lba as usize) * (SECTOR_SIZE as usize);
        let nbytes = (numsects as usize) * (SECTOR_SIZE as usize);

        if start + nbytes > self.size || buf == 0 {
            return Err(1);
        }

        unsafe {
            ptr::copy_nonoverlapping(buf as *const u8, self.ptr.add(start), nbytes);
        }
        Ok(())
    }

    fn sector_size(&self) -> u32 {
        SECTOR_SIZE
    }
}
