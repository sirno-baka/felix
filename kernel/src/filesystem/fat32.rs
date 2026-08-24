//! FAT12/16/32 via `fatfs`, wired to Felix `BlockDevice` and VFS.
//!
//! Storage adapter implements fatfs `Read`/`Write`/`Seek` on top of
//! `read_sectors` / `write_sectors`. Synthetic inodes map paths for `read_at`.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cmp;

use fatfs::{FileSystem, FsOptions, Read, Seek, SeekFrom, Write};

use crate::disk::interface::BlockDevice;
use crate::disk::PartitionConfig;
use crate::filesystem::vfs::{DirEntry, Filesystem};
use crate::println;
use crate::spin::Mutex;

const SECTOR: u64 = 512;

/// MBR partition types that usually mean FAT.
const FAT_PART_TYPES: &[u8] = &[
    0x01, // FAT12
    0x04, // FAT16 <32M
    0x06, // FAT16
    0x0B, // FAT32 CHS
    0x0C, // FAT32 LBA
    0x0E, // FAT16 LBA
];

/// Find first FAT partition in MBR; fallback to whole disk.
pub fn find_fat_partition_config(device: &dyn BlockDevice) -> PartitionConfig {
    let mut mbr = [0u8; 512];
    if device
        .read_sectors(1, 0, mbr.as_mut_ptr() as u32)
        .is_err()
    {
        println!("[FAT] Failed to read MBR, using whole disk");
        return PartitionConfig::whole_disk();
    }
    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        println!("[FAT] No MBR signature, using whole disk");
        return PartitionConfig::whole_disk();
    }

    for i in 0..4 {
        let off = 0x1BE + i * 16;
        let ptype = mbr[off + 4];
        if FAT_PART_TYPES.contains(&ptype) {
            let lba = u32::from_le_bytes([
                mbr[off + 8],
                mbr[off + 9],
                mbr[off + 10],
                mbr[off + 11],
            ]) as u64;
            let sectors = u32::from_le_bytes([
                mbr[off + 12],
                mbr[off + 13],
                mbr[off + 14],
                mbr[off + 15],
            ]) as u64;
            println!(
                "[FAT] partition type={:#x} LBA={} sectors={}",
                ptype, lba, sectors
            );
            return PartitionConfig::new(lba);
        }
    }

    println!("[FAT] no FAT partition in MBR, using whole disk");
    PartitionConfig::whole_disk()
}

// ---------------------------------------------------------------------------
// Block device → byte stream for fatfs
// ---------------------------------------------------------------------------

pub struct FatDisk {
    disk: Arc<Mutex<dyn BlockDevice>>,
    start_lba: u64,
    pos: u64,
    size: u64,
}

impl FatDisk {
    pub fn new(disk: Arc<Mutex<dyn BlockDevice>>, start_lba: u64, size_bytes: u64) -> Self {
        Self {
            disk,
            start_lba,
            pos: 0,
            size: size_bytes,
        }
    }

    fn abs_lba(&self, byte_off: u64) -> (u32, usize) {
        let abs = self.start_lba * SECTOR + byte_off;
        ((abs / SECTOR) as u32, (abs % SECTOR) as usize)
    }
}

impl fatfs::IoBase for FatDisk {
    type Error = ();
}

impl Read for FatDisk {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, ()> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.size != 0 && self.pos >= self.size {
            return Ok(0);
        }

        let mut done = 0usize;
        let mut sector = [0u8; 512];

        while done < buf.len() {
            if self.size != 0 && self.pos + done as u64 >= self.size {
                break;
            }
            let (lba, off) = self.abs_lba(self.pos + done as u64);
            {
                let disk = self.disk.lock();
                disk.read_sectors(1, lba, sector.as_mut_ptr() as u32)
                    .map_err(|_| ())?;
            }
            let room = 512 - off;
            let mut n = cmp::min(room, buf.len() - done);
            if self.size != 0 {
                let left = (self.size - self.pos - done as u64) as usize;
                n = cmp::min(n, left);
            }
            buf[done..done + n].copy_from_slice(&sector[off..off + n]);
            done += n;
        }

