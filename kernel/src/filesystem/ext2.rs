use alloc::vec::Vec;
// fs/ext2.rs
use core::mem;
use crate::drivers::disk::{Disk, DISK};
use crate::println;

// ====================== ОСНОВНЫЕ СТРУКТУРЫ EXT2 ======================

#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
pub struct Ext2SuperBlock {
    pub s_inodes_count: u32,
    pub s_blocks_count: u32,
    pub s_r_blocks_count: u32,
    pub s_free_blocks_count: u32,
    pub s_free_inodes_count: u32,
    pub s_first_data_block: u32,
    pub s_log_block_size: u32,
    pub s_log_frag_size: u32,
    pub s_blocks_per_group: u32,
    pub s_frags_per_group: u32,
    pub s_inodes_per_group: u32,
    pub s_mtime: u32,
    pub s_wtime: u32,
    pub s_mnt_count: u16,
    pub s_max_mnt_count: u16,
    pub s_magic: u16,
    pub s_state: u16,
    pub s_errors: u16,
    pub s_minor_rev_level: u16,
    pub s_lastcheck: u32,
    pub s_checkinterval: u32,
    pub s_creator_os: u32,
    pub s_rev_level: u32,
    pub s_def_resuid: u16,
    pub s_def_resgid: u16,

    // EXT2_DYNAMIC_REV (revision >= 1)
    pub s_first_ino: u32,
    pub s_inode_size: u16,
    pub s_block_group_nr: u16,
    pub s_feature_compat: u32,
    pub s_feature_incompat: u32,
    pub s_feature_ro_compat: u32,
    pub s_uuid: [u8; 16],
    pub s_volume_name: [u8; 16],
    // ... дальше можно добавить остальные поля по мере необходимости
    _padding: [u8; 896], // чтобы структура была ровно 1024 байта
}

#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
pub struct Ext2BlockGroupDescriptor {
    pub bg_block_bitmap: u32,
    pub bg_inode_bitmap: u32,
    pub bg_inode_table: u32,
    pub bg_free_blocks_count: u16,
    pub bg_free_inodes_count: u16,
    pub bg_used_dirs_count: u16,
    pub bg_pad: u16,
    _reserved: [u8; 12],
}

#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
pub struct Ext2Inode {
    pub i_mode: u16,
    pub i_uid: u16,
    pub i_size: u32,        // lower 32 bits
    pub i_atime: u32,
    pub i_ctime: u32,
    pub i_mtime: u32,
    pub i_dtime: u32,
    pub i_gid: u16,
    pub i_links_count: u16,
    pub i_blocks: u32,      // количество 512-байтных секторов
    pub i_flags: u32,
    pub i_osd1: u32,
    pub i_block: [u32; 15], // 12 direct + 1 single + 1 double + 1 triple
    pub i_generation: u32,
    pub i_file_acl: u32,
    pub i_dir_acl: u32,
    pub i_faddr: u32,
    pub i_osd2: [u8; 12],
}

#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
pub struct Ext2DirEntry {
    pub inode: u32,
    pub rec_len: u16,
    pub name_len: u8,
    pub file_type: u8,
    // имя переменной длины
}

// ====================== ВСПОМОГАТЕЛЬНЫЕ ФУНКЦИИ ======================

const EXT2_MAGIC: u16 = 0xEF53;

pub struct Ext2 {
    pub superblock: Ext2SuperBlock,
    block_size: u32,
    pub mounted: bool,
    disk: &'static mut Disk,
}

impl Ext2 {
    /// Создаём файловую систему, привязанную к конкретному диску
    pub fn new(disk: &'static mut Disk) -> Self {
        Ext2 {
            superblock: unsafe { mem::zeroed() },
            block_size: 0,
            mounted: false,
            disk,
        }
    }

    // ====================== НИЗКОУРОВНЕВЫЕ ЧТЕНИЕ/ЗАПИСЬ ======================

