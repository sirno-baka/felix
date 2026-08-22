#![no_std]
#![no_main]

mod disk;

use core::arch::asm;
use core::arch::global_asm;
use core::panic::PanicInfo;
use disk::DiskReader;

// MBR gap: bootloader starts at LBA 1 (right after this sector), size 64 sectors.
const BOOTLOADER_LBA: u16 = 1;
const BOOTLOADER_SIZE: u16 = 64;

global_asm!(include_str!("boot.asm"));

extern "C" {
    static _bootloader_start: u16;
}

#[no_mangle]
pub extern "C" fn main() -> ! {
    clear();
    print(b"[!] Felix MBR\r\n\0");
    print(b"[!] Loading stage2\r\n\0");

    let start = unsafe { &_bootloader_start as *const u16 };
    let mut disk = DiskReader::new(BOOTLOADER_LBA, start as u16);
    disk.read_sectors(BOOTLOADER_SIZE);
    jump(start);
    loop {}
}

fn clear() {
    unsafe {
        asm!("mov ax, 0x0003", "int 0x10", options(nostack));
    }
}

fn print(msg: &[u8]) {
    unsafe {
        asm!(
            "mov si, {0:x}",
            "2:",
            "lodsb",
            "or al, al",
            "jz 1f",
            "mov ah, 0x0e",
            "mov bh, 0",
            "out 0xe9, al",
            "int 0x10",
            "jmp 2b",
            "1:",
            in(reg) msg.as_ptr()
        );
    }
}

fn jump(addr: *const u16) {
    unsafe {
        asm!("jmp {0:x}", in(reg) addr as u16, options(nostack));
    }
}

#[no_mangle]
pub extern "C" fn fail() -> ! {
    print(b"Fail\r\n\0");
    loop {}
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {}
}
