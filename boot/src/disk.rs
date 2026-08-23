// MBR disk reader — INT 13h AH=42 (LBA). Fits in 440 bytes of boot code.
// Reads the stage2 bootloader from the disk gap after the MBR.

use core::arch::asm;
use core::mem;

#[repr(C, packed)]
struct DiskAddressPacket {
    size: u8,
    zero: u8,
    sectors: u16,
    offset: u16,
    segment: u16,
    lba_low: u32,
    lba_high: u32,
}

pub struct DiskReader {
    lba: u32,
    target: u16,
}

impl DiskReader {
    pub fn new(lba: u32, target: u16) -> Self {
        Self { lba, target }
    }

    pub fn read_sectors(&self, sectors: u16) {
        let dap = DiskAddressPacket {
            size: mem::size_of::<DiskAddressPacket>() as u8,
            zero: 0,
            sectors,
            offset: self.target,
            segment: 0x0000,
            lba_low: self.lba,
            lba_high: 0,
        };

        let dap_address = &dap as *const DiskAddressPacket;

        unsafe {
            asm!(
                "mov {1:x}, si",
                "mov si, {0:x}",
                "int 0x13",
                "jc fail",
                "mov si, {1:x}",
                in(reg) dap_address as u16,
                out(reg) _,
                in("ax") 0x4200u16,
                in("dx") 0x0080u16,
            );
        }
    }
}
