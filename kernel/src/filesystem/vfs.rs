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

#[derive(Debug, Clone, Copy, Default)]
pub struct Metadata {
    pub inode: u32,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u64,
    pub size: u64,
    pub blksize: u32,
    pub blocks: u64,
    pub atime: u32,
    pub mtime: u32,
    pub ctime: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameError {
    NotFound,
    Exists,
    CrossDevice,
    Unsupported,
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

    /// Backend metadata. Mode includes both file type and permission bits.
    fn metadata(&self, _path: &str) -> Option<Metadata> { None }
    fn metadata_inode(&self, _inode: u32) -> Option<Metadata> { None }

    /// Rename within one filesystem. VFS rejects cross-mount renames before
    /// reaching the backend.
    fn rename(&mut self, _old: &str, _new: &str) -> bool { false }
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

    pub fn metadata(&self, path: &str) -> Option<Metadata> {
        let inner = self.inner.lock();
        let normalized = normalize_dir_path(path);
        let (fs, rel_path, fs_id) = resolve(&inner, &normalized);
        if let Some(mut meta) = fs.metadata(&rel_path) {
            meta.inode = ((fs_id as u32) << 24) | (meta.inode & 0x00ff_ffff);
            return Some(meta);
        }

        // A parent implied only by mount points (e.g. /mnt for /mnt/usb0)
        // is still a real directory in the VFS namespace.
        if has_mount_descendant(&inner, &normalized) {
            return Some(Metadata {
                inode: 0,
                mode: 0o040755,
                nlink: 2,
                blksize: 4096,
                ..Metadata::default()
            });
        }
        None
    }

    pub fn metadata_inode(&self, global_inode: u32) -> Option<Metadata> {
        let inner = self.inner.lock();
        let fs_id = (global_inode >> 24) as u8;
        let local_inode = global_inode & 0x00ff_ffff;
        let mut meta = if fs_id == 0 {
            inner.root_fs.as_ref()?.metadata_inode(local_inode)?
        } else {
            inner.mounts.iter().find(|m| m.fs_id == fs_id)?.fs.metadata_inode(local_inode)?
        };
        meta.inode = global_inode;
        Some(meta)
    }

    pub fn rename(&self, old_path: &str, new_path: &str) -> Result<(), RenameError> {
        let mut inner = self.inner.lock();
        let old_path = normalize_dir_path(old_path);
        let new_path = normalize_dir_path(new_path);
        if old_path == "/" || new_path == "/" {
            return Err(RenameError::Unsupported);
        }

        let (old_rel, old_id) = {
            let (_, rel, id) = resolve(&inner, &old_path);
            (rel, id)
        };
        let (new_rel, new_id) = {
            let (_, rel, id) = resolve(&inner, &new_path);
            (rel, id)
        };
        if old_id != new_id {
            return Err(RenameError::CrossDevice);
        }

        let fs: &mut dyn Filesystem = if old_id == 0 {
            inner.root_fs.as_mut().ok_or(RenameError::NotFound)?.as_mut()
        } else {
            inner.mounts.iter_mut().find(|m| m.fs_id == old_id)
                .ok_or(RenameError::NotFound)?.fs.as_mut()
        };
        let old_exists = fs.resolve_path(&old_rel).is_some() || fs.list_directory_entries(&old_rel).is_some();
        if !old_exists {
            return Err(RenameError::NotFound);
        }
        let new_exists = fs.resolve_path(&new_rel).is_some() || fs.list_directory_entries(&new_rel).is_some();
        if new_exists {
            return Err(RenameError::Exists);
        }
        if fs.rename(&old_rel, &new_rel) {
            Ok(())
        } else {
            Err(RenameError::Unsupported)
        }
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

    /// Atomic O_APPEND primitive. EOF lookup and write happen while holding the
    /// same VFS lock, so independently opened append descriptors cannot race
    /// each other between stat() and write(). Returns (bytes_written, new_eof).
    pub fn append_write(&self, global_inode: u32, buf: &[u8]) -> (usize, u64) {
        let mut inner = self.inner.lock();
        let fs_id = (global_inode >> 24) as u8;
        let local_inode = global_inode & 0x00ff_ffff;

        let fs: &mut dyn Filesystem = if fs_id == 0 {
            match inner.root_fs.as_mut() {
                Some(fs) => fs.as_mut(),
                None => return (0, 0),
            }
        } else {
            match inner.mounts.iter_mut().find(|m| m.fs_id == fs_id) {
                Some(mount) => mount.fs.as_mut(),
                None => return (0, 0),
            }
        };

        let eof = match fs.metadata_inode(local_inode) {
            Some(meta) => meta.size,
            None => return (0, 0),
        };
        let written = fs.write_at(local_inode, eof, buf);
        (written, eof.saturating_add(written as u64))
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
fn has_mount_descendant(inner: &VfsInner, path: &str) -> bool {
    inner.mounts.iter().any(|mount| {
        if path == "/" {
            mount.point != "/"
        } else {
            mount.point.starts_with(path)
                && mount.point.as_bytes().get(path.len()) == Some(&b'/')
        }
    })
}

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
