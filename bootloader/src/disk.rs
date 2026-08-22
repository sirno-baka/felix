//! Disk reader — INT 13h LBA (AH=42) for hard disk (DL=0x80).
//! Falls back is not implemented; QEMU IDE disk uses LBA.

use core::arch::asm;
use crate::print;

pub static mut DISK: Disk = Disk {
    lba: 0,
    buffer: 0,
    drive: 0x80,
};

const SECTOR_SIZE: u32 = 512;
const MAX_RETRIES: u8 = 5;

/// Disk Address Packet for INT 13h extensions.
#[repr(C, packed)]
struct Dap {
    size: u8,
    reserved: u8,
    count: u16,
    offset: u16,
    segment: u16,
    lba: u64,
}

pub struct Disk {
    lba: u32,
    buffer: u16,
    drive: u8,
}

impl Disk {
    pub fn init(&mut self, lba: u32, buffer: u16) {
        self.lba = lba;
        self.buffer = buffer;
        self.drive = 0x80;
    }

    pub fn set_drive(&mut self, drive: u8) {
        self.drive = drive;
    }

    pub(crate) fn reset(&self) {
        let dl = self.drive;
        unsafe {
            asm!(
                "xor ax, ax",
                "int 0x13",
                in("dl") dl,
                lateout("ax") _,
                options(nostack),
            );
        }
    }

    /// Read `count` sectors starting at self.lba into conventional buffer,
    /// then copy to physical `target` (may be >1MiB via unreal/protected movsb).
    pub fn read_sectors(&mut self, count: u16, target: u32) {
        let mut remaining = count;
        let mut dst = target;
        let mut lba = self.lba;

        while remaining > 0 {
            // BIOS often limits to 127 sectors; we use 1 for simplicity/reliability
            let batch: u16 = 1;

            for _attempt in 0..MAX_RETRIES {
                let dap = Dap {
                    size: 16,
                    reserved: 0,
                    count: batch,
                    offset: self.buffer,
                    segment: 0,
                    lba: lba as u64,
                };
                let dap_ptr = &dap as *const Dap as u16;
                let mut err: u16;
                let dl = self.drive;
                unsafe {
                    asm!("sti", options(nostack));
                    asm!(
                        "mov ah, 0x42",
                        "int 0x13",
                        "mov ax, 0",
                        "jnc 1f",
                        "inc ax",
                        "1:",
                        in("dl") dl,
                        in("si") dap_ptr,
                        lateout("ax") err,
                        options(nostack),
                    );
                }
                if err == 0 {
                    // copy 512 bytes buffer -> dst
                    let mut src = self.buffer as u32;
                    for _ in 0..SECTOR_SIZE {
                        unsafe {
                            let mut b: u8;
                            asm!("mov {0}, [{1:e}]", out(reg_byte) b, in(reg) src, options(nostack));
                            asm!("mov [{0:e}], {1}", in(reg) dst, in(reg_byte) b, options(nostack));
                        }
                        src += 1;
                        dst += 1;
                    }
                    lba += batch as u32;
                    remaining -= batch;
                    break;
                }
                self.reset();
                if _attempt + 1 == MAX_RETRIES {
                    println!("[disk] LBA read fail lba={}", lba);
                    unsafe { asm!("jmp fail", options(noreturn)); }
                }
            }
        }
        self.lba = lba;
    }
}
