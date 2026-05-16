// Весь файл vfs.rs можно заменить на это:

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use crate::{print, println};
use spin::Mutex;

// ====================== ТРЕЙТ ДЛЯ ЛЮБОЙ ФАЙЛОВОЙ СИСТЕМЫ ======================
pub trait Filesystem: Send + Sync {
    fn read_file(&self, path: &str) -> Option<Vec<u8>>;
    fn write_file(&self, path: &str, data: &[u8]) -> bool;
    fn list_directory(&self, path: &str);
    fn is_mounted(&self) -> bool;
}

// ====================== VFS ======================
pub struct Vfs {
    root_fs: Option<Box<dyn Filesystem>>,
    mounts: Vec<(String, Box<dyn Filesystem>)>,
}

impl Vfs {
    pub fn new() -> Self {
        Vfs {
            root_fs: None,
            mounts: Vec::new(),
        }
    }

    pub fn set_root(&mut self, fs: Box<dyn Filesystem>) {
        self.root_fs = Some(fs);
    }

    pub fn mount(&mut self, mount_point: &str, fs: Box<dyn Filesystem>) {
        if !mount_point.starts_with('/') {
            println!("[VFS] Mount point must start with /");
            return;
        }
        self.mounts.push((mount_point.to_string(), fs));
        println!("[VFS] Mounted at {}", mount_point);
    }

    // ИСПРАВЛЕНО: теперь &self + &dyn Filesystem
    fn resolve(&self, path: &str) -> (&dyn Filesystem, String) {
        let path = if path.is_empty() { "/" } else { path };

        let mut best_fs: &dyn Filesystem = self.root_fs
            .as_ref()
            .expect("[VFS] No root filesystem set!")
            .as_ref();

        let mut best_prefix = "/";

        for (mp, fs_box) in &self.mounts {
            if path.starts_with(mp.as_str()) && mp.len() > best_prefix.len() {
                best_fs = fs_box.as_ref();
                best_prefix = mp;
            }
        }

        let relative = if path == best_prefix {
            "/"
        } else if path.starts_with(best_prefix) {
            &path[best_prefix.len()..]
        } else {
            path
        };

        let relative = if relative.is_empty() { "/" } else { relative };
        (best_fs, relative.to_string())
    }

    pub fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        let (fs, rel_path) = self.resolve(path);
        fs.read_file(&rel_path)
    }

    pub fn write_file(&self, path: &str, data: &[u8]) -> bool {
        let (fs, rel_path) = self.resolve(path);
        fs.write_file(&rel_path, data)
    }

    pub fn list_directory(&self, path: &str) {
        let (fs, rel_path) = self.resolve(path);
        fs.list_directory(&rel_path);
    }
}

// ====================== ГЛОБАЛЬНЫЙ VFS ======================
pub static VFS: Mutex<Option<Vfs>> = Mutex::new(None);