    /// Читаем блоки ext2
    unsafe fn read_blocks(&self, block: u32, buf: *mut u8, count: u32) {
        if self.block_size == 0 {
            println!("[EXT2] Ошибка: read_blocks вызван до mount()!");
            return;
        }
        let sectors_per_block = self.block_size / 512;
        let lba = block as u64 * sectors_per_block as u64;
        self.disk.read(buf as *mut u32, lba, (sectors_per_block * count) as u16);
    }

    /// Записываем блоки ext2 (симметрично read_blocks)
    unsafe fn write_blocks(&self, block: u32, buf: *const u8, count: u32) {
        if self.block_size == 0 {
            println!("[EXT2] Ошибка: write_blocks вызван до mount()!");
            return;
        }
        let sectors_per_block = self.block_size / 512;
        let lba = block as u64 * sectors_per_block as u64;
        self.disk.write(buf as *const u32, lba, (sectors_per_block * count) as u16);
    }

    // ====================== MOUNT ======================
    pub fn mount(&mut self) {
        if !self.disk.enabled {
            println!("[EXT2] Disk not enabled!");
            return;
        }

        let mut superblock_buf = [0u8; 1024];

        unsafe {
            self.disk.read(superblock_buf.as_mut_ptr() as *mut u32, 2, 2);
            core::ptr::copy_nonoverlapping(
                superblock_buf.as_ptr(),
                &mut self.superblock as *mut Ext2SuperBlock as *mut u8,
                1024,
            );
        }

        if self.superblock.s_magic != EXT2_MAGIC {
            let s_magic = self.superblock.s_magic;
            println!("[EXT2] Invalid magic: 0x{:X} (ожидали 0xEF53)", s_magic);
            return;
        }

        self.block_size = 1024u32 << self.superblock.s_log_block_size;
        self.mounted = true;
        let s_block_count = self.superblock.s_blocks_count;
        let s_inodes_count = self.superblock.s_inodes_count;
        println!("[EXT2] Mounted successfully!");
        println!("      Volume: {:?}", core::str::from_utf8(&self.superblock.s_volume_name[..]).unwrap_or("???"));
        println!("      Block size: {} bytes", self.block_size);
        println!("      Total blocks: {}", s_block_count);
        println!("      Inodes: {}", s_inodes_count);
    }

    // ====================== BLOCK GROUP & INODE ======================

    pub fn get_bg_descriptor(&self, group: u32) -> Ext2BlockGroupDescriptor {
        let bgd_block = if self.superblock.s_first_data_block == 0 { 1 } else { self.superblock.s_first_data_block };
        let bgd_offset = bgd_block as u64 * self.block_size as u64 / 512;

        let mut bgd: Ext2BlockGroupDescriptor = unsafe { mem::zeroed() };
        unsafe {
            let lba = bgd_offset + (group as u64 * mem::size_of::<Ext2BlockGroupDescriptor>() as u64 / 512);
            self.disk.read(&mut bgd as *mut _ as *mut u32, lba, 1);
        }
        bgd
    }

    pub fn read_inode(&self, inode_num: u32) -> Ext2Inode {
        let group = (inode_num - 1) / self.superblock.s_inodes_per_group;
        let bgd = self.get_bg_descriptor(group);

        let index_in_group = (inode_num - 1) % self.superblock.s_inodes_per_group;
        let inode_table_block = bgd.bg_inode_table;
        let inode_offset = index_in_group as u64 * self.superblock.s_inode_size as u64;

        let block_offset = inode_offset / self.block_size as u64;
        let byte_offset = inode_offset % self.block_size as u64;

        let mut inode: Ext2Inode = unsafe { mem::zeroed() };

        unsafe {
            let lba = (inode_table_block as u64 + block_offset) * (self.block_size / 512) as u64;
            let mut block_buf = [0u8; 4096];
            self.disk.read(block_buf.as_mut_ptr() as *mut u32, lba, (self.block_size / 512) as u16);

            core::ptr::copy_nonoverlapping(
                block_buf.as_ptr().add(byte_offset as usize),
                &mut inode as *mut _ as *mut u8,
                mem::size_of::<Ext2Inode>(),
            );
        }
        inode
    }

