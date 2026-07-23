#![no_std]
#![allow(unused)]

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::filesystem::vfs::{DirEntry, Filesystem};

// ==============================================
// RAM File System (полноценная in-memory FS)
// ==============================================

#[derive(Debug)]
enum Node {
    File(Vec<u8>),
    Directory(BTreeMap<String, Node>),
}

pub struct RamFs {
    root: Node,
    inode_counter: AtomicU32,
    inode_map: BTreeMap<u32, Vec<String>>, // inode -> path (для быстрого поиска)
    mounted: bool,
}

impl RamFs {
    pub fn new() -> Self {
        let mut fs = Self {
            root: Node::Directory(BTreeMap::new()),
            inode_counter: AtomicU32::new(2), // 1 — root
            inode_map: BTreeMap::new(),
            mounted: true,
        };
        fs.inode_map.insert(1, vec!["/".to_string()]);
        fs
    }

    fn split_path(path: &str) -> Vec<&str> {
        path.split('/').filter(|s| !s.is_empty()).collect()
    }

    fn allocate_inode(&self) -> u32 {
        self.inode_counter.fetch_add(1, Ordering::SeqCst)
    }

    fn register_inode(&mut self, inode: u32, path: &str) {
        self.inode_map.insert(inode, vec![path.to_string()]);
    }

    fn get_node_mut<'a>(&'a mut self, path: &str) -> Option<&'a mut Node> {
        let parts = Self::split_path(path);
        let mut current = &mut self.root;
        for part in parts {
            if let Node::Directory(dir) = current {
                current = dir.get_mut(part)?;
            } else {
                return None;
            }
        }
        Some(current)
    }

    fn get_node<'a>(&'a self, path: &str) -> Option<&'a Node> {
        let parts = Self::split_path(path);
        let mut current = &self.root;
        for part in parts {
            if let Node::Directory(dir) = current {
                current = dir.get(part)?;
            } else {
                return None;
            }
        }
        Some(current)
    }

    fn parent_and_name(path: &str) -> (String, String) {
        let parts: Vec<&str> = Self::split_path(path);
        if parts.is_empty() {
            return ("/".to_string(), String::new());
        }
        let name = parts.last().unwrap().to_string();
        let parent = if parts.len() == 1 {
            "/".to_string()
        } else {
            alloc::format!("/{}", &parts[..parts.len() - 1].join("/"))
        };
        (parent, name)
    }

    fn ensure_dirs(&mut self, path: &str) -> bool {
        if path == "/" {
            return true;
        }
        let parts = Self::split_path(path);
        let mut current = &mut self.root;

        for part in parts {
            if let Node::Directory(dir) = current {
                if !dir.contains_key(part) {
                    let new_inode = self.allocate_inode();
                    dir.insert(part.to_string(), Node::Directory(BTreeMap::new()));
                    self.register_inode(new_inode, &alloc::format!("/{}", part)); // упрощённо
                }
                current = dir.get_mut(part).unwrap();
            } else {
                return false;
            }
        }
        true
    }
}

