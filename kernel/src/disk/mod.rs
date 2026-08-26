use crate::alloc::boxed::Box;
use crate::alloc::vec::Vec;
use crate::disk::interface::BlockDevice;
use crate::pci::ide::IDE;
use crate::println;
use alloc::vec;

pub mod interface;
pub mod ramdisk;
// ====================== PARTITION CONFIG + MBR PARSER ======================

#[derive(Copy, Clone, Debug)]
pub struct PartitionConfig {
    pub start_lba: u64,
}

impl PartitionConfig {
    /// Использовать весь диск (старый режим, без партишенов)
    pub const fn whole_disk() -> Self {
        PartitionConfig { start_lba: 0 }
    }

    /// Создать конфиг с конкретным смещением
    pub const fn new(start_lba: u64) -> Self {
        PartitionConfig { start_lba }
    }
}

#[repr(C, packed)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct CHS {
    /// Cylinder
    pub cylinder: u8,
    /// Head
    pub head: u8,
    /// Sector
    pub sector: u8,
}
#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
struct MbrPartitionEntry {
    pub status: u8,
    pub chs_start: CHS,
    pub partition_type: u8,
    pub chs_end: CHS,
    pub lba_start: u32,
    pub num_sectors: u32,
}

pub fn compare_vecs_diff(a: &Vec<u8>, b: &Vec<u8>) -> Option<usize> {
    for i in 0..a.len() {
        if a[i] != b[i] {
            println!("{} 0x{:08x} != 0x{:08x}", i, a[i], b[i])
        }
    }

    None
}
pub fn copy_sectors(
    src: &impl BlockDevice,
    dst: &mut impl BlockDevice,
    src_lba: u32,
    dst_lba: u32,
    num_sectors: u32,
) -> Result<(), u8> {
    let sector_size = src.sector_size();
    if sector_size != dst.sector_size() {
        return Err(3); // разные размеры секторов
    }

    let mut remaining = num_sectors;
    let mut current_src = src_lba;
    let mut current_dst = dst_lba;

    while remaining > 0 {
        // Максимум 255 секторов за раз (из-за u8 в трейте)
        let chunk = core::cmp::min(remaining, 8) as u8;
        let bytes = (chunk as u32) * sector_size;

        // Временный буфер
        let mut buf = vec![0u8; bytes as usize];

        // Читаем с источника
        src.read_sectors(chunk, current_src, buf.as_mut_ptr() as u32)?;
        // println!("{:02x?}", &buf);
        // Пишем на приёмник
        dst.write_sectors(chunk, current_dst, buf.as_ptr() as u32)?;
        let mut buf2 = vec![0u8; bytes as usize];
        dst.read_sectors(chunk, current_src, buf2.as_mut_ptr() as u32)?;

        compare_vecs_diff(&buf, &buf2);
        remaining -= chunk as u32;
        current_src += chunk as u32;
        current_dst += chunk as u32;
    }

    Ok(())
}
