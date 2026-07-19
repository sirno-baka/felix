#![no_std]
#![no_main]

mod disk;

use core::arch::asm;
use core::arch::global_asm;
use core::panic::PanicInfo;
use disk::DiskReader;

// Floppy: sector 0 = boot, 1..64 = bootloader, 65+ = kernel
const BOOTLOADER_LBA: u16 = 1;
const BOOTLOADER_SIZE: u16 = 64;

global_asm!(include_str!("boot.asm"));

extern "C" {
    static _bootloader_start: u16;
}

#[no_mangle]
pub extern "C" fn main() -> ! {
    clear();
    print(b"[!] Felix\r\n\0");
    print(b"[!] Load\r\n\0");

    let start = unsafe { &_bootloader_start as *const u16 };
    let mut disk = DiskReader::new(BOOTLOADER_LBA, start as u16);
    disk.read_sectors(BOOTLOADER_SIZE);
    jump(start);
    loop {}
}

fn clear() {
    unsafe { asm!("mov ax, 0x0003", "int 0x10", options(nostack)); }
}

fn print(msg: &[u8]) {
    unsafe {
        asm!("mov si, {0:x}", //move given string address to si
            "2:",
            "lodsb", //load a byte (next character) from si to al
            "or al, al", //bitwise or on al, if al is null set zf to true
            "jz 1f", //if zf is true (end of string) jump to end

            "mov ah, 0x0e",
            "mov bh, 0",
            "out 0xe9, al", //e9 port hack
            "int 0x10", //tell the bios to write content of al to screen

            "jmp 2b", //start again
            "1:",
            in(reg) msg.as_ptr());
    }
}

fn jump(addr: *const u16) {
    unsafe { asm!("jmp {0:x}", in(reg) addr as u16, options(nostack)); }
}

#[no_mangle]
pub extern "C" fn fail() -> ! {
    print(b"Fail\r\n\0");
    loop {}
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! { loop {} }