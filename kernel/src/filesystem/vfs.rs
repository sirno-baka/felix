// kernel/src/filesystem/vfs.rs
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use crate::println;
use crate::sync::mutex::Mutex;
use crate::sync::MutexLazy;

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub inode: u32,
    pub name: String,
    pub file_type: u8,     // 1 = regular file, 2 = directory, etc.
    pub size: u32,         // размер файла (0 для директорий)
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
        let path = if path.is_empty() { "/" } else { path };

        // Специальная обработка для корневой директории
        if path == "/" {
            let mut entries = inner.root_fs.as_ref()
                .and_then(|fs| fs.list_directory_entries("/"))
                .unwrap_or_default();

            // Добавляем точки монтирования как синтетические директории
            for (mount_point, _fs) in &inner.mounts {
                let name = mount_point.trim_start_matches('/');
                if !name.is_empty() {
                    if !entries.iter().any(|e| e.name == name) {
                        entries.push(DirEntry {
                            inode: 0,
                            name: name.to_string(),
                            file_type: 2, // 2 = Directory (S_IFDIR)
                            size: 0,
                        });
                    }
                }
            }
            return Some(entries);
        }

        // Для всех остальных путей используем механизм разрешения
        let (fs, rel_path, _fs_id) = resolve(&inner, path);
        fs.list_directory_entries(&rel_path)
    }

    pub fn resolve_path(&self, path: &str) -> Option<u32> {
        let inner = self.inner.lock();
        let (fs, rel_path, fs_id) = resolve(&inner, path);

        // Кодируем fs_id в старшие 8 битах, локальный inode в младшие 24 бита
        fs.resolve_path(&rel_path).map(|local_inode| {
            ((fs_id as u32) << 24) | (local_inode & 0x00FFFFFF)
        })
    }

    pub fn read_at(&self, global_inode: u32, offset: u64, buf: &mut [u8]) -> usize {
        let inner = self.inner.lock();
        let fs_id = (global_inode >> 24) as usize;
        let local_inode = global_inode & 0x00FFFFFF;

        if fs_id == 0 {
            if let Some(fs) = &inner.root_fs {
                return fs.read_at(local_inode, offset, buf);
            }
        } else if fs_id > 0 && fs_id <= inner.mounts.len() {
            return inner.mounts[fs_id - 1].1.read_at(local_inode, offset, buf);
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
        } else if fs_id > 0 && fs_id <= inner.mounts.len() {
            return inner.mounts[fs_id - 1].1.write_at(local_inode, offset, buf);
        }
        0
    }
}

// ====================== ВСПОМОГАТЕЛЬНЫЕ ФУНКЦИИ ======================

fn resolve<'a>(inner: &'a VfsInner, path: &'a str) -> (&'a dyn Filesystem, String, u8) {
    let path = if path.is_empty() { "/" } else { path };
    let mut best_fs: &dyn Filesystem = inner
        .root_fs
        .as_ref()
        .expect("[VFS] No root filesystem set!")
        .as_ref();
    let mut best_prefix = "/";
    let mut best_id: u8 = 0; // 0 = root_fs

    for (idx, (mp, fs_box)) in inner.mounts.iter().enumerate() {
        if path.starts_with(mp) && mp.len() > best_prefix.len() {
            best_fs = fs_box.as_ref();
            best_prefix = mp;
            best_id = (idx + 1) as u8; // 1-based ID для точек монтирования
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

fn resolve_mut<'a>(inner: &'a mut VfsInner, path: &'a str) -> (&'a mut dyn Filesystem, String, u8) {
    let path = if path.is_empty() { "/" } else { path };
    let mut best_fs: &mut dyn Filesystem = inner
        .root_fs
        .as_mut()
        .expect("[VFS] No root filesystem set!")
        .as_mut();
    let mut best_prefix = "/";
    let mut best_id: u8 = 0;

    for (idx, (mp, fs_box)) in inner.mounts.iter_mut().enumerate() {
        if path.starts_with(mp.as_str()) && mp.len() > best_prefix.len() {
            best_fs = fs_box.as_mut();
            best_prefix = mp;
            best_id = (idx + 1) as u8;
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