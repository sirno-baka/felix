#![no_std]
#![no_main]

#[macro_use]
mod print;

mod disk;
mod ext2;
mod gdt;
mod splash;
mod tss;
mod vesa;

use core::arch::asm;
use core::panic::PanicInfo;
use disk::DISK;
use ext2::Ext2;
use gdt::GDT;

/// Absolute LBA of the first (and only) partition — must match disk.layout.
const PART_LBA: u32 = 2048;

/// Where to put the kernel image in physical memory (matches kernel linker).
const KERNEL_TARGET: u32 = 0x0100_0000;

/// Path on the ext2 root of the partition.
const KERNEL_PATH: &[u8] = b"/boot/kernel.bin";

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("PANIC! Info: {}", info);
    loop {}
}

#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() -> ! {
    gdt::GlobalDescriptorTable::init();
    unsafe {
        GDT.load();
    }

    print!("[!] Felix bootloader\r\n");
    enable_a20();
    println!("[!] Unreal mode...");
    unreal_mode();

    println!("[!] Mounting ext2 at LBA {}", PART_LBA);
    let fs = match Ext2::mount(PART_LBA) {
        Some(f) => f,
        None => {
            println!("[!] ext2 mount failed");
            loop {}
        }
    };

    println!("[!] Loading /boot/kernel.bin ...");
    match fs.load_path(KERNEL_PATH, KERNEL_TARGET) {
        Some(sz) => println!("[!] Kernel {} bytes @ {:#x}", sz, KERNEL_TARGET),
        None => {
            println!("[!] kernel not found or too large");
            loop {}
        }
    }

    unsafe {
        GDT.load();
    }
    println!("[!] Protected mode → kernel");
    protected_mode();
    loop {}
}

#[no_mangle]
pub extern "C" fn fail() -> ! {
    println!("[!] Read fail!");
    loop {}
}

fn protected_mode() {
    unsafe {
        GDT.load();
        asm!(
            "mov eax, cr0",
            "or eax, 1",
            "mov cr0, eax",
            options(nostack, preserves_flags)
        );
        asm!(
            "lea eax, [2f]",
            "push 0x08",
            "push eax",
            "retf",
            "2:",
            options(nostack)
        );
        asm!(
            ".code32",
            "mov ax, 0x10",
            "mov ds, ax",
            "mov es, ax",
            "mov fs, ax",
            "mov gs, ax",
            "mov ss, ax",
            "mov esp, 0x90000",
            "mov eax, 0x01000000",
            "call eax",
            "3:",
            "hlt",
            "jmp 3b",
            options(nostack)
        );
    }
}

fn unreal_mode() {
    let ds: u16;
    let ss: u16;
    unsafe {
        asm!("mov {0:x}, ds", out(reg) ds);
        asm!("mov {0:x}, ss", out(reg) ss);
        GDT.load();
        let mut cr0: u32;
        asm!("mov {0:e}, cr0", out(reg) cr0);
        let cr0_protected = cr0 | 1;
        asm!("mov cr0, {0:e}", in(reg) cr0_protected);
        asm!("mov {0:x}, 0x10", "mov ds, {0:x}", "mov ss, {0:x}", out(reg) _);
        asm!("mov cr0, {0:e}", in(reg) cr0);
        asm!("mov ds, {0:x}", in(reg) ds);
        asm!("mov ss, {0:x}", in(reg) ss);
    }
}

fn enable_a20() {
    unsafe {
        let mut val: u8;
        asm!("in al, 0x92", out("al") val);
        if (val & 2) == 0 {
            val |= 2;
            val &= !1;
            asm!("out 0x92, al", in("al") val);
        }
        asm!(
            "mov ax, 0x2401",
            "int 0x15",
            lateout("ax") _,
            options(nostack, preserves_flags),
        );
    }
}
