#![no_std]
#![no_main]
#![allow(static_mut_refs)]
#[macro_use]
mod print;

mod disk;
mod ext2;
mod gdt;
mod splash;
mod tss;
mod vesa;

use crate::gdt::GDT;
use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use ext2::Ext2Fs;

const KERNEL_BUFFER: u16 = 0x1000; // low-memory BIOS bounce buffer
const KERNEL_TARGET: u32 = 0x0100_0000; // where to put kernel in memory
const KERNEL_PATH: &str = "/kernel.bin";

global_asm!(include_str!("entry.asm"));

#[unsafe(no_mangle)]
static mut STAGE2_BOOT_DRIVE: u8 = 0xFF;

pub(crate) fn boot_drive() -> u8 {
    unsafe { core::ptr::read_volatile(&raw const STAGE2_BOOT_DRIVE) }
}

/// Boot info handed to the kernel (phys 0x7000).
/// Kernel maps root from `disk_phys` when magic matches — no IDE needed (USB/PXE/CSM).
const BOOTINFO_PHYS: u32 = 0x0000_7000; // not 0x6000 — VESA uses 0x6000 as scratch
const BOOTINFO_MAGIC: u32 = 0xFE11_B007;
/// Whole-disk image in RAM, immediately after the fixed kernel heap.
/// The heap occupies phys 0x01800000..0x02800000, so placing the old ramdisk
/// at 0x02000000 overwrote its final 8 MiB during network boot.
const RAMDISK_PHYS: u32 = 0x0280_0000;
/// Fallback size if INT 13h AH=48h fails (matches Makefile's 64 MiB disk.img).
const RAMDISK_FALLBACK_SECTORS: u32 = (64 * 1024 * 1024) / 512;

#[repr(C)]
struct BootInfo {
    magic: u32,
    disk_phys: u32,
    disk_sectors: u32,
    flags: u32,
    /// Low/direct-mapped RAM usable by the current kernel allocator.
    mem_bytes: u32,
    /// Installed/addressable RAM reported by firmware, in MiB. This is kept
    /// separately because exactly 4 GiB does not fit in a u32 byte count.
    mem_total_mib: u32,
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("PANIC! Info: {}", info);
    loop {}
}