impl Filesystem for RamFs {
    fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        match self.get_node(path) {
            Some(Node::File(data)) => Some(data.clone()),
            _ => None,
        }
    }

    fn write_file(&mut self, path: &str, data: &[u8]) -> bool {
        if let Some(Node::File(file_data)) = self.get_node_mut(path) {
            *file_data = data.to_vec();
            true
        } else {
            false
        }
    }

    fn create_file(&mut self, path: &str, data: &[u8]) -> bool {
        if path == "/" || path.is_empty() {
            return false;
        }
        let (parent_path, name) = Self::parent_and_name(path);

        if !self.ensure_dirs(&parent_path) {
            return false;
        }

        if let Some(Node::Directory(dir)) = self.get_node_mut(&parent_path) {
            if dir.contains_key(&name) {
                return false;
            }
            let inode = self.allocate_inode();
            dir.insert(name.clone(), Node::File(data.to_vec()));
            self.register_inode(inode, path);
            true
        } else {
            false
        }
    }

    fn remove_file(&mut self, path: &str) -> bool {
        let (parent_path, name) = Self::parent_and_name(path);
        if let Some(Node::Directory(dir)) = self.get_node_mut(&parent_path) {
            if matches!(dir.get(&name), Some(Node::File(_))) {
                dir.remove(&name);
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    fn mkdir(&mut self, path: &str) -> bool {
        if path == "/" || path.is_empty() {
            return false;
        }
        let (parent_path, name) = Self::parent_and_name(path);

        if !self.ensure_dirs(&parent_path) {
            return false;
        }

        if let Some(Node::Directory(dir)) = self.get_node_mut(&parent_path) {
            if dir.contains_key(&name) {
                return false;
            }
            let inode = self.allocate_inode();
            dir.insert(name.clone(), Node::Directory(BTreeMap::new()));
            self.register_inode(inode, path);
            true
        } else {
            false
        }
    }

    fn rmdir(&mut self, path: &str) -> bool {
        let (parent_path, name) = Self::parent_and_name(path);
        if let Some(Node::Directory(dir)) = self.get_node_mut(&parent_path) {
            if let Some(Node::Directory(sub)) = dir.get(&name) {
                if sub.is_empty() {
                    dir.remove(&name);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        }
    }

    fn list_directory_entries(&self, path: &str) -> Option<Vec<DirEntry>> {
        let node = self.get_node(path)?;
        if let Node::Directory(dir) = node {
            let mut entries = Vec::new();
            for (name, node) in dir.iter() {
                let (file_type, size) = match node {
                    Node::File(data) => (1u8, data.len() as u32),
                    Node::Directory(_) => (2u8, 0),
                };
                entries.push(DirEntry {
                    inode: 0, // можно улучшить
                    name: name.clone(),
                    file_type,
                    size,
                });
            }
            Some(entries)
        } else {
            None
        }
    }

    fn resolve_path(&self, path: &str) -> Option<u32> {
        // Простая заглушка, при необходимости — поиск по inode_map
        if path == "/" {
            Some(1)
        } else {
            Some(42)
        }
    }

    /// ========== ДОДЕЛАНО: read_at / write_at ==========
    fn read_at(&self, inode: u32, offset: u64, buf: &mut [u8]) -> usize {
        // Ищем файл по inode (упрощённо — можно улучшить)
        if inode == 1 {
            return 0; // root — не файл
        }

        // Для простоты сейчас ищем среди всех файлов (в реальном ядре лучше хранить отдельный map inode -> data)
        // Здесь используем рекурсивный поиск
        fn find_file<'a>(node: &'a Node, target_inode: u32, current_inode: &mut u32) -> Option<&'a Vec<u8>> {
            match node {
                Node::File(data) => {
                    if *current_inode == target_inode {
                        Some(data)
                    } else {
                        *current_inode += 1;
                        None
                    }
                }
                Node::Directory(dir) => {
                    *current_inode += 1; // директория тоже занимает inode
                    for child in dir.values() {
                        if let Some(data) = find_file(child, target_inode, current_inode) {
                            return Some(data);
                        }
                    }
                    None
                }
            }
        }

        let mut current_inode = 1u32;
        if let Some(data) = find_file(&self.root, inode, &mut current_inode) {
            let offset = offset as usize;
            if offset >= data.len() {
                return 0;
            }
            let to_copy = core::cmp::min(buf.len(), data.len() - offset);
            buf[..to_copy].copy_from_slice(&data[offset..offset + to_copy]);
            to_copy
        } else {
            0
        }
    }

    fn write_at(&mut self, inode: u32, offset: u64, buf: &[u8]) -> usize {
        if inode == 1 {
            return 0;
        }

        fn find_file_mut<'a>(node: &'a mut Node, target_inode: u32, current_inode: &mut u32) -> Option<&'a mut Vec<u8>> {
            match node {
                Node::File(data) => {
                    if *current_inode == target_inode {
                        Some(data)
                    } else {
                        *current_inode += 1;
                        None
                    }
                }
                Node::Directory(dir) => {
                    *current_inode += 1;
                    for child in dir.values_mut() {
                        if let Some(data) = find_file_mut(child, target_inode, current_inode) {
                            return Some(data);
                        }
                    }
                    None
                }
            }
        }

        let mut current_inode = 1u32;
        if let Some(data) = find_file_mut(&mut self.root, inode, &mut current_inode) {
            let offset = offset as usize;
            let end = offset + buf.len();

            if end > data.len() {
                data.resize(end, 0);
            }

            let to_write = buf.len();
            data[offset..offset + to_write].copy_from_slice(buf);
            to_write
        } else {
            0
        }
    }

    fn is_mounted(&self) -> bool {
        self.mounted
    }

    fn format(
        _disk: &mut crate::drivers::disk::Disk,
        _partition_offset: u64,
        _total_sectors: u64,
        _block_size: u32,
    ) -> Self {
        Self::new()
    }
}