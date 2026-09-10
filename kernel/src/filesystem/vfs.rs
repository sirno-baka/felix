// kernel/src/filesystem/vfs.rs
use crate::println;
use crate::sync::MutexLazy;
use crate::sync::mutex::Mutex;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub inode: u32,
    pub name: String,
    pub file_type: u8, // 1 = regular file, 2 = directory, etc.
    pub size: u32,     // размер файла (0 для директорий)
}

pub trait Filesystem: Send + Sync {
    fn read_file(&self, path: &str) -> Option<Vec<u8>>;
    fn write_file(&mut self, path: &str, data: &[u8]) -> bool;
    fn create_file(&mut self, path: &str, data: &[u8]) -> bool;
    fn remove_file(&mut self, path: &str) -> bool;
    fn mkdir(&mut self, path: &str) -> bool;
    fn rmdir(&mut self, path: &str) -> bool;
    fn list_directory_entries(&self, path: &str) -> Option<Vec<DirEntry>>;
    fn resolve_path(&self, path: &str) -> Option<u32>;
    fn read_at(&self, inode: u32, offset: u64, buf: &mut [u8]) -> usize;
    fn write_at(&mut self, inode: u32, offset: u64, buf: &[u8]) -> usize;
    fn is_mounted(&self) -> bool;
}

pub struct Vfs {
    inner: Mutex<VfsInner>,
}

fn new_mount_registry() -> Mutex<Vec<String>> {
    Mutex::new(Vec::new())
}

/// Kept separately from VfsInner so synthetic filesystems such as /proc can
/// inspect the mount table while VFS is already dispatching a read to them.
pub static MOUNT_REGISTRY: MutexLazy<Mutex<Vec<String>>> = MutexLazy::new(new_mount_registry);

struct Mount {
    point: String,
    fs_id: u8,
    fs: Box<dyn Filesystem>,
}

struct VfsInner {
    root_fs: Option<Box<dyn Filesystem>>,
    mounts: Vec<Mount>,
    next_fs_id: u8,
}