#[unsafe(no_mangle)]
pub extern "C" fn stage2_main() -> ! {
    gdt::GlobalDescriptorTable::init();

    unsafe {
        GDT.load();
    }

    enable_a20();
    println!("[!] BIOS boot drive: 0x{:02x}", boot_drive());
    println!("[!] Switching to 16bit unreal mode...");
    unreal_mode();

    // Keep the pre-kernel path deliberately minimal and BIOS-only.
    // This is the path already proven on the Xiaomi CSM BIOS: each ext2 block
    // is read through the low 0x1000 buffer, then copied to high memory.
    // No PCI scan, PXE scan or direct ATA command is allowed before the kernel
    // is fully resident at KERNEL_TARGET.
    disk::set_fast_high_reads(false);

    let part_lba = ext2::find_ext2_part_lba();
    println!("[!] Mounting ext2 at LBA {}", part_lba);
    let fs = match Ext2Fs::mount(part_lba, KERNEL_BUFFER) {
        Some(fs) => fs,
        None => {
            println!("[!] ext2 mount failed");
            loop {}
        }
    };

    let edd = disk::Disk::edd_version();
    println!(
        "[!] Loading {} via EDD={}.{} (pure-bios)",
        KERNEL_PATH,
        edd >> 4,
        edd & 0x0f,
    );
    match fs.load_file(KERNEL_PATH, KERNEL_TARGET) {
        Some(size) => println!("[!] Kernel loaded ({} bytes)", size),
        None => {
            println!("[!] Failed to load {}", KERNEL_PATH);
            loop {}
        }
    }

    // Hardware/source detection starts only after the kernel is loaded.
    let network_hint = has_network_boot_signature();
    let legacy_ide_hint = has_legacy_pci_ide_controller();
    if legacy_ide_hint {
        println!("[!] Legacy PCI IDE controller found");
    }
    let primary_ata = if network_hint || legacy_ide_hint {
        // Compare the physical ATA MBR with the BIOS-loaded MBR. A machine
        // may have an IDE controller while actually booting this image via USB.
        booted_from_primary_ata()
    } else {
        false
    };
    let network_boot = network_hint && detect_network_boot(primary_ata);

    // Always publish RAM size. PXE keeps its old RAM-disk behavior.
    // Legacy IDE remains zero-copy. USB/non-ATA copies only the Felix image
    // described by the MBR, never the full physical USB-stick capacity.
    let (mem_bytes, mem_total_mib) = detect_memory();
    println!(
        "[!] Detected RAM: {} MiB ({} MiB managed lowmem)",
        mem_total_mib,
        mem_bytes / (1024 * 1024)
    );

    if network_boot {
        use disk::DISK;
        disk::set_fast_high_reads(false); // preserve the old PXE path exactly
        let sectors = disk::Disk::drive_sector_count(RAMDISK_FALLBACK_SECTORS);
        if !ramdisk_fits(sectors, mem_bytes) {
            println!(
                "[!] RAM disk does not fit: start=0x{:08x}, sectors={}, RAM={} MiB",
                RAMDISK_PHYS,
                sectors,
                mem_bytes / (1024 * 1024)
            );
            loop {}
        }
        println!(
            "[!] Network boot — hydrating disk -> RAM @ 0x{:08x} ({} sectors)",
            RAMDISK_PHYS, sectors
        );
        unsafe {
            DISK.init(0, KERNEL_BUFFER);
            DISK.copy_disk_to_ram(sectors, RAMDISK_PHYS);
        }
        write_bootinfo(RAMDISK_PHYS, sectors, mem_bytes, mem_total_mib);
        println!("[!] BootInfo @ 0x{:08x}", BOOTINFO_PHYS);
    } else if primary_ata {
        write_bootinfo(0, 0, mem_bytes, mem_total_mib);
        println!("[!] Local IDE boot — kernel uses IDE");
    } else {
        use disk::DISK;
        // Prefer the conservative low-memory BIOS path for USB too. Once this
        // is confirmed on real hardware we can optimize RAM hydration separately.
        disk::set_fast_high_reads(false);
        let sectors = ext2::image_sectors_from_mbr().unwrap_or(RAMDISK_FALLBACK_SECTORS);
        let physical = disk::Disk::drive_sector_count(sectors);
        println!(
            "[!] USB/non-ATA: Felix image={} sectors, physical BIOS disk={} sectors",
            sectors, physical
        );
        if !ramdisk_fits(sectors, mem_bytes) {
            println!(
                "[!] Felix image RAM copy does not fit: start=0x{:08x}, sectors={}, RAM={} MiB",
                RAMDISK_PHYS,
                sectors,
                mem_bytes / (1024 * 1024)
            );
            loop {}
        }
        println!(
            "[!] USB/non-ATA — hydrating Felix image -> RAM @ 0x{:08x} ({} sectors)",
            RAMDISK_PHYS, sectors
        );
        unsafe {
            DISK.init(0, KERNEL_BUFFER);
            DISK.copy_disk_to_ram(sectors, RAMDISK_PHYS);
        }
        write_bootinfo(RAMDISK_PHYS, sectors, mem_bytes, mem_total_mib);
        println!("[!] BootInfo @ 0x{:08x}", BOOTINFO_PHYS);
    }

    // Clear FB_INFO so kernel does not treat garbage as a valid LFB.
    unsafe {
        core::ptr::write_bytes(
            vesa::FB_INFO_PHYS as *mut u8,
            0,
            core::mem::size_of::<vesa::FramebufferInfo>(),
        );
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

#[unsafe(no_mangle)]
pub extern "C" fn fail() -> ! {
    println!("[!] Read fail!");
    loop {}
}

/// Detect PXE / iPXE SAN boot (not a real local IDE/SATA disk).
///
/// A matching physical ATA boot sector takes precedence. Without one, PXE
/// signatures indicate that INT 13h is backed by a network/SAN provider.
fn has_network_boot_signature() -> bool {
    scan_signature(b"PXENV+") || scan_signature(b"!PXE") || scan_signature(b"iPXE")
}

/// Passive PCI class-code check. Old machines with a real PCI IDE controller
/// keep the historical low-memory kernel loader, without issuing ATA commands.
fn has_legacy_pci_ide_controller() -> bool {
    for dev in 0u8..32 {
        for func in 0u8..8 {
            let id = pci_config_read(0, dev, func, 0x00);
            if id == 0xffff_ffff || (id & 0xffff) == 0xffff {
                if func == 0 {
                    break;
                }
                continue;
            }

            let class_reg = pci_config_read(0, dev, func, 0x08);
            let class = ((class_reg >> 24) & 0xff) as u8;
            let subclass = ((class_reg >> 16) & 0xff) as u8;
            if class == 0x01 && subclass == 0x01 {
                return true;
            }

            if func == 0 {
                let hdr = pci_config_read(0, dev, 0, 0x0c);
                if ((hdr >> 16) & 0x80) == 0 {
                    break;
                }
            }
        }
    }
    false
}

fn pci_config_read(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    let address = 0x8000_0000u32
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xfc);
    let value: u32;
    unsafe {
        asm!(
            "out dx, eax",
            in("dx") 0x0cf8u16,
            in("eax") address,
            options(nomem, nostack, preserves_flags)
        );
        asm!(
            "in eax, dx",
            in("dx") 0x0cfcu16,
            out("eax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

fn detect_network_boot(primary_ata: bool) -> bool {
    // Preserve the old precedence: a matching physical ATA MBR means this is
    // a local IDE boot even if a PXE option ROM happens to be installed.
    if primary_ata {
        println!("[!] Boot disk matches physical IDE ATA");
        return false;
    }

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

fn ramdisk_fits(sectors: u32, mem_bytes: u32) -> bool {
    sectors
        .checked_mul(512)
        .and_then(|bytes| RAMDISK_PHYS.checked_add(bytes))
        .is_some_and(|end| end <= mem_bytes)
}

#[inline]
fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe { asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack)); }
    value
}

#[inline]
fn outb(port: u16, value: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack)); }
}

