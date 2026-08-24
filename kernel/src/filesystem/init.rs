//! Early userspace-ish bring-up: DevFS, auto-detect filesystems on IDE,
//! pick a root (prefer one that has `/shell`), mount the rest under `/mnt/*`.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::device::char::{NullDevice, ZeroDevice};
use crate::disk::interface::BlockDevice;
use crate::filesystem::devfs::DevFS;
use crate::filesystem::ext2::Ext2;
use crate::filesystem::fat32::FatFs;
use crate::filesystem::vfs::{Filesystem, VFS};
use crate::pci::ide::{IDE, IDEDevice};
use crate::println;
use crate::spin;
use crate::sync::mutex::Mutex;

/// Result of probing one block device.
struct ProbedFs {
    name: String, // sda, sdb, …
    kind: &'static str,
    fs: Box<dyn Filesystem>,
    has_shell: bool,
}

/// Register `/dev`, scan ATA disks, auto-mount root + secondary volumes.
///
/// Call after `IDE.initialize()`. Returns `false` if no filesystem could be
/// mounted as root.
pub fn init_rootfs() -> bool {
    println!("[init] probing IDE disks…");

    let disks = collect_ata_disks();
    if disks.is_empty() {
        println!("[init] no ATA disks found");
        return false;
    }

    // ---- /dev ----
    let devfs = Box::new(DevFS::new());
    for (i, dev) in disks.iter().enumerate() {
        let name = disk_name(i);
        devfs.register_block(
            &name,
            Mutex::new(Box::new(dev.clone()) as Box<dyn BlockDevice>),
        );
        println!("[init] /dev/{}  size={} sectors type={}", name, dev.size, dev.r#type);
    }
    devfs.register_char("null", Box::new(NullDevice));
    devfs.register_char("zero", Box::new(ZeroDevice));

    // ---- probe FS on each disk ----
    let mut probed: Vec<ProbedFs> = Vec::new();
    for (i, dev) in disks.into_iter().enumerate() {
        let name = disk_name(i);
        let arc: Arc<spin::Mutex<dyn BlockDevice>> =
            Arc::new(spin::Mutex::new(dev));

        match try_mount(arc, &name) {
            Some(p) => {
                println!(
                    "[init] {} → {} (shell={})",
                    name,
                    p.kind,
                    p.has_shell
                );
                probed.push(p);
            }
            None => println!("[init] {} → no supported filesystem", name),
        }
    }

    if probed.is_empty() {
        println!("[init] nothing mountable");
        return false;
    }

    // Prefer: has /shell, then ext2, then first available.
    let root_idx = pick_root(&probed);
    let root = probed.swap_remove(root_idx);
    println!("[VFS] root = {} on /dev/{} ({})", root.kind, root.name, if root.has_shell { "has /shell" } else { "no /shell" });
    VFS.get().set_root(root.fs);

    // Remaining volumes under /mnt/<name>
    for p in probed {
        let mp = format!("/mnt/{}", p.name);
        println!("[VFS] mount {} ({}) at {}", p.name, p.kind, mp);
        VFS.get().mount(&mp, p.fs);
    }

    VFS.get().mount("/dev", devfs);
    true
}

fn disk_name(index: usize) -> String {
    // sda, sdb, sdc, …
    let letter = (b'a' + (index as u8)).min(b'z') as char;
    format!("sd{}", letter)
}

fn collect_ata_disks() -> Vec<IDEDevice> {
    let ide = IDE.lock();
    let mut out = Vec::new();
    for i in 0..4u8 {
        if let Some(dev) = ide.get_device(i) {
            // type 0 = ATA (HDD), skip ATAPI
            if dev.r#type == 0 && dev.reserved != 0 {
                out.push(dev);
            }
        }
    }
    out
}

fn try_mount(disk: Arc<spin::Mutex<dyn BlockDevice>>, name: &str) -> Option<ProbedFs> {
    // 1) EXT2
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

    // 2) FAT12/16/32
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
    // 1. anything with /shell
    if let Some(i) = probed.iter().position(|p| p.has_shell) {
        return i;
    }
    // 2. prefer ext2
    if let Some(i) = probed.iter().position(|p| p.kind == "ext2") {
        return i;
    }
    0
}
