//! Early bring-up: prefer bootloader ramdisk (PXE), else IDE disks.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::device::char::{NullDevice, ZeroDevice};
use crate::disk::interface::BlockDevice;
use crate::disk::ramdisk::RamDisk;
use crate::filesystem::devfs::DevFS;
use crate::filesystem::ext2::Ext2;
use crate::filesystem::fat32::FatFs;
use crate::filesystem::vfs::{Filesystem, VFS};
use crate::memory::paging::{phys_to_virt, PAGING};
use crate::pci::ide::{IDE, IDEDevice};
use crate::println;
use crate::spin;
use crate::sync::mutex::Mutex;

/// Must match bootloader `BootInfo` at phys 0x6000.
const BOOTINFO_PHYS: u32 = 0x0000_7000; // must match bootloader (VESA uses 0x6000)
const BOOTINFO_MAGIC: u32 = 0xFE11_B007;

#[repr(C)]
struct BootInfo {
    magic: u32,
    disk_phys: u32,
    disk_sectors: u32,
    flags: u32,
    mem_bytes: u32,
}

struct ProbedFs {
    name: String,
    kind: &'static str,
    fs: Box<dyn Filesystem>,
    has_shell: bool,
}

/// Prefer bootloader-provided ramdisk (PXE / INT 13h path).
/// Fall back to probing real IDE disks (QEMU / local HDD) — no RAM copy.
pub fn init_rootfs() -> bool {
    let devfs = Box::new(DevFS::new());
    devfs.register_char("null", Box::new(NullDevice));
    devfs.register_char("zero", Box::new(ZeroDevice));

    // ---- Path A: bootloader already hydrated the disk into RAM (PXE) ----
    if let Some(info) = read_bootinfo() {
        println!(
            "[init] BootInfo: disk phys=0x{:08x} sectors={} flags={:#x} mem={}MiB",
            info.disk_phys,
            info.disk_sectors,
            info.flags,
            info.mem_bytes / (1024 * 1024)
        );

        // Local IDE boot still publishes BootInfo for mem_bytes, but disk_phys=0.
        if info.disk_phys == 0 || info.disk_sectors == 0 {
            println!("[init] BootInfo has no ramdisk — IDE path");
        } else {
        // Do not let the frame allocator hand out pages that overlap the image.
        reserve_frames_past(info.disk_phys, info.disk_sectors);

        let ram = RamDisk::from_phys(info.disk_phys, info.disk_sectors);
        let ram_fs = ram;
        let ram_dev = ram;

        devfs.register_block(
            "ram0",
            Mutex::new(Box::new(ram_dev) as Box<dyn BlockDevice>),
        );

        let arc: Arc<spin::Mutex<dyn BlockDevice>> =
            Arc::new(spin::Mutex::new(ram_fs));

        match try_mount(arc, "ram0") {
            Some(root) => {
                println!(
                    "[VFS] root = {} on /dev/ram0 (shell={})",
                    root.kind, root.has_shell
                );
                VFS.get().set_root(root.fs);

                // Optional: still register any real IDE disks under /dev + /mnt
                register_ide_disks(&devfs, true);

                VFS.get().mount("/dev", devfs);
                return true;
            }
            None => {
                println!("[init] BootInfo present but FS mount failed, trying IDE…");
            }
        }
        } // end disk_phys != 0
    } else {
        println!("[init] no BootInfo (magic mismatch) — IDE path");
    }

    // ---- Path B: real ATA disks (QEMU / bare metal HDD) ----
    let disks = collect_ata_disks();
    if disks.is_empty() {
        println!("[init] no ATA disks and no BootInfo ramdisk");
        return false;
    }

    for (i, dev) in disks.iter().enumerate() {
        let name = disk_name(i);
        devfs.register_block(
            &name,
            Mutex::new(Box::new(dev.clone()) as Box<dyn BlockDevice>),
        );
        println!(
            "[init] /dev/{}  size={} sectors (~{} MiB)",
            name,
            dev.size,
            (dev.size as u64 * 512) / (1024 * 1024)
        );
    }

    let mut probed: Vec<ProbedFs> = Vec::new();
    for (i, dev) in disks.into_iter().enumerate() {
        let name = disk_name(i);
        let arc: Arc<spin::Mutex<dyn BlockDevice>> =
            Arc::new(spin::Mutex::new(dev));
        match try_mount(arc, &name) {
            Some(p) => {
                println!("[init] {} → {} (shell={})", name, p.kind, p.has_shell);
                probed.push(p);
            }
            None => println!("[init] {} → no supported filesystem", name),
        }
    }

    if probed.is_empty() {
        println!("[init] nothing mountable");
        return false;
    }

    let root_idx = pick_root(&probed);
    let root = probed.swap_remove(root_idx);
    println!(
        "[VFS] root = {} on /dev/{} (shell={})",
        root.kind, root.name, root.has_shell
    );
    VFS.get().set_root(root.fs);

    for p in probed {
        let mp = format!("/mnt/{}", p.name);
        println!("[VFS] mount {} ({}) at {}", p.name, p.kind, mp);
        VFS.get().mount(&mp, p.fs);
    }

    VFS.get().mount("/dev", devfs);
    true
}