/// Probe the legacy primary IDE channel without relying on BIOS INT 13h.
/// A bounded poll is important because this also runs on machines with no IDE.
fn booted_from_primary_ata() -> bool {
    const DATA: u16 = 0x1f0;
    const SECTOR_COUNT: u16 = 0x1f2;
    const LBA_LOW: u16 = 0x1f3;
    const LBA_MID: u16 = 0x1f4;
    const LBA_HIGH: u16 = 0x1f5;
    const DRIVE: u16 = 0x1f6;
    const STATUS_COMMAND: u16 = 0x1f7;
    const CMD_IDENTIFY: u8 = 0xec;
    const STATUS_ERR: u8 = 1 << 0;
    const STATUS_DRQ: u8 = 1 << 3;
    const STATUS_BSY: u8 = 1 << 7;

    for drive in [0xa0u8, 0xb0u8] {
        outb(DRIVE, drive);
        // ATA requires a short settling delay after selecting a device.
        for _ in 0..4 { let _ = inb(STATUS_COMMAND); }
        outb(SECTOR_COUNT, 0);
        outb(LBA_LOW, 0);
        outb(LBA_MID, 0);
        outb(LBA_HIGH, 0);
        outb(STATUS_COMMAND, CMD_IDENTIFY);

        let first = inb(STATUS_COMMAND);
        if first == 0 || first == 0xff {
            continue;
        }
        for _ in 0..100_000 {
            let status = inb(STATUS_COMMAND);
            if status & STATUS_BSY != 0 {
                core::hint::spin_loop();
                continue;
            }
            // Non-zero signature registers indicate ATAPI or another device,
            // not the ATA disk the kernel can later use as its IDE root.
            if inb(LBA_MID) != 0 || inb(LBA_HIGH) != 0 {
                break;
            }
            if status & STATUS_ERR != 0 {
                break;
            }
            if status & STATUS_DRQ != 0 {
                // Drain IDENTIFY data so the channel is idle for later BIOS IO.
                for _ in 0..256 {
                    unsafe {
                        asm!("in ax, dx", in("dx") DATA, out("ax") _, options(nomem, nostack));
                    }
                }
                if ata_boot_sector_matches(drive) {
                    return true;
                }
                break;
            }
        }
    }
    false
}