        self.pos += done as u64;
        Ok(done)
    }
}

impl Write for FatDisk {
    fn write(&mut self, buf: &[u8]) -> Result<usize, ()> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut done = 0usize;
        let mut sector = [0u8; 512];

        while done < buf.len() {
            let (lba, off) = self.abs_lba(self.pos + done as u64);
            let room = 512 - off;
            let n = cmp::min(room, buf.len() - done);

            {
                let mut disk = self.disk.lock();
                if off != 0 || n != 512 {
                    disk.read_sectors(1, lba, sector.as_mut_ptr() as u32)
                        .map_err(|_| ())?;
                }
                sector[off..off + n].copy_from_slice(&buf[done..done + n]);
                disk.write_sectors(1, lba, sector.as_ptr() as u32)
                    .map_err(|_| ())?;
            }

            done += n;
        }

        self.pos += done as u64;
        if self.size != 0 && self.pos > self.size {
            self.size = self.pos;
        }
        Ok(done)
    }

    fn flush(&mut self) -> Result<(), ()> {
        Ok(())
    }
}

impl Seek for FatDisk {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, ()> {
        let new = match pos {
            SeekFrom::Start(o) => o as i64,
            SeekFrom::Current(o) => self.pos as i64 + o,
            SeekFrom::End(o) => {
                if self.size == 0 {
                    return Err(());
                }
                self.size as i64 + o
            }
        };
        if new < 0 {
            return Err(());
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

fn normalize_rel(path: &str) -> String {
    let p = path.trim_start_matches('/');
    if p.is_empty() {
        String::from("/")
    } else {
        p.to_string()
    }
}

fn split_path(path: &str) -> Vec<&str> {
    path.trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect()
}

fn parent_and_name(path: &str) -> Option<(Vec<&str>, &str)> {
    let comps = split_path(path);
    if comps.is_empty() {
        return None;
    }
    let (last, rest) = comps.split_last()?;
    Some((rest.to_vec(), *last))
}

// ---------------------------------------------------------------------------
// FatFs — VFS backend
// ---------------------------------------------------------------------------

pub struct FatFs {
    fs: Mutex<Option<FileSystem<FatDisk>>>,
    path_to_ino: Mutex<BTreeMap<String, u32>>,
    ino_to_path: Mutex<BTreeMap<u32, String>>,
    next_ino: Mutex<u32>,
}

impl FatFs {
    pub fn mount(
        disk: Arc<Mutex<dyn BlockDevice>>,
        config: Option<PartitionConfig>,
    ) -> Result<Self, ()> {
        let start = config.map(|c| c.start_lba).unwrap_or(0);
        let storage = FatDisk::new(disk, start, 0);
        let fs = FileSystem::new(storage, FsOptions::new()).map_err(|e| {
            println!("[FAT] mount failed: {:?}", e);
        })?;
        println!("[FAT] mounted at LBA {}", start);
        Ok(Self {
            fs: Mutex::new(Some(fs)),
            path_to_ino: Mutex::new(BTreeMap::new()),
            ino_to_path: Mutex::new(BTreeMap::new()),
            next_ino: Mutex::new(1),
        })
    }

    pub fn mount_auto(disk: Arc<Mutex<dyn BlockDevice>>) -> Result<Self, ()> {
        let cfg = {
            let d = disk.lock();
            find_fat_partition_config(&*d)
        };
        Self::mount(disk, Some(cfg))
    }

    fn alloc_inode(&self, path: &str) -> u32 {
        let key = normalize_rel(path);
        {
            let map = self.path_to_ino.lock();
            if let Some(&ino) = map.get(&key) {
                return ino;
            }
        }
        let mut next = self.next_ino.lock();
        let ino = *next;
        *next = next.wrapping_add(1).max(1);
        self.path_to_ino.lock().insert(key.clone(), ino);
        self.ino_to_path.lock().insert(ino, key);
        ino
    }

    fn path_for_inode(&self, ino: u32) -> Option<String> {
        self.ino_to_path.lock().get(&ino).cloned()
    }

    /// Run `f` while holding the FS lock. All `Dir`/`File` must die inside `f`.
    fn with_fs<R>(&self, f: impl FnOnce(&FileSystem<FatDisk>) -> R) -> Option<R> {
        let guard = self.fs.lock();
        let fs = guard.as_ref()?;
        Some(f(fs))
    }
}

impl Filesystem for FatFs {
    fn is_mounted(&self) -> bool {
        self.fs.lock().is_some()
    }

    fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        let (parent, name) = parent_and_name(path)?;
        self.with_fs(|fs| {
            let mut dir = fs.root_dir();
            for c in &parent {
                dir = dir.open_dir(*c).ok()?;
            }
            let mut file = dir.open_file(name).ok()?;
            let mut out = Vec::new();
            let mut chunk = [0u8; 512];
            loop {
                match file.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => out.extend_from_slice(&chunk[..n]),
                    Err(_) => return None,
                }
            }
            Some(out)
        })?
    }

