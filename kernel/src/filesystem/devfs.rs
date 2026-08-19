use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp;

use crate::sync::mutex::Mutex;
use crate::filesystem::vfs::{Filesystem, DirEntry};
use crate::disk::interface::BlockDevice;
use crate::device::char::CharDevice;

pub enum DeviceType {
    // Mutex необходим, так как write_sectors требует &mut self
    Block(Mutex<Box<dyn BlockDevice>>),
    Char(Box<dyn CharDevice>),
}

pub struct DeviceNode {
    pub name: String,
    pub inode: u32,
    pub dev_type: DeviceType,
}

pub struct DevFS {
    devices: Mutex<Vec<DeviceNode>>,
    next_inode: Mutex<u32>,
}

impl DevFS {
    pub fn new() -> Self {
        Self {
            devices: Mutex::new(Vec::new()),
            next_inode: Mutex::new(1), // inode 0 зарезервируем или начнем с 1
        }
    }

    pub fn register_block(&self, name: &str, dev: Box<dyn BlockDevice>) -> u32 {
        let mut devices = self.devices.lock();
        let mut next_inode = self.next_inode.lock();
        let inode = *next_inode;
        *next_inode += 1;

        devices.push(DeviceNode {
            name: name.into(),
            inode,
            dev_type: DeviceType::Block(Mutex::new(dev)),
        });
        inode
    }

    pub fn register_char(&self, name: &str, dev: Box<dyn CharDevice>) -> u32 {
        let mut devices = self.devices.lock();
        let mut next_inode = self.next_inode.lock();
        let inode = *next_inode;
        *next_inode += 1;

        devices.push(DeviceNode {
            name: name.into(),
            inode,
            dev_type: DeviceType::Char(dev),
        });
        inode
    }
}

impl Filesystem for DevFS {
    fn resolve_path(&self, path: &str) -> Option<u32> {
        // Убираем ведущий слеш для сравнения, если он есть
        let clean_name = path.strip_prefix('/').unwrap_or(path);
        let devices = self.devices.lock();
        devices.iter().find(|d| d.name == clean_name).map(|d| d.inode)
    }

    fn read_at(&self, inode: u32, offset: u64, buf: &mut [u8]) -> usize {
        let devices = self.devices.lock();
        let node = devices.iter().find(|d| d.inode == inode).unwrap();

        match &node.dev_type {
            DeviceType::Block(dev_mutex) => {
                let dev = dev_mutex.lock();
                read_from_block_device(dev.as_ref(), offset, buf)
            }
            DeviceType::Char(dev) => dev.read(offset, buf),
        }
    }

    fn write_at(&mut self, inode: u32, offset: u64, buf: &[u8]) -> usize {
        let devices = self.devices.lock();
        let node = devices.iter().find(|d| d.inode == inode).unwrap();

        match &node.dev_type {
            DeviceType::Block(dev_mutex) => {
                let mut dev = dev_mutex.lock();
                write_to_block_device(dev.as_mut(), offset, buf)
            }
            DeviceType::Char(dev) => dev.write(offset, buf),
        }
    }

    fn list_directory_entries(&self, _path: &str) -> Option<Vec<DirEntry>> {
        let devices = self.devices.lock();
        Some(devices.iter().map(|d| DirEntry {
            inode: d.inode,
            name: d.name.clone(),
            // 2 = S_IFCHR (символьное), 3 = S_IFBLK (блочное)
            file_type: match d.dev_type {
                DeviceType::Block(_) => 3,
                DeviceType::Char(_) => 2,
            },
            size: 0,
        }).collect())
    }

    // DevFS не поддерживает создание/удаление файлов через обычные системные вызовы
    fn read_file(&self, _path: &str) -> Option<Vec<u8>> { None }
    fn write_file(&mut self, _path: &str, _data: &[u8]) -> bool { false }
    fn create_file(&mut self, _path: &str, _data: &[u8]) -> bool { false }
    fn remove_file(&mut self, _path: &str) -> bool { false }
    fn mkdir(&mut self, _path: &str) -> bool { false }
    fn rmdir(&mut self, _path: &str) -> bool { false }
    fn is_mounted(&self) -> bool { true }
}

// ==========================
// Read-Modify-Write для блочных устройств
// ==========================

fn read_from_block_device(dev: &dyn BlockDevice, offset: u64, buf: &mut [u8]) -> usize {
    if buf.is_empty() { return 0; }

    let sector_size = dev.sector_size() as u64;
    let start_sector = (offset / sector_size) as u32;
    let sector_offset = (offset % sector_size) as usize;

    let mut bytes_read = 0;
    // Выделяем буфер размером с сектор в куче (или используйте стек, если сектор <= 4KB)
    let mut temp_sector = vec![0u8; sector_size as usize];

    while bytes_read < buf.len() {
        let current_sector = start_sector + ((bytes_read as u64) / sector_size) as u32;
        let current_offset = if bytes_read == 0 { sector_offset } else { 0 };
        let chunk = cmp::min(buf.len() - bytes_read, sector_size as usize - current_offset);

        // Читаем целый сектор
        if dev.read_sectors(1, current_sector, temp_sector.as_mut_ptr() as u32).is_err() {
            break; // Ошибка чтения
        }

        // Копируем нужную часть в пользовательский буфер
        buf[bytes_read..bytes_read + chunk].copy_from_slice(
            &temp_sector[current_offset..current_offset + chunk]
        );
        bytes_read += chunk;
    }
    bytes_read
}

fn write_to_block_device(dev: &mut dyn BlockDevice, offset: u64, buf: &[u8]) -> usize {
    if buf.is_empty() { return 0; }

    let sector_size = dev.sector_size() as u64;
    let start_sector = (offset / sector_size) as u32;
    let sector_offset = (offset % sector_size) as usize;

    let mut bytes_written = 0;
    let mut temp_sector = vec![0u8; sector_size as usize];

    while bytes_written < buf.len() {
        let current_sector = start_sector + ((bytes_written as u64) / sector_size) as u32;
        let current_offset = if bytes_written == 0 { sector_offset } else { 0 };
        let chunk = cmp::min(buf.len() - bytes_written, sector_size as usize - current_offset);

        // 1. READ: читаем существующий сектор
        if dev.read_sectors(1, current_sector, temp_sector.as_mut_ptr() as u32).is_err() {
            break;
        }

        // 2. MODIFY: заменяем только нужные байты
        temp_sector[current_offset..current_offset + chunk]
            .copy_from_slice(&buf[bytes_written..bytes_written + chunk]);

        // 3. WRITE: записываем сектор целиком обратно
        if dev.write_sectors(1, current_sector, temp_sector.as_mut_ptr() as u32).is_err() {
            break;
        }

        bytes_written += chunk;
    }
    bytes_written
}