    pub fn write_inode(&self, inode_num: u32, inode: &Ext2Inode) {
        if !self.mounted { return; }

        let group = (inode_num - 1) / self.superblock.s_inodes_per_group;
        let bgd = self.get_bg_descriptor(group);

        let index_in_group = (inode_num - 1) % self.superblock.s_inodes_per_group;
        let inode_table_block = bgd.bg_inode_table;
        let inode_offset = index_in_group as u64 * self.superblock.s_inode_size as u64;

        let block_offset = inode_offset / self.block_size as u64;
        let byte_offset = inode_offset % self.block_size as u64;

        unsafe {
            let lba = (inode_table_block as u64 + block_offset) * (self.block_size / 512) as u64;
            let mut block_buf = [0u8; 4096];

            self.disk.read(block_buf.as_mut_ptr() as *mut u32, lba, (self.block_size / 512) as u16);

            core::ptr::copy_nonoverlapping(
                inode as *const Ext2Inode as *const u8,
                block_buf.as_mut_ptr().add(byte_offset as usize),
                mem::size_of::<Ext2Inode>(),
            );

            self.write_blocks(inode_table_block + block_offset as u32, block_buf.as_ptr(), 1);
        }
    }

    // ====================== DIRECTORY HELPERS ======================

    /// Найти inode по имени внутри директории (только direct блоки)
    fn find_dir_entry(&self, dir_inode_num: u32, name: &str) -> Option<u32> {
        let inode = self.read_inode(dir_inode_num);
        if (inode.i_mode & 0xF000) != 0x4000 {
            return None; // не директория
        }

        for i in 0..12 {
            if inode.i_block[i] == 0 { break; }

            let mut block_buf = [0u8; 4096];
            unsafe { self.read_blocks(inode.i_block[i], block_buf.as_mut_ptr(), 1); }

            let mut offset = 0usize;
            while offset < self.block_size as usize {
                let entry = unsafe { &*(block_buf.as_ptr().add(offset) as *const Ext2DirEntry) };
                if entry.inode == 0 { break; }

                let name_slice = &block_buf[offset + 8..offset + 8 + entry.name_len as usize];
                if core::str::from_utf8(name_slice).unwrap_or("") == name {
                    return Some(entry.inode);
                }

                offset += entry.rec_len as usize;
                if entry.rec_len == 0 { break; }
            }
        }
        None
    }

    // ====================== PATH RESOLVER ======================

    /// Разрешает путь вида "/dir/subdir/file.txt" в номер inode
    /// Поддерживает только абсолютные пути (начинаются с /)
    pub fn resolve_path(&self, path: &str) -> Option<u32> {
        if !self.mounted {
            return None;
        }

        // root
        if path == "/" || path.is_empty() {
            return Some(2);
        }

        let mut current_inode = 2u32;

        for component in path.split('/').filter(|s| !s.is_empty()) {
            if let Some(next_inode) = self.find_dir_entry(current_inode, component) {
                current_inode = next_inode;
            } else {
                println!("[EXT2] Path component not found: '{}'", component);
                return None;
            }
        }
        Some(current_inode)
    }

    // ====================== FILE I/O ПО INODE ======================

    /// Прочитать файл по inode (только direct блоки)
    pub fn read_file_by_inode(&self, inode_num: u32) -> Option<alloc::vec::Vec<u8>> {
        let inode = self.read_inode(inode_num);
        if (inode.i_mode & 0xF000) != 0x8000 {
            println!("[EXT2] Not a regular file!");
            return None;
        }

        let file_size = inode.i_size as usize;
        let mut data = alloc::vec::Vec::with_capacity(file_size);

        let blocks_to_read = (file_size + self.block_size as usize - 1) / self.block_size as usize;

        for i in 0..core::cmp::min(blocks_to_read, 12) {
            if inode.i_block[i] == 0 { break; }

            let mut block_buf = [0u8; 4096];
            unsafe {
                self.read_blocks(inode.i_block[i], block_buf.as_mut_ptr(), 1);
            }

            let start = i * self.block_size as usize;
            let end = core::cmp::min(start + self.block_size as usize, file_size);
            data.extend_from_slice(&block_buf[0..(end - start)]);
        }

        Some(data)
    }

