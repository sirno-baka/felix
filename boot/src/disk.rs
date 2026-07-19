// Minimal CHS floppy reader (INT 13h AH=02). No DAP.
// lba is u16 — enough for 1.44MB floppy (max ~2880). Avoids fat 64-bit div.

use core::arch::asm;

const SECTOR_SIZE: u16 = 512;
const SPT: u16 = 18;
const HEADS: u16 = 2;

pub struct DiskReader {
    lba: u16,
    target: u16,
}

impl DiskReader {
    pub fn new(lba: u16, target: u16) -> Self {
        Self { lba, target }
    }

    pub fn read_sector(&self) {
        // LBA → CHS (all 16-bit, tiny code)
        let sector = (self.lba % SPT) + 1;
        let temp = self.lba / SPT;
        let head = temp % HEADS;
        let cyl = temp / HEADS;

        let cx = (cyl << 8) | sector;
        let dx = head << 8; // DL=0

        unsafe {
            asm!(
            "xor ax, ax",
            "mov es, ax",
            "mov ax, 0x0201",
            "int 0x13",
            "jc fail",
            in("bx") self.target,
            in("cx") cx,
            in("dx") dx,
            lateout("ax") _,
            options(nostack),
            );
        }
    }

    pub fn read_sectors(&mut self, mut n: u16) {
        while n != 0 {
            self.read_sector();
            self.target = self.target.wrapping_add(SECTOR_SIZE);
            self.lba = self.lba.wrapping_add(1);
            n -= 1;
        }
    }
}