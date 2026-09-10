use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp;

use crate::device::char::CharDevice;
use crate::disk::interface::BlockDevice;
use crate::filesystem::file::DeviceKind;
use crate::filesystem::vfs::{DirEntry, Filesystem};
use crate::spin;
use crate::sync::MutexLazy;
use crate::sync::mutex::Mutex;

pub type SharedBlockDevice = Arc<spin::Mutex<dyn BlockDevice>>;

pub enum DeviceType {
    /// Shared with mounted filesystems; /dev and VFS see the exact same media.
    Block(SharedBlockDevice),
    Char(Box<dyn CharDevice>),
}

pub struct DeviceNode {
    pub name: String,
    pub inode: u32,
    pub dev_type: DeviceType,
}

fn new_devices() -> Mutex<Vec<DeviceNode>> { Mutex::new(Vec::new()) }
fn new_inode_counter() -> Mutex<u32> { Mutex::new(1) }

/// Device nodes outlive the DevFS mount object so hotplug drivers can publish
/// and withdraw /dev entries at runtime.
static DEVICES: MutexLazy<Mutex<Vec<DeviceNode>>> = MutexLazy::new(new_devices);
static NEXT_INODE: MutexLazy<Mutex<u32>> = MutexLazy::new(new_inode_counter);

pub struct DevFS;

impl DevFS {
    pub fn new() -> Self { Self }

    fn alloc_inode() -> u32 {
        let mut next = NEXT_INODE.get().lock();
        let inode = *next;
        *next = next.saturating_add(1);
        inode
    }

    pub fn register_block(&self, name: &str, dev: SharedBlockDevice) -> u32 {
        Self::register_block_global(name, dev)
    }

    pub fn register_block_global(name: &str, dev: SharedBlockDevice) -> u32 {
        let mut devices = DEVICES.get().lock();
        if let Some(existing) = devices.iter().find(|d| d.name == name) {
            return existing.inode;
        }
        let inode = Self::alloc_inode();
        devices.push(DeviceNode { name: name.into(), inode, dev_type: DeviceType::Block(dev) });
        inode
    }

    pub fn register_char(&self, name: &str, dev: Box<dyn CharDevice>) -> u32 {
        Self::register_char_global(name, dev)
    }

    pub fn register_char_global(name: &str, dev: Box<dyn CharDevice>) -> u32 {
        let mut devices = DEVICES.get().lock();
        if let Some(existing) = devices.iter().find(|d| d.name == name) {
            return existing.inode;
        }
        let inode = Self::alloc_inode();
        devices.push(DeviceNode { name: name.into(), inode, dev_type: DeviceType::Char(dev) });
        inode
    }

    pub fn unregister(name: &str) -> bool {
        let mut devices = DEVICES.get().lock();
        let before = devices.len();
        devices.retain(|d| d.name != name);
        devices.len() != before
    }

    pub fn contains(name: &str) -> bool {
        DEVICES.get().lock().iter().any(|d| d.name == name)
    }

    pub fn block_device(name: &str) -> Option<SharedBlockDevice> {
        let devices = DEVICES.get().lock();
        devices.iter().find_map(|d| {
            if d.name != name { return None; }
            match &d.dev_type {
                DeviceType::Block(dev) => Some(dev.clone()),
                DeviceType::Char(_) => None,
            }
        })
    }

    pub fn device_kind(name: &str) -> Option<DeviceKind> {
        let devices = DEVICES.get().lock();
        let node = devices.iter().find(|d| d.name == name)?;
        Some(match &node.dev_type {
            DeviceType::Block(_) => DeviceKind::Block,
            DeviceType::Char(_) => DeviceKind::Char,
        })
    }
}

impl Filesystem for DevFS {
    fn resolve_path(&self, path: &str) -> Option<u32> {
        // `/sda`, `sda`, `/dev/sda` → имя узла `sda`
        let clean_name = path
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or(path);
        if clean_name.is_empty() || clean_name == "dev" {
            return None;
        }
        let devices = DEVICES.get().lock();
        devices
            .iter()
            .find(|d| d.name == clean_name)
            .map(|d| d.inode)
    }

    fn read_at(&self, inode: u32, offset: u64, buf: &mut [u8]) -> usize {
        let devices = DEVICES.get().lock();
        let Some(node) = devices.iter().find(|d| d.inode == inode) else { return 0; };

        match &node.dev_type {
            DeviceType::Block(dev_mutex) => {
                let dev = dev_mutex.lock();
                read_from_block_device(&*dev, offset, buf)
            }
            DeviceType::Char(dev) => dev.read(offset, buf),
        }
    }