    /// Записать данные в файл по inode (только direct блоки, перезапись)
    pub fn write_file_by_inode(&self, inode_num: u32, data: &[u8]) -> bool {
        let mut inode = self.read_inode(inode_num);
        if (inode.i_mode & 0xF000) != 0x8000 {
            println!("[EXT2] Not a regular file for writing!");
            return false;
        }

        let num_blocks_needed = ((data.len() as u32) + self.block_size - 1) / self.block_size;

        if num_blocks_needed > 12 {
            println!("[EXT2] File too big for simple direct blocks (max 12)!");
            return false;
        }

        for i in 0..num_blocks_needed as usize {
            if inode.i_block[i] == 0 {
                println!("[EXT2] Block {} not allocated (add block allocation later)", i);
                return false;
            }

            let start = i * self.block_size as usize;
            let end = core::cmp::min(start + self.block_size as usize, data.len());
            let chunk = &data[start..end];

            unsafe {
                self.write_blocks(inode.i_block[i], chunk.as_ptr(), 1);
            }
        }

        // обновляем inode
        inode.i_size = data.len() as u32;
        inode.i_mtime = 0; // TODO: real time
        inode.i_blocks = num_blocks_needed * (self.block_size / 512);

        self.write_inode(inode_num, &inode);
        true
    }

    // ====================== FILE I/O ПО PATH ======================

    /// Прочитать файл по пути
    pub fn read_file(&self, path: &str) -> Option<alloc::vec::Vec<u8>> {
        if let Some(inode_num) = self.resolve_path(path) {
            self.read_file_by_inode(inode_num)
        } else {
            println!("[EXT2] File not found: {}", path);
            None
        }
    }

    /// Записать файл по пути (перезаписывает существующий файл)
    pub fn write_file(&mut self, path: &str, data: &[u8]) -> bool {
        if let Some(inode_num) = self.resolve_path(path) {
            if self.write_file_by_inode(inode_num, data) {
                println!("[EXT2] Successfully wrote {} bytes to {}", data.len(), path);
                true
            } else {
                false
            }
        } else {
            println!("[EXT2] File not found for writing: {}", path);
            false
        }
    }

    // ====================== DEBUG ======================

    pub fn list_directory(&self, inode_num: u32) {
        let inode = self.read_inode(inode_num);
        if (inode.i_mode & 0xF000) != 0x4000 {
            println!("[EXT2] Not a directory!");
            return;
        }

        println!("[EXT2] Directory listing for inode {}:", inode_num);
        for i in 0..12 {
            if inode.i_block[i] == 0 { break; }

            let mut block_buf = [0u8; 4096];
            unsafe { self.read_blocks(inode.i_block[i], block_buf.as_mut_ptr(), 1); }

            let mut offset = 0usize;
            while offset < self.block_size as usize {
                let entry = unsafe { &*(block_buf.as_ptr().add(offset) as *const Ext2DirEntry) };
                if entry.inode == 0 { break; }

                let name_slice = &block_buf[offset + 8..offset + 8 + entry.name_len as usize];
                let name = core::str::from_utf8(name_slice).unwrap_or("???");
                let inode = entry.inode;
                let file_type = entry.file_type;
                println!("  [{:4}] {:<20} type={}", inode, name, file_type);

                offset += entry.rec_len as usize;
                if entry.rec_len == 0 { break; }
            }
        }
    }

    /// Удобный дебаг: list_directory по пути
    pub fn list_directory_path(&self, path: &str) {
        if let Some(inode) = self.resolve_path(path) {
            self.list_directory(inode);
        } else {
            println!("[EXT2] Directory not found: {}", path);
        }
    }
}

impl crate::filesystem::Filesystem for Ext2 {
    fn read_file(&mut self, path: &str) -> Option<Vec<u8>> {
        self.read_file(path)
    }

    fn write_file(&mut self, path: &str, data: &[u8]) -> bool {
        self.write_file(path, data)
    }

    fn list_directory(&mut self, path: &str) {
        self.list_directory_path(path);
    }

    fn is_mounted(&self) -> bool {
        self.mounted
    }
}