    fn write_file(&mut self, path: &str, data: &[u8]) -> bool {
        let Some((parent, name)) = parent_and_name(path) else {
            return false;
        };
        self.with_fs(|fs| {
            let mut dir = fs.root_dir();
            for c in &parent {
                dir = match dir.open_dir(*c) {
                    Ok(d) => d,
                    Err(_) => return false,
                };
            }
            let mut file = match dir.open_file(name) {
                Ok(f) => f,
                Err(_) => return false,
            };
            if file.seek(SeekFrom::Start(0)).is_err() {
                return false;
            }
            if file.truncate().is_err() {
                return false;
            }
            if file.write_all(data).is_err() {
                return false;
            }
            file.flush().is_ok()
        })
        .unwrap_or(false)
    }

    fn create_file(&mut self, path: &str, data: &[u8]) -> bool {
        let Some((parent, name)) = parent_and_name(path) else {
            return false;
        };
        let ok = self
            .with_fs(|fs| {
                let mut dir = fs.root_dir();
                for c in &parent {
                    dir = match dir.open_dir(*c) {
                        Ok(d) => d,
                        Err(_) => return false,
                    };
                }
                let mut file = match dir.create_file(name) {
                    Ok(f) => f,
                    Err(_) => return false,
                };
                let _ = file.seek(SeekFrom::Start(0));
                let _ = file.truncate();
                if !data.is_empty() && file.write_all(data).is_err() {
                    return false;
                }
                let _ = file.flush();
                true
            })
            .unwrap_or(false);
        if ok {
            let _ = self.alloc_inode(path);
        }
        ok
    }

    fn remove_file(&mut self, path: &str) -> bool {
        let Some((parent, name)) = parent_and_name(path) else {
            return false;
        };
        let ok = self
            .with_fs(|fs| {
                let mut dir = fs.root_dir();
                for c in &parent {
                    dir = match dir.open_dir(*c) {
                        Ok(d) => d,
                        Err(_) => return false,
                    };
                }
                dir.remove(name).is_ok()
            })
            .unwrap_or(false);
        if ok {
            let key = normalize_rel(path);
            if let Some(ino) = self.path_to_ino.lock().remove(&key) {
                self.ino_to_path.lock().remove(&ino);
            }
        }
        ok
    }

    fn mkdir(&mut self, path: &str) -> bool {
        let Some((parent, name)) = parent_and_name(path) else {
            return false;
        };
        let ok = self
            .with_fs(|fs| {
                let mut dir = fs.root_dir();
                for c in &parent {
                    dir = match dir.open_dir(*c) {
                        Ok(d) => d,
                        Err(_) => return false,
                    };
                }
                // Force Result to drop before leaving the closure.
                let created = dir.create_dir(name);
                created.is_ok()
            })
            .unwrap_or(false);
        if ok {
            let _ = self.alloc_inode(path);
        }
        ok
    }

