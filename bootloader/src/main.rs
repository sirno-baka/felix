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
use crate::gdt::GDT;
use ext2::Ext2Fs;

const KERNEL_BUFFER: u16 = 0x1000; // low-memory transfer buffer (below bootloader)
const KERNEL_TARGET: u32 = 0x0100_0000; // where to put kernel in memory
const KERNEL_PATH: &str = "/kernel.bin";

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

    enable_a20();
    println!("[!] Switching to 16bit unreal mode...");
    unreal_mode();

    let part_lba = ext2::find_ext2_part_lba();
    println!("[!] Mounting ext2 at LBA {}", part_lba);

    let fs = match Ext2Fs::mount(part_lba, KERNEL_BUFFER) {
        Some(fs) => fs,
        None => {
            println!("[!] ext2 mount failed");
            loop {}
        }
    };

    println!("[!] Loading {}", KERNEL_PATH);
    match fs.load_file(KERNEL_PATH, KERNEL_TARGET) {
        Some(size) => println!("[!] Kernel loaded ({} bytes)", size),
        None => {
            println!("[!] Failed to load {}", KERNEL_PATH);
            loop {}
        }
    }

    // VESA after the kernel is in memory so boot messages stay visible.
    println!("[!] Setting VESA graphics mode...");
    unsafe {
        // let _ = vesa::init_vesa();
    }

    restore_unreal();

    println!("[!] Switching to 32bit protected mode and jumping to kernel...");
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

/// iPXE / BIOS INT 13h reloads segments and kills the 4GB unreal-mode limits.
/// Call this before any access above 1MB.
pub(crate) fn restore_unreal() {
    enable_a20_fast();
    unreal_mode();
}

fn unreal_mode() {
    let ds: u16;
    let es: u16;
    let ss: u16;
    unsafe {
        asm!("mov {0:x}, ds", out(reg) ds);
        asm!("mov {0:x}, es", out(reg) es);
        asm!("mov {0:x}, ss", out(reg) ss);
    }

    unsafe {
        GDT.load();
    }

    unsafe {
        let mut cr0: u32;
        asm!("mov {0:e}, cr0", out(reg) cr0);

        let cr0_protected = cr0 | 1;
        asm!("mov cr0, {0:e}", in(reg) cr0_protected);

        asm!(
            "mov ax, 0x10",
            "mov ds, ax",
            "mov es, ax",
            "mov fs, ax",
            "mov gs, ax",
            "mov ss, ax",
            out("ax") _,
        );

        asm!("mov cr0, {0:e}", in(reg) cr0);

        asm!("mov ds, {0:x}", in(reg) ds);
        asm!("mov es, {0:x}", in(reg) es);
        asm!("mov ss, {0:x}", in(reg) ss);
    }
}

fn enable_a20_fast() {
    unsafe {
        let mut val: u8;
        asm!("in al, 0x92", out("al") val);
        if (val & 2) == 0 {
            val |= 2;
            val &= !1;
            asm!("out 0x92, al", in("al") val);
        }
    }
}

fn enable_a20() {
    enable_a20_fast();
    unsafe {
        let mut ax: u16;
        asm!(
            "mov ax, 0x2401",
            "int 0x15",
            "mov {0:x}, ax",
            lateout(reg) ax,
            options(nostack, preserves_flags),
        );
    }
}

#[allow(dead_code)]
fn wait_for_key() {
    unsafe {
        asm!("int 0x16", in("ah") 0x00 as u8);
    }
}
