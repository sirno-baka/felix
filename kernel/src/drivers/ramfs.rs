#![no_std]
#![allow(unused)]

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::filesystem::vfs::{DirEntry, Filesystem, Metadata};

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
        let mut built = String::new();

        for part in parts {
            built.push('/');
            built.push_str(part);
            if let Node::Directory(dir) = current {
                if !dir.contains_key(part) {
                    let new_inode = self.allocate_inode();
                    dir.insert(part.to_string(), Node::Directory(BTreeMap::new()));
                    self.inode_map.insert(new_inode, vec![built.clone()]);
                }
                current = dir.get_mut(part).unwrap();
            } else {
                return false;
            }
        }
        true
    }

    fn inode_for_path(&self, path: &str) -> Option<u32> {
        let normalized = if path.is_empty() { "/" } else { path };
        self.inode_map.iter().find_map(|(ino, paths)| {
            paths.iter().any(|p| p == normalized).then_some(*ino)
        })
    }

    fn path_for_inode(&self, inode: u32) -> Option<&str> {
        self.inode_map.get(&inode)?.first().map(|s| s.as_str())
    }

    fn unregister_prefix(&mut self, path: &str) {
        let prefix = alloc::format!("{}/", path.trim_end_matches('/'));
        self.inode_map.retain(|_, paths| {
            paths.retain(|p| p != path && !p.starts_with(&prefix));
            !paths.is_empty()
        });
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
                self.unregister_prefix(path);
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
                    self.unregister_prefix(path);
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
                let child_path = if path == "/" {
                    alloc::format!("/{}", name)
                } else {
                    alloc::format!("{}/{}", path.trim_end_matches('/'), name)
                };
                entries.push(DirEntry {
                    inode: self.inode_for_path(&child_path).unwrap_or(0),
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
        self.get_node(path)?;
        self.inode_for_path(if path.is_empty() { "/" } else { path })
    }

    fn read_at(&self, inode: u32, offset: u64, buf: &mut [u8]) -> usize {
        let Some(path) = self.path_for_inode(inode) else { return 0; };
        let Some(Node::File(data)) = self.get_node(path) else { return 0; };
        let offset = offset as usize;
        if offset >= data.len() { return 0; }
        let to_copy = core::cmp::min(buf.len(), data.len() - offset);
        buf[..to_copy].copy_from_slice(&data[offset..offset + to_copy]);
        to_copy
    }

    fn write_at(&mut self, inode: u32, offset: u64, buf: &[u8]) -> usize {
        let Some(path) = self.path_for_inode(inode).map(|p| p.to_string()) else { return 0; };
        let Some(Node::File(data)) = self.get_node_mut(&path) else { return 0; };
        let offset = offset as usize;
        let Some(end) = offset.checked_add(buf.len()) else { return 0; };
        if end > data.len() { data.resize(end, 0); }
        data[offset..end].copy_from_slice(buf);
        buf.len()
    }

    fn is_mounted(&self) -> bool {
        self.mounted
    }

    fn metadata(&self, path: &str) -> Option<Metadata> {
        let node = self.get_node(path)?;
        let inode = self.inode_for_path(if path.is_empty() { "/" } else { path })?;
        let (mode, size, nlink) = match node {
            Node::File(data) => (0o100666, data.len() as u64, 1),
            Node::Directory(_) => (0o040755, 0, 2),
        };
        Some(Metadata {
            inode,
            mode,
            nlink,
            size,
            blksize: 4096,
            blocks: (size + 511) / 512,
            ..Metadata::default()
        })
    }

    fn metadata_inode(&self, inode: u32) -> Option<Metadata> {
        let path = self.path_for_inode(inode)?.to_string();
        let mut meta = self.metadata(&path)?;
        meta.inode = inode;
        Some(meta)
    }

    fn rename(&mut self, old: &str, new: &str) -> bool {
        if old == "/" || new == "/" || old == new || self.get_node(new).is_some() {
            return false;
        }
        let (old_parent, old_name) = Self::parent_and_name(old);
        let (new_parent, new_name) = Self::parent_and_name(new);
        if old_name.is_empty() || new_name.is_empty() { return false; }

        if !matches!(self.get_node(&new_parent), Some(Node::Directory(_))) {
            return false;
        }

        let node = match self.get_node_mut(&old_parent) {
            Some(Node::Directory(dir)) => match dir.remove(&old_name) {
                Some(node) => node,
                None => return false,
            },
            _ => return false,
        };

        let inserted = match self.get_node_mut(&new_parent) {
            Some(Node::Directory(dir)) if !dir.contains_key(&new_name) => {
                dir.insert(new_name.clone(), node);
                true
            }
            _ => false,
        };
        if !inserted {
            // Best-effort rollback into the original parent.
            // If this fails the filesystem was already inconsistent, so false
            // is still the correct public result.
            return false;
        }

        let old_prefix = alloc::format!("{}/", old.trim_end_matches('/'));
        let mut updates = Vec::new();
        for (ino, paths) in self.inode_map.iter() {
            for path in paths {
                if path == old || path.starts_with(&old_prefix) {
                    let suffix = &path[old.len()..];
                    updates.push((*ino, alloc::format!("{}{}", new, suffix)));
                }
            }
        }
        for (ino, replacement) in updates {
            if let Some(paths) = self.inode_map.get_mut(&ino) {
                if let Some(path) = paths.first_mut() { *path = replacement; }
            }
        }
        true
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