    fn write_at(&mut self, inode: u32, offset: u64, buf: &[u8]) -> usize {
        let devices = DEVICES.get().lock();
        let Some(node) = devices.iter().find(|d| d.inode == inode) else { return 0; };

        match &node.dev_type {
            DeviceType::Block(dev_mutex) => {
                let mut dev = dev_mutex.lock();
                write_to_block_device(&mut *dev, offset, buf)
            }
            DeviceType::Char(dev) => dev.write(offset, buf),
        }
    }

    fn list_directory_entries(&self, path: &str) -> Option<Vec<DirEntry>> {
        // Каталог только корень DevFS (`/`, `/dev`). Узел `/dev/sda` — файл устройства.
        let rest = path.trim_matches('/');
        if !rest.is_empty() && rest != "." && rest != "dev" {
            return None;
        }
        let devices = DEVICES.get().lock();
        Some(
            devices
                .iter()
                .map(|d| DirEntry {
                    inode: d.inode,
                    name: d.name.clone(),
                    // 2 = directory, 3 = block, 4 = char
                    file_type: match &d.dev_type {
                        DeviceType::Block(_) => 3,
                        DeviceType::Char(_) => 4,
                    },
                    size: 0,
                })
                .collect(),
        )
    }

    // DevFS не поддерживает создание/удаление файлов через обычные системные вызовы
    fn read_file(&self, _path: &str) -> Option<Vec<u8>> {
        None
    }
    fn write_file(&mut self, _path: &str, _data: &[u8]) -> bool {
        false
    }
    fn create_file(&mut self, _path: &str, _data: &[u8]) -> bool {
        false
    }
    fn remove_file(&mut self, _path: &str) -> bool {
        false
    }
    fn mkdir(&mut self, _path: &str) -> bool {
        false
    }
    fn rmdir(&mut self, _path: &str) -> bool {
        false
    }
    fn is_mounted(&self) -> bool {
        true
    }
}

// ==========================
// Read-Modify-Write для блочных устройств
// ==========================

fn read_from_block_device(dev: &dyn BlockDevice, offset: u64, buf: &mut [u8]) -> usize {
    if buf.is_empty() {
        return 0;
    }

    let sector_size = dev.sector_size() as u64;
    let start_sector = (offset / sector_size) as u32;
    let sector_offset = (offset % sector_size) as usize;

    let mut bytes_read = 0;
    // Выделяем буфер размером с сектор в куче (или используйте стек, если сектор <= 4KB)
    let mut temp_sector = vec![0u8; sector_size as usize];

    while bytes_read < buf.len() {
        let current_sector = start_sector + ((bytes_read as u64) / sector_size) as u32;
        let current_offset = if bytes_read == 0 { sector_offset } else { 0 };
        let chunk = cmp::min(
            buf.len() - bytes_read,
            sector_size as usize - current_offset,
        );

        // Читаем целый сектор
        if dev
            .read_sectors(1, current_sector, temp_sector.as_mut_ptr() as u32)
            .is_err()
        {
            break; // Ошибка чтения
        }

        // Копируем нужную часть в пользовательский буфер
        buf[bytes_read..bytes_read + chunk]
            .copy_from_slice(&temp_sector[current_offset..current_offset + chunk]);
        bytes_read += chunk;
    }
    bytes_read
}

fn write_to_block_device(dev: &mut dyn BlockDevice, offset: u64, buf: &[u8]) -> usize {
    if buf.is_empty() {
        return 0;
    }

    let sector_size = dev.sector_size() as u64;
    let start_sector = (offset / sector_size) as u32;
    let sector_offset = (offset % sector_size) as usize;

    let mut bytes_written = 0;
    let mut temp_sector = vec![0u8; sector_size as usize];

    while bytes_written < buf.len() {
        let current_sector = start_sector + ((bytes_written as u64) / sector_size) as u32;
        let current_offset = if bytes_written == 0 { sector_offset } else { 0 };
        let chunk = cmp::min(
            buf.len() - bytes_written,
            sector_size as usize - current_offset,
        );

        // 1. READ: читаем существующий сектор
        if dev
            .read_sectors(1, current_sector, temp_sector.as_mut_ptr() as u32)
            .is_err()
        {
            break;
        }

        // 2. MODIFY: заменяем только нужные байты
        temp_sector[current_offset..current_offset + chunk]
            .copy_from_slice(&buf[bytes_written..bytes_written + chunk]);

        // 3. WRITE: записываем сектор целиком обратно
        if dev
            .write_sectors(1, current_sector, temp_sector.as_mut_ptr() as u32)
            .is_err()
        {
            break;
        }

        bytes_written += chunk;
    }
    bytes_written
}