    fn rmdir(&mut self, path: &str) -> bool {
        self.remove_file(path)
    }

    fn list_directory_entries(&self, path: &str) -> Option<Vec<DirEntry>> {
        let comps = split_path(path);

        // Collect plain data under the FS lock, then build DirEntry outside.
        let entries: Vec<(String, bool, u32)> = self.with_fs(|fs| {
            let mut dir = fs.root_dir();
            for c in &comps {
                dir = dir.open_dir(*c).ok()?;
            }
            let mut list = Vec::new();
            for e in dir.iter() {
                let e = e.ok()?;
                let name = e.file_name();
                if name == "." || name == ".." {
                    continue;
                }
                let is_dir = e.is_dir();
                let size = if is_dir { 0 } else { e.len() as u32 };
                list.push((name, is_dir, size));
            }
            Some(list)
        })??;

        let mut out = Vec::with_capacity(entries.len());
        for (name, is_dir, size) in entries {
            let child_path = if comps.is_empty() {
                name.clone()
            } else {
                let mut p = comps.join("/");
                p.push('/');
                p.push_str(&name);
                p
            };
            let ino = self.alloc_inode(&child_path);
            out.push(DirEntry {
                inode: ino,
                name,
                file_type: if is_dir { 2 } else { 1 },
                size,
            });
        }
        Some(out)
    }

    fn resolve_path(&self, path: &str) -> Option<u32> {
        let comps = split_path(path);
        if comps.is_empty() {
            return Some(self.alloc_inode("/"));
        }
        let (name, parent) = comps.split_last()?;

        let exists = self.with_fs(|fs| {
            let mut dir = fs.root_dir();
            for c in parent {
                dir = dir.open_dir(*c).ok()?;
            }
            let mut found = false;
            for e in dir.iter() {
                let e = e.ok()?;
                if e.file_name() == *name {
                    found = true;
                    break;
                }
            }
            Some(found)
        })??;

        if !exists {
            return None;
        }
        Some(self.alloc_inode(path))
    }

    fn read_at(&self, inode: u32, offset: u64, buf: &mut [u8]) -> usize {
        let Some(path) = self.path_for_inode(inode) else {
            return 0;
        };
        if path == "/" {
            return 0;
        }
        let Some((parent, name)) = parent_and_name(&path) else {
            return 0;
        };

        self.with_fs(|fs| {
            let mut dir = fs.root_dir();
            for c in &parent {
                dir = match dir.open_dir(*c) {
                    Ok(d) => d,
                    Err(_) => return 0,
                };
            }
            let mut file = match dir.open_file(name) {
                Ok(f) => f,
                Err(_) => return 0,
            };
            if file.seek(SeekFrom::Start(offset)).is_err() {
                return 0;
            }
            file.read(buf).unwrap_or(0)
        })
        .unwrap_or(0)
    }

    fn write_at(&mut self, inode: u32, offset: u64, buf: &[u8]) -> usize {
        let Some(path) = self.path_for_inode(inode) else {
            return 0;
        };
        if path == "/" {
            return 0;
        }
        let Some((parent, name)) = parent_and_name(&path) else {
            return 0;
        };

        self.with_fs(|fs| {
            let mut dir = fs.root_dir();
            for c in &parent {
                dir = match dir.open_dir(*c) {
                    Ok(d) => d,
                    Err(_) => return 0,
                };
            }
            let mut file = match dir.open_file(name) {
                Ok(f) => f,
                Err(_) => return 0,
            };
            if file.seek(SeekFrom::Start(offset)).is_err() {
                return 0;
            }
            match file.write(buf) {
                Ok(n) => {
                    let _ = file.flush();
                    n
                }
                Err(_) => 0,
            }
        })
        .unwrap_or(0)
    }
}

pub fn boxed_fat(disk: Arc<Mutex<dyn BlockDevice>>) -> Option<Box<dyn Filesystem>> {
    FatFs::mount_auto(disk)
        .ok()
        .map(|fs| Box::new(fs) as Box<dyn Filesystem>)
}
