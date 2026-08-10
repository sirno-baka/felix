use alloc::vec;
use alloc::vec::Vec;
use crate::disk::interface::BlockDevice;
use core::ptr;
use crate::println;

pub const SECTOR_SIZE: u32 = 512;
pub const RAMDISK_SIZE: usize = 5 * 1024 * 1024; // 5MB
pub const RAMFS_LBA: u64 = 2114; //kernel location logical block address
pub const RAMFS_TARGET: u32 = 0x0060_0000; //where to put kernel in memory

#[derive(Clone)]
pub struct RamDisk {
    pub data: Vec<u8>,
}

impl RamDisk {
    pub fn new() -> Self {
        let mut data = vec![0; RAMDISK_SIZE];
        unsafe {
            ptr::copy_nonoverlapping(RAMFS_TARGET as *const u8, data.as_mut_ptr(), (766 * SECTOR_SIZE) as usize);
        }
        RamDisk {
            data: data,
        }
    }
}

impl BlockDevice for RamDisk {
    fn read_sectors(&self, numsects: u8, lba: u32, buf: u32) -> Result<(), u8> {
        let start_offset = lba * SECTOR_SIZE;
        let bytes_to_read = (numsects as u32) * SECTOR_SIZE;

        if start_offset as usize + bytes_to_read as usize > self.data.len() {
            return Err(1);
        }

        if buf == 0 {
            return Err(2);
        }

        let src_ptr = unsafe { self.data.as_ptr().add(start_offset as usize) };
        let dst_ptr = buf as *mut u8;

        unsafe {
            ptr::copy_nonoverlapping(src_ptr, dst_ptr, bytes_to_read as usize);
        }

        Ok(())
    }

    fn write_sectors(&mut self, numsects: u8, lba: u32, buf: u32) -> Result<(), u8> {
        let start_offset = lba * SECTOR_SIZE;
        let bytes_to_write = (numsects as u32) * SECTOR_SIZE;

        if start_offset as usize + bytes_to_write as usize > self.data.len() {
            return Err(1);
        }

        if buf == 0 {
            return Err(2);
        }

        let src_ptr = buf as *const u8;
        let dst_ptr = unsafe { self.data.as_mut_ptr().add(start_offset as usize) };
        println!("src {:x}", src_ptr as usize);
        println!("dst {:x}", dst_ptr as usize);
        unsafe {
            ptr::copy_nonoverlapping(src_ptr, dst_ptr, bytes_to_write as usize);
        }

        Ok(())
    }

    fn sector_size(&self) -> u32 {
        SECTOR_SIZE
    }
}