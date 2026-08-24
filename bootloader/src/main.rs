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

/// Boot info handed to the kernel (phys 0x6000).
/// Kernel maps root from `disk_phys` when magic matches — no IDE needed (PXE).
const BOOTINFO_PHYS: u32 = 0x0000_6000;
const BOOTINFO_MAGIC: u32 = 0xFE11_B007;
/// Whole-disk image in RAM (after kernel @ 0x01000000).
const RAMDISK_PHYS: u32 = 0x0200_0000;
/// Fallback size if INT 13h AH=48h fails (matches Makefile 64 MiB disk.img).
const RAMDISK_FALLBACK_SECTORS: u32 = (64 * 1024 * 1024) / 512;

#[repr(C)]
struct BootInfo {
    magic: u32,
    disk_phys: u32,
    disk_sectors: u32,
    flags: u32,
}

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

    // ---- PXE/iPXE only: whole disk → RAM ----
    // Real IDE/HDD boot skips this; kernel mounts ATA directly.
    if is_network_boot() {
        use disk::DISK;
        let sectors = disk::Disk::drive_sector_count(RAMDISK_FALLBACK_SECTORS);
        println!("[!] Network boot — hydrating disk → RAM @ 0x{:08x} ({} sectors)", RAMDISK_PHYS, sectors);
        unsafe {
            DISK.init(0, KERNEL_BUFFER);
            DISK.copy_disk_to_ram(sectors, RAMDISK_PHYS);
        }
        write_bootinfo(RAMDISK_PHYS, sectors);
        println!("[!] BootInfo @ 0x{:08x}", BOOTINFO_PHYS);
    } else {
        clear_bootinfo();
        println!("[!] Local disk boot — skip RAM hydrate (kernel uses IDE)");
    }

    // VESA after the kernel is in memory so boot messages stay visible.
    println!("[!] Setting VESA graphics mode...");
    unsafe {
        let _ = vesa::init_vesa();
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

/// Detect PXE / iPXE SAN boot (not a real local IDE/SATA disk).
///
/// Checks (any one is enough):
/// 1. Classic PXE Installation Check — INT 1Ah AX=5650h → AL=50h
/// 2. "PXENV+" or "!PXE" signature in low memory / option ROM area
/// 3. "iPXE" string in 0xA0000..0xF0000 (iPXE option ROM / residual)
fn is_network_boot() -> bool {
    // if pxe_installation_check() {
    //     println!("[!] PXE installation check: yes");
    //     return true;
    // }
    if scan_signature(b"PXENV+") || scan_signature(b"!PXE") {
        println!("[!] Found PXENV+/!PXE signature");
        return true;
    }
    if scan_signature(b"iPXE") {
        println!("[!] Found iPXE signature");
        return true;
    }
    false
}

/// INT 1Ah, AX=5650h ("PX"). AL=50h means a PXE stack is installed.
fn pxe_installation_check() -> bool {
    let al: u16;
    unsafe {
        // Don't list ES as an asm operand — rustc/LLVM reject it on this target.
        // BIOS may clobber ES:BX; we only care about AL.
        asm!(
            "push es",
            "mov ax, 0x5650",
            "int 0x1A",
            "movzx {0:x}, al",
            "pop es",
            out(reg) al,
            out("ax") _,
            out("bx") _,
        );
    }
    al == 0x50
}

/// Scan phys 0x80000..0xF0000 for a short ASCII needle (16-bit real/unreal).
fn scan_signature(needle: &[u8]) -> bool {
    if needle.is_empty() {
        return false;
    }
    let start = 0x0008_0000u32;
    let end = 0x000F_0000u32;
    let n = needle.len();
    let mut addr = start;
    while addr + n as u32 <= end {
        let mut ok = true;
        for i in 0..n {
            let b = unsafe { core::ptr::read_volatile((addr + i as u32) as *const u8) };
            if b != needle[i] {
                ok = false;
                break;
            }
        }
        if ok {
            return true;
        }
        // step by 16 — signatures are usually paragraph-aligned in ROMs
        addr += 16;
    }
    false
}

fn clear_bootinfo() {
    unsafe {
        core::ptr::write_volatile(BOOTINFO_PHYS as *mut u32, 0);
    }
}

fn write_bootinfo(disk_phys: u32, disk_sectors: u32) {
    let info = BootInfo {
        magic: BOOTINFO_MAGIC,
        disk_phys,
        disk_sectors,
        flags: 1, // bit0 = ramdisk present
    };
    unsafe {
        core::ptr::write_volatile(BOOTINFO_PHYS as *mut BootInfo, info);
    }
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
