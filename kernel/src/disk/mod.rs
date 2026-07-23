use crate::alloc::boxed::Box;
use crate::alloc::vec::Vec;
use crate::pci::ide::IDE;

pub mod ide;

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


