// DISK READER — INT 0x13 AH=42 (LBA DAP)
// One DAP per request. Do not increment the buffer across sectors: that was
// leaving inode-table bytes past the first 512B as leftover directory data,
// so /kernel.bin (inode ~12, offset 1408) parsed as size=0.

use core::arch::asm;
use core::mem;

pub static mut DISK: Disk = Disk {
    lba: 0,
    buffer: 0,
};

#[repr(C, packed)]
struct DiskAddressPacket {
    size: u8,
    zero: u8,
    sectors: u16,
    offset: u16,
    segment: u16,
    lba: u64,
}

pub struct Disk {
    lba: u64,
    buffer: u16,
}

impl Disk {
    pub fn init(&mut self, lba: u64, buffer: u16) {
        self.lba = lba;
        self.buffer = buffer;
    }

    fn int13(&self, sectors: u16) {
        let dap = DiskAddressPacket {
            size: mem::size_of::<DiskAddressPacket>() as u8,
            zero: 0,
            sectors,
            offset: self.buffer,
            segment: 0x0000,
            lba: self.lba,
        };

        let dap_address = &dap as *const DiskAddressPacket as u16;

        // LLVM reserves ESI on this target — never list si as an asm operand.
        // Save/restore SI via a compiler-allocated register, same as the MBR reader.
        unsafe {
            asm!(
                "push ds",
                "push ax",
                "xor ax, ax",
                "mov ds, ax",
                "pop ax",
                "mov {1:x}, si",
                "mov si, {0:x}",
                "int 0x13",
                "jc fail",
                "cld",
                "mov si, {1:x}",
                "pop ds",
                in(reg) dap_address,
                out(reg) _,
                in("ax") 0x4200u16,
                in("dx") 0x0080u16,
            );
        }
    }

    pub fn read_sector(&self) {
        self.int13(1);
    }

    /// Read `sectors` into self.buffer in a single BIOS call.
    pub fn read_low(&self, sectors: u16) {
        self.int13(sectors);
    }

    /// Read `sectors` one at a time and copy each to high memory (unreal mode).
    pub fn read_sectors(&mut self, sectors: u16, target: u32) {
        let scratch = self.buffer;
        let mut dest = target;
        let mut left = sectors;
        while left > 0 {
            self.int13(1);

            let mut src = scratch as u32;
            let end = dest + 512;
            while dest < end {
                unsafe {
                    let mut byte: u8;
                    asm!("mov {0}, [{1:e}]", out(reg_byte) byte, in(reg) src);
                    asm!("mov [{0:e}], {1}", in(reg) dest, in(reg_byte) byte);
                }
                src += 1;
                dest += 1;
            }

            self.lba += 1;
            left -= 1;
            let read = sectors - left;
            if read % 64 == 0 {
                print!(".");
            }
        }
    }
}