/// Compare the physical ATA MBR with the boot sector still resident at 0x7c00.
/// This distinguishes an actual IDE boot from PXE on systems that merely also
/// have an IDE disk attached.
fn ata_boot_sector_matches(drive: u8) -> bool {
    const DATA: u16 = 0x1f0;
    const SECTOR_COUNT: u16 = 0x1f2;
    const LBA_LOW: u16 = 0x1f3;
    const LBA_MID: u16 = 0x1f4;
    const LBA_HIGH: u16 = 0x1f5;
    const DRIVE: u16 = 0x1f6;
    const STATUS_COMMAND: u16 = 0x1f7;
    const CMD_READ_SECTORS: u8 = 0x20;
    const STATUS_ERR: u8 = 1 << 0;
    const STATUS_DRQ: u8 = 1 << 3;
    const STATUS_BSY: u8 = 1 << 7;

    outb(DRIVE, drive | 0x40); // LBA mode, sector 0
    for _ in 0..4 { let _ = inb(STATUS_COMMAND); }
    outb(SECTOR_COUNT, 1);
    outb(LBA_LOW, 0);
    outb(LBA_MID, 0);
    outb(LBA_HIGH, 0);
    outb(STATUS_COMMAND, CMD_READ_SECTORS);

    for _ in 0..100_000 {
        let status = inb(STATUS_COMMAND);
        if status & STATUS_BSY != 0 {
            core::hint::spin_loop();
            continue;
        }
        if status & STATUS_ERR != 0 {
            return false;
        }
        if status & STATUS_DRQ != 0 {
            let mut matches = true;
            for i in 0..256usize {
                let word: u16;
                unsafe {
                    asm!("in ax, dx", in("dx") DATA, out("ax") word, options(nomem, nostack));

                    // Stage1 stores BIOS DL in its own .data inside the loaded
                    // MBR image, so comparing all 512 resident bytes is no
                    // longer valid. The MBR metadata area is never modified:
                    // disk-id + partition table + 0x55AA are sufficient to
                    // identify that BIOS booted from this physical ATA disk.
                    let byte_off = i * 2;
                    if byte_off >= 0x1B8 {
                        let boot_word =
                            core::ptr::read_volatile((0x7c00usize as *const u16).add(i));
                        if word != boot_word {
                            matches = false;
                        }
                    }
                }
            }
            return matches;
        }
    }
    false
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

#[derive(Clone, Copy)]
struct MemoryInfo {
    managed_bytes: u32,
    total_mib: u32,
}

#[repr(C, packed)]
struct E820Entry {
    base: u64,
    length: u64,
    kind: u32,
    attrs: u32,
}

/// INT 15h E820 memory map. We need the usable low-RAM region containing the
/// bootloader ramdisk destination, not the highest address in the machine
/// (which could sit above PCI/MMIO holes). Cap at 1 GiB for the current 32-bit
/// physical allocator; this is still far beyond what boot-time RAM hydration
/// needs and avoids turning a 16/32 GiB laptop into enormous early page tables.
fn detect_memory_e820() -> Option<MemoryInfo> {
    const SMAP: u32 = 0x534D_4150;
    const ENTRY_PHYS: usize = 0x0000_5000;
    const MAX_MANAGED_RAM: u64 = 1024 * 1024 * 1024;
    const NON_PAE_LIMIT: u64 = 4 * 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    let mut continuation = 0u32;
    let mut low_usable_end = 0u64;
    let mut highest_usable_end = 0u64;
    let mut iterations = 0u16;

    loop {
        unsafe {
            core::ptr::write_bytes(ENTRY_PHYS as *mut u8, 0, core::mem::size_of::<E820Entry>());
        }

        let mut eax = 0xE820u32;
        let mut ebx = continuation;
        let mut ecx = core::mem::size_of::<E820Entry>() as u32;
        unsafe {
            asm!(
                "push ds",
                "push es",
                "push di",
                "push ax",
                "xor ax, ax",
                "mov es, ax",
                "pop ax",
                "mov di, 0x5000",
                "int 0x15",
                "pop di",
                "pop es",
                "pop ds",
                inout("eax") eax,
                inout("ebx") ebx,
                inout("ecx") ecx,
                in("edx") SMAP,
            );
        }

        if eax != SMAP || ecx < 20 {
            break;
        }

        let entry = unsafe { core::ptr::read_unaligned(ENTRY_PHYS as *const E820Entry) };
        let base = entry.base;
        let length = entry.length;
        let kind = entry.kind;
        if kind == 1 && length != 0 {
            let end = base.saturating_add(length);
            highest_usable_end = core::cmp::max(highest_usable_end, end);
            let ramdisk = RAMDISK_PHYS as u64;
            if base <= ramdisk && end > ramdisk {
                low_usable_end = core::cmp::max(low_usable_end, end);
            }
        }

        continuation = ebx;
        iterations = iterations.saturating_add(1);
        if continuation == 0 || iterations >= 128 {
            break;
        }
    }

    if low_usable_end <= RAMDISK_PHYS as u64 {
        return None;
    }

    let managed = core::cmp::min(low_usable_end, MAX_MANAGED_RAM) as u32;
    let total_mib = ((core::cmp::min(highest_usable_end, NON_PAE_LIMIT) + MIB - 1) / MIB)
        .max((managed as u64) / MIB) as u32;
    Some(MemoryInfo {
        managed_bytes: managed,
        total_mib,
    })
}

/// Prefer E820 on modern BIOS/CSM firmware. Old machines retain the previous
/// E801 + CMOS fallbacks unchanged. The current kernel can *report* the full
/// non-PAE 4 GiB address space, while its permanent direct map remains capped
/// at 1 GiB until highmem/kmap support is added.
fn detect_memory() -> (u32, u32) {
    const MAX_MANAGED_RAM: u64 = 1024 * 1024 * 1024;
    const NON_PAE_LIMIT: u64 = 4 * 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    if let Some(info) = detect_memory_e820() {
        println!(
            "[mem] E820 RAM: {} MiB total, {} MiB managed lowmem",
            info.total_mib,
            info.managed_bytes / (1024 * 1024)
        );
        return (info.managed_bytes, info.total_mib);
    }

    // INT 15h AX=E801 — old BIOS fallback.
    let mut ax: u16;
    let mut bx: u16;
    let mut cx: u16;
    let mut dx: u16;
    let mut cf: u16;
    unsafe {
        asm!(
            "mov ax, 0xE801",
            "int 0x15",
            "mov {cf:x}, 0",
            "jnc 3f",
            "mov {cf:x}, 1",
            "3:",
            out("ax") ax,
            out("bx") bx,
            out("cx") cx,
            out("dx") dx,
            cf = out(reg) cf,
        );
    }
    if cf == 0 {
        let (kb_1_16, blocks_64k) = if ax != 0 || bx != 0 {
            (ax as u64, bx as u64)
        } else {
            (cx as u64, dx as u64)
        };
        let total = (1024u64 * 1024) + kb_1_16 * 1024 + blocks_64k * 64 * 1024;
        if total >= 16 * MIB {
            let visible = core::cmp::min(total, NON_PAE_LIMIT);
            let managed = core::cmp::min(visible, MAX_MANAGED_RAM) as u32;
            return (managed, ((visible + MIB - 1) / MIB) as u32);
        }
    }

    // CMOS fallback: extended memory KB at 0x17/0x18.
    let lo: u8;
    let hi: u8;
    unsafe {
        asm!("mov al, 0x17", "out 0x70, al", "in al, 0x71", out("al") lo);
        asm!("mov al, 0x18", "out 0x70, al", "in al, 0x71", out("al") hi);
    }
    let ext_kb = (lo as u32) | ((hi as u32) << 8);
    let total = if ext_kb > 0 {
        (1024u64 + ext_kb as u64) * 1024
    } else {
        64 * MIB
    };
    let managed = core::cmp::min(total, MAX_MANAGED_RAM) as u32;
    (managed, ((total + MIB - 1) / MIB) as u32)
}

fn write_bootinfo(disk_phys: u32, disk_sectors: u32, mem_bytes: u32, mem_total_mib: u32) {
    let flags = if disk_phys != 0 { 1 } else { 0 }; // bit0 = ramdisk present
    let info = BootInfo {
        magic: BOOTINFO_MAGIC,
        disk_phys,
        disk_sectors,
        flags,
        mem_bytes,
        mem_total_mib,
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
