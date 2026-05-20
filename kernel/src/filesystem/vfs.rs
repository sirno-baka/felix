// kernel/src/filesystem/vfs.rs

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::arch::asm;
use interrupt_sync::{InterruptLazy};
use crate::println;
use crate::filesystem::ext2::DirEntry;
use crate::sync::mutex::Mutex;
use crate::sync::MutexLazy;

pub trait Filesystem: Send + Sync {
    fn read_file(&self, path: &str) -> Option<Vec<u8>>;
    fn write_file(&mut self, path: &str, data: &[u8]) -> bool;
    fn create_file(&mut self, path: &str, data: &[u8]) -> bool;
    fn remove_file(&mut self, path: &str) -> bool;
    fn mkdir(&mut self, path: &str) -> bool;
    fn rmdir(&mut self, path: &str) -> bool;
    fn list_directory_entries(&self, path: &str) -> Option<Vec<DirEntry>>;
    fn is_mounted(&self) -> bool;
}

pub struct Vfs {
    inner: Mutex<VfsInner>,
}

struct VfsInner {
    root_fs: Option<Box<dyn Filesystem>>,
    mounts: Vec<(String, Box<dyn Filesystem>)>,
}

impl Vfs {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(VfsInner {
                root_fs: None,
                mounts: Vec::new(),
            }),
        }
    }

    pub fn set_root(&self, fs: Box<dyn Filesystem>) {
        let mut inner = self.inner.lock();
        inner.root_fs = Some(fs);
    }

    pub fn mount(&self, mount_point: &str, fs: Box<dyn Filesystem>) {
        if !mount_point.starts_with('/') {
            println!("[VFS] Mount point must start with /");
            return;
        }
        let mut inner = self.inner.lock();
        inner.mounts.push((mount_point.to_string(), fs));
        println!("[VFS] Mounted at {}", mount_point);
    }

    // ====================== PUBLIC API ======================

    pub fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        let inner = self.inner.lock();
        let (fs, rel_path) = resolve(&inner, path);
        let res = fs.read_file(&rel_path);
        res
    }

    pub fn write_file(&self, path: &str, data: &[u8]) -> bool {
        let mut inner = self.inner.lock();
        let (fs, rel_path) = resolve_mut(&mut inner, path);
        let res = fs.write_file(&rel_path, data);
        res
    }

    pub fn create_file(&self, path: &str, data: &[u8]) -> bool {
        let mut inner = self.inner.lock();
        let (fs, rel_path) = resolve_mut(&mut inner, path);
        let res = fs.create_file(&rel_path, data);
        res
    }

    pub fn remove_file(&self, path: &str) -> bool {
        let mut inner = self.inner.lock();
        let (fs, rel_path) = resolve_mut(&mut inner, path);
        let res = fs.remove_file(&rel_path);
        res
    }

    pub fn mkdir(&self, path: &str) -> bool {
        let mut inner = self.inner.lock();
        let (fs, rel_path) = resolve_mut(&mut inner, path);
        let res = fs.mkdir(&rel_path);
        res
    }

    pub fn rmdir(&self, path: &str) -> bool {
        let mut inner = self.inner.lock();
        let (fs, rel_path) = resolve_mut(&mut inner, path);
        let res = fs.rmdir(&rel_path);
        res
    }

    pub fn list_directory_entries(&self, path: &str) -> Option<Vec<DirEntry>> {
        let inner = self.inner.lock();
        let (fs, rel_path) = resolve(&inner, path);
        let res = fs.list_directory_entries(&rel_path);
        res
    }
}

// ====================== ВСПОМОГАТЕЛЬНЫЕ ФУНКЦИИ ======================

fn resolve<'a>(inner: &'a VfsInner, path: &'a str) -> (&'a dyn Filesystem, String) {
    let path = if path.is_empty() { "/" } else { path };

    let mut best_fs: &dyn Filesystem = inner
        .root_fs
        .as_ref()
        .expect("[VFS] No root filesystem set!")
        .as_ref();

    let mut best_prefix = "/";

    for (mp, fs_box) in &inner.mounts {
        if path.starts_with(mp) && mp.len() > best_prefix.len() {
            best_fs = fs_box.as_ref();
            best_prefix = mp;
        }
    }

    let relative = if path == best_prefix {
        "/"
    } else if path.starts_with(best_prefix) && best_prefix != "/" {
        &path[best_prefix.len()..]
    } else {
        path
    };

    (best_fs, relative.to_string())
}

fn resolve_mut<'a>(inner: &'a mut VfsInner, path: &'a str) -> (&'a mut dyn Filesystem, String) {
    let path = if path.is_empty() { "/" } else { path };

    let mut best_fs: &mut dyn Filesystem = inner
        .root_fs
        .as_mut()
        .expect("[VFS] No root filesystem set!")
        .as_mut();

    let mut best_prefix = "/";

    for (mp, fs_box) in &mut inner.mounts {
        if path.starts_with(mp.as_str()) && mp.len() > best_prefix.len() {
            best_fs = fs_box.as_mut();
            best_prefix = mp;
        }
    }

    let relative = if path == best_prefix {
        "/"
    } else if path.starts_with(best_prefix) && best_prefix != "/" {
        &path[best_prefix.len()..]
    } else {
        path
    };

    (best_fs, relative.to_string())
}

// ====================== ГЛОБАЛЬНЫЙ VFS ======================

pub static VFS: MutexLazy<Vfs> = MutexLazy::new(Vfs::new);