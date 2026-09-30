#![no_std]
#![no_main]

mod disk;

use core::arch::asm;
use core::arch::global_asm;
use core::panic::PanicInfo;
use disk::DiskReader;

// HDD/USB/PXE: sector 0 = MBR, 1..64 = bootloader (fits below 64K), 2048+ = ext2
const BOOTLOADER_LBA: u32 = 1;
const BOOTLOADER_SIZE: u16 = 64;

global_asm!(include_str!("boot.asm"));

#[unsafe(no_mangle)]
static mut BOOT_DRIVE: u8 = 0xFF;

pub(crate) fn boot_drive() -> u8 {
    unsafe { core::ptr::read_volatile(&raw const BOOT_DRIVE) }
}

unsafe extern "C" {
    static _bootloader_start: u16;
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> ! {
    clear();
    print(b"[!] Felix\r\n\0");
    print(b"[!] Load\r\n\0");

    let start = unsafe { &_bootloader_start as *const u16 };
    let disk = DiskReader::new(BOOTLOADER_LBA, start as u16);
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
            "jz 3f",
            "mov ah, 0x0e",
            "mov bh, 0",
            "out 0xe9, al",
            "int 0x10",
            "jmp 2b",
            "3:",
            in(reg) msg.as_ptr()
        );
    }
}

fn jump(addr: *const u16) {
    let drive = boot_drive() as u16;
    unsafe {
        // BIOS defines DL as the boot drive on entry. Preserve that ABI for
        // stage2 instead of assuming the boot disk is always 0x80.
        asm!(
            "jmp ax",
            in("ax") addr as u16,
            in("dx") drive,
            options(nostack)
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fail() -> ! {
    print(b"Fail\r\n\0");
    loop {}
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {}
}