impl Vfs {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(VfsInner {
                root_fs: None,
                mounts: Vec::new(),
                next_fs_id: 1,
            }),
        }
    }

    pub fn set_root(&self, fs: Box<dyn Filesystem>) {
        let mut inner = self.inner.lock();
        inner.root_fs = Some(fs);
        drop(inner);
        let mut mounts = MOUNT_REGISTRY.get().lock();
        if !mounts.iter().any(|m| m == "/") {
            mounts.push("/".to_string());
        }
    }

    pub fn mount(&self, mount_point: &str, fs: Box<dyn Filesystem>) {
        println!("[VFS] mount request: {}", mount_point);
        if !mount_point.starts_with('/') {
            println!("[VFS] Mount point must start with /");
            return;
        }
        let mut inner = self.inner.lock();
        if inner.mounts.iter().any(|m| m.point == mount_point) {
            println!("[VFS] Already mounted at {}", mount_point);
            return;
        }
        let Some(fs_id) = alloc_mount_id(&mut inner) else {
            println!("[VFS] No free filesystem id for {}", mount_point);
            return;
        };
        inner.mounts.push(Mount { point: mount_point.to_string(), fs_id, fs });
        let count = inner.mounts.len();
        drop(inner);
        let mut mounts = MOUNT_REGISTRY.get().lock();
        if !mounts.iter().any(|m| m == mount_point) {
            mounts.push(mount_point.to_string());
        }
        println!("[VFS] Mounted at {} (mounts={})", mount_point, count);
    }

    pub fn unmount(&self, mount_point: &str) -> bool {
        let mut inner = self.inner.lock();
        let before = inner.mounts.len();
        inner.mounts.retain(|m| m.point != mount_point);
        let ok = inner.mounts.len() < before;
        if ok {
            drop(inner);
            MOUNT_REGISTRY.get().lock().retain(|mp| mp != mount_point);
            println!("[VFS] Unmounted {}", mount_point);
        }
        ok
    }

    pub fn is_mounted_at(&self, mount_point: &str) -> bool {
        let inner = self.inner.lock();
        inner.mounts.iter().any(|m| m.point == mount_point)
    }

    /// Snapshot mount points for /proc/mounts and userspace diagnostics.
    pub fn mount_points(&self) -> Vec<String> {
        MOUNT_REGISTRY.get().lock().clone()
    }

    // ====================== PUBLIC API ======================

    pub fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        let inner = self.inner.lock();
        let (fs, rel_path, _fs_id) = resolve(&inner, path);
        fs.read_file(&rel_path)
    }

    pub fn write_file(&self, path: &str, data: &[u8]) -> bool {
        let mut inner = self.inner.lock();
        let (fs, rel_path, _fs_id) = resolve_mut(&mut inner, path);
        fs.write_file(&rel_path, data)
    }

    pub fn create_file(&self, path: &str, data: &[u8]) -> bool {
        let mut inner = self.inner.lock();
        let (fs, rel_path, _fs_id) = resolve_mut(&mut inner, path);
        fs.create_file(&rel_path, data)
    }

    pub fn remove_file(&self, path: &str) -> bool {
        let mut inner = self.inner.lock();
        let (fs, rel_path, _fs_id) = resolve_mut(&mut inner, path);
        fs.remove_file(&rel_path)
    }

    pub fn mkdir(&self, path: &str) -> bool {
        let mut inner = self.inner.lock();
        let (fs, rel_path, _fs_id) = resolve_mut(&mut inner, path);
        fs.mkdir(&rel_path)
    }

    pub fn rmdir(&self, path: &str) -> bool {
        let mut inner = self.inner.lock();
        let (fs, rel_path, _fs_id) = resolve_mut(&mut inner, path);
        fs.rmdir(&rel_path)
    }

    pub fn list_directory_entries(&self, path: &str) -> Option<Vec<DirEntry>> {
        let inner = self.inner.lock();
        let path = normalize_dir_path(path);

        // Ask the backing filesystem first. A synthetic parent such as /mnt
        // may not physically exist on rootfs, so None is not final until mount
        // descendants have been considered.
        let (fs, rel_path, _fs_id) = resolve(&inner, &path);
        let backing = fs.list_directory_entries(&rel_path);
        let backing_exists = backing.is_some();
        let mut entries = backing.unwrap_or_default();
        let synthetic = add_mount_children(&inner, &path, &mut entries);

        if backing_exists || synthetic {
            Some(entries)
        } else {
            None
        }
    }

    pub fn resolve_path(&self, path: &str) -> Option<u32> {
        let inner = self.inner.lock();
        let (fs, rel_path, fs_id) = resolve(&inner, path);

        // Кодируем fs_id в старшие 8 битах, локальный inode в младшие 24 бита
        fs.resolve_path(&rel_path)
            .map(|local_inode| ((fs_id as u32) << 24) | (local_inode & 0x00FFFFFF))
    }

    pub fn read_at(&self, global_inode: u32, offset: u64, buf: &mut [u8]) -> usize {
        let inner = self.inner.lock();
        let fs_id = (global_inode >> 24) as usize;
        let local_inode = global_inode & 0x00FFFFFF;

        if fs_id == 0 {
            if let Some(fs) = &inner.root_fs {
                return fs.read_at(local_inode, offset, buf);
            }
        } else if let Some(mount) = inner.mounts.iter().find(|m| m.fs_id as usize == fs_id) {
            return mount.fs.read_at(local_inode, offset, buf);
        }
        0
    }

    pub fn write_at(&self, global_inode: u32, offset: u64, buf: &[u8]) -> usize {
        let mut inner = self.inner.lock();
        let fs_id = (global_inode >> 24) as usize;
        let local_inode = global_inode & 0x00FFFFFF;

        if fs_id == 0 {
            if let Some(fs) = &mut inner.root_fs {
                return fs.write_at(local_inode, offset, buf);
            }
        } else if let Some(mount) = inner.mounts.iter_mut().find(|m| m.fs_id as usize == fs_id) {
            return mount.fs.write_at(local_inode, offset, buf);
        }
        0
    }
}

// ====================== ВСПОМОГАТЕЛЬНЫЕ ФУНКЦИИ ======================

