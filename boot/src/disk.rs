//! MBR stage: load bootloader via INT 13h LBA from HDD (DL=0x80).

use core::arch::asm;

#[repr(C, packed)]
struct Dap {
    size: u8,
    reserved: u8,
    count: u16,
    offset: u16,
    segment: u16,
    lba: u64,
}

pub struct DiskReader {
    lba: u32,
    buffer: u16,
}

impl DiskReader {
    pub fn new(lba: u16, buffer: u16) -> Self {
        Self {
            lba: lba as u32,
            buffer,
        }
    }

    pub fn read_sectors(&mut self, count: u16) {
        let mut remaining = count;
        let mut lba = self.lba;
        let mut buf = self.buffer;

        while remaining > 0 {
            let dap = Dap {
                size: 16,
                reserved: 0,
                count: 1,
                offset: buf,
                segment: 0,
                lba: lba as u64,
            };
            let dap_ptr = &dap as *const Dap as u16;
            let mut err: u16;
            unsafe {
                asm!("sti", options(nostack));
                asm!(
                    "mov ah, 0x42",
                    "mov dl, 0x80",
                    "int 0x13",
                    "mov ax, 0",
                    "jnc 1f",
                    "inc ax",
                    "1:",
                    in("si") dap_ptr,
                    lateout("ax") err,
                    options(nostack),
                );
            }
            if err != 0 {
                unsafe {
                    asm!("jmp fail", options(noreturn));
                }
            }
            lba += 1;
            buf = buf.wrapping_add(512);
            remaining -= 1;
        }
    }
}