fn read_bootinfo() -> Option<BootInfo> {
    // BootInfo lives in low memory; identity/higher-half large pages cover it.
    let ptr = phys_to_virt(BOOTINFO_PHYS) as *const BootInfo;
    let info = unsafe { core::ptr::read_volatile(ptr) };
    if info.magic == BOOTINFO_MAGIC && (info.flags & 1) != 0 && info.disk_sectors > 0 {
        Some(info)
    } else {
        None
    }
}

/// Bump the frame allocator past the bootloader ramdisk so we never reuse it.
fn reserve_frames_past(disk_phys: u32, sectors: u32) {
    let end = disk_phys as u64 + sectors as u64 * 512;
    let end_page = ((end + 4095) / 4096) as u32;
    interrupt_sync::without_interrupts(|| unsafe {
        let mut pm = PAGING.lock();
        if pm.next_free_page < end_page {
            println!(
                "[init] reserve frames: next {} → {}",
                pm.next_free_page, end_page
            );
            pm.next_free_page = end_page;
        }
    });
}

fn register_ide_disks(devfs: &DevFS, mount_extra: bool) {
    let disks = collect_ata_disks();
    for (i, dev) in disks.iter().enumerate() {
        let name = disk_name(i);
        devfs.register_block(
            &name,
            Mutex::new(Box::new(dev.clone()) as Box<dyn BlockDevice>),
        );
        println!("[init] /dev/{} (physical)", name);
    }
    if !mount_extra {
        return;
    }
    for (i, dev) in disks.into_iter().enumerate() {
        let name = disk_name(i);
        let arc: Arc<spin::Mutex<dyn BlockDevice>> =
            Arc::new(spin::Mutex::new(dev));
        if let Some(p) = try_mount(arc, &name) {
            let mp = format!("/mnt/{}", name);
            println!("[VFS] mount {} ({}) at {}", name, p.kind, mp);
            VFS.get().mount(&mp, p.fs);
        }
    }
}

fn disk_name(index: usize) -> String {
    let letter = (b'a' + (index as u8)).min(b'z') as char;
    format!("sd{}", letter)
}

fn collect_ata_disks() -> Vec<IDEDevice> {
    let ide = IDE.lock();
    let mut out = Vec::new();
    for i in 0..4u8 {
        if let Some(dev) = ide.get_device(i) {
            if dev.r#type == 0 && dev.reserved != 0 {
                out.push(dev);
            }
        }
    }
    out
}

fn try_mount(disk: Arc<spin::Mutex<dyn BlockDevice>>, name: &str) -> Option<ProbedFs> {
    {
        let mut ext2 = Ext2::new_with_auto_partition(disk.clone());
        ext2.mount(None);
        if ext2.mounted {
            let has_shell = fs_has_shell(&ext2);
            return Some(ProbedFs {
                name: name.into(),
                kind: "ext2",
                fs: Box::new(ext2),
                has_shell,
            });
        }
    }

    match FatFs::mount_auto(disk) {
        Ok(fat) => {
            let has_shell = fs_has_shell(&fat);
            Some(ProbedFs {
                name: name.into(),
                kind: "fat",
                fs: Box::new(fat),
                has_shell,
            })
        }
        Err(()) => None,
    }
}

fn fs_has_shell(fs: &dyn Filesystem) -> bool {
    fs.read_file("/shell").is_some() || fs.read_file("shell").is_some()
}

fn pick_root(probed: &[ProbedFs]) -> usize {
    if let Some(i) = probed.iter().position(|p| p.has_shell) {
        return i;
    }
    if let Some(i) = probed.iter().position(|p| p.kind == "ext2") {
        return i;
    }
    0
}

pub fn init_net() {
    match crate::drivers::net::i8255x::I8255x::init() {
        Ok(_) => {
            crate::net::stack::init();
            println!("[init] network ready");
        }
        Err(_) => println!("[init] I8255x init failed"),
    }
}