fn normalize_dir_path(path: &str) -> String {
    if path.is_empty() || path == "/" {
        return "/".to_string();
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() { "/".to_string() } else { trimmed.to_string() }
}

/// Add the immediate directory components implied by deeper mountpoints.
/// Example: /mnt/usb0 and /mnt/cf0 make `ls /` show `mnt/`, while `ls /mnt`
/// shows `usb0/` and `cf0/` even if the root filesystem has no /mnt inode.
fn add_mount_children(inner: &VfsInner, path: &str, entries: &mut Vec<DirEntry>) -> bool {
    let mut added = false;
    for mount in &inner.mounts {
        if mount.point == path {
            continue;
        }
        let rest = if path == "/" {
            mount.point.strip_prefix('/').unwrap_or(mount.point.as_str())
        } else {
            let Some(rest) = mount.point.strip_prefix(path) else { continue; };
            let Some(rest) = rest.strip_prefix('/') else { continue; };
            rest
        };
        if rest.is_empty() {
            continue;
        }
        let child = rest.split('/').next().unwrap_or(rest);
        if child.is_empty() || entries.iter().any(|e| e.name.trim_end_matches('/') == child) {
            continue;
        }
        entries.push(DirEntry {
            inode: 0,
            name: child.to_string(),
            file_type: 2,
            size: 0,
        });
        added = true;
    }
    added
}

fn alloc_mount_id(inner: &mut VfsInner) -> Option<u8> {
    for _ in 0..255 {
        let id = if inner.next_fs_id == 0 { 1 } else { inner.next_fs_id };
        inner.next_fs_id = id.wrapping_add(1);
        if !inner.mounts.iter().any(|m| m.fs_id == id) {
            return Some(id);
        }
    }
    None
}

/// `/dev` matches `/dev` and `/dev/sda`, but not `/devfoo`.
fn is_mount_prefix(path: &str, mp: &str) -> bool {
    if mp == "/" {
        return true;
    }
    path == mp || path.starts_with(mp) && path.as_bytes().get(mp.len()) == Some(&b'/')
}

fn resolve<'a>(inner: &'a VfsInner, path: &'a str) -> (&'a dyn Filesystem, String, u8) {
    let path = if path.is_empty() { "/" } else { path };
    // println!("[VFS] resolve start path={} mounts={}", path, inner.mounts.len());
    let mut best_fs: &dyn Filesystem = inner
        .root_fs
        .as_ref()
        .expect("[VFS] No root filesystem set!")
        .as_ref();
    let mut best_prefix = "/";
    let mut best_id: u8 = 0; // 0 = root_fs

    for mount in inner.mounts.iter() {
        if is_mount_prefix(path, &mount.point) && mount.point.len() > best_prefix.len() {
            best_fs = mount.fs.as_ref();
            best_prefix = &mount.point;
            best_id = mount.fs_id;
        }
    }

    let relative = if path == best_prefix {
        "/"
    } else if path.starts_with(best_prefix) && best_prefix != "/" {
        &path[best_prefix.len()..]
    } else {
        path
    };

    // println!("[VFS] resolve done mount={} rel={} fs_id={}", best_prefix, relative, best_id);
    (best_fs, relative.to_string(), best_id)
}

fn resolve_mut<'a>(inner: &'a mut VfsInner, path: &'a str) -> (&'a mut dyn Filesystem, String, u8) {
    let path = if path.is_empty() { "/" } else { path };
    let mut best_fs: &mut dyn Filesystem = inner
        .root_fs
        .as_mut()
        .expect("[VFS] No root filesystem set!")
        .as_mut();
    let mut best_prefix = "/";
    let mut best_id: u8 = 0;

    for mount in inner.mounts.iter_mut() {
        if is_mount_prefix(path, mount.point.as_str()) && mount.point.len() > best_prefix.len() {
            best_fs = mount.fs.as_mut();
            best_prefix = &mount.point;
            best_id = mount.fs_id;
        }
    }

    let relative = if path == best_prefix {
        "/"
    } else if path.starts_with(best_prefix) && best_prefix != "/" {
        &path[best_prefix.len()..]
    } else {
        path
    };

    (best_fs, relative.to_string(), best_id)
}

// ====================== ГЛОБАЛЬНЫЙ VFS ======================
pub static VFS: MutexLazy<Vfs> = MutexLazy::new(Vfs::new);
