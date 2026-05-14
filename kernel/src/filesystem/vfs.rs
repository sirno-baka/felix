use alloc::boxed::Box;
// filesystem/vfs.rs
use alloc::vec::Vec;
use alloc::string::{String, ToString};
use core::iter::Once;
use crate::println;
use spin::Mutex;

// ====================== ТРЕЙТ ДЛЯ ЛЮБОЙ ФАЙЛОВОЙ СИСТЕМЫ ======================

pub trait Filesystem: Send + Sync {
    fn read_file(&mut self, path: &str) -> Option<Vec<u8>>;
    fn write_file(&mut self, path: &str, data: &[u8]) -> bool;
    fn list_directory(&mut self, path: &str);
    fn is_mounted(&self) -> bool;          // этот можно оставить &self
}

// ====================== VFS ======================

pub struct Vfs {
    root_fs: Option<Box<dyn Filesystem>>,           // теперь автоматически Send + Sync
    mounts: Vec<(String, Box<dyn Filesystem>)>,     // то же самое
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

    /// Теперь возвращает mutable ссылку
    fn resolve(&mut self, path: &str) -> (&mut dyn Filesystem, String) {
        let path = if path.is_empty() { "/" } else { path };

        // Ищем самый длинный подходящий mount point
        let mut best_fs = self.root_fs.as_mut().unwrap_or_else(|| panic!("[VFS] No root filesystem set!"));
        let mut best_prefix = "/";

        for (mp, fs) in &mut self.mounts {
            if path.starts_with(mp.as_str()) && mp.len() > best_prefix.len() {
                best_fs = fs;
                best_prefix = mp;
            }
        }

        // Вычисляем относительный путь внутри выбранной ФС
        let relative = if path == best_prefix {
            "/"
        } else if path.starts_with(best_prefix) {
            &path[best_prefix.len()..]
        } else {
            path
        };
        println!("[VFS] best_prefix: {}", best_prefix);
        println!("[VFS] relative: {}", relative);

        let relative = if relative.is_empty() { "/" } else { relative };
        (best_fs.as_mut(), relative.to_string())
    }

    // ====================== ПУБЛИЧНЫЕ МЕТОДЫ ======================

    pub fn read_file(&mut self, path: &str) -> Option<Vec<u8>> {
        let (fs, rel_path) = self.resolve(path);
        fs.read_file(&rel_path)
    }

    pub fn write_file(&mut self, path: &str, data: &[u8]) -> bool {
        let (fs, rel_path) = self.resolve(path);
        fs.write_file(&rel_path, data)
    }

    pub fn list_directory(&mut self, path: &str) {
        let (fs, rel_path) = self.resolve(path);
        fs.list_directory(&rel_path);
    }
}
// ====================== ГЛОБАЛЬНЫЙ VFS ======================

pub static mut VFS: Option<Mutex<Vfs>> = None;