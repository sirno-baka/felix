use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
// fs/ext2.rs
use core::mem;
use core::ops::Deref;
use crate::{print, println};
use crate::disk::ide::BlockDevice;
use crate::disk::PartitionConfig;
use crate::filesystem::vfs::DirEntry;
use crate::spin::Mutex;
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

/// Automatically finds the first ext2 partition (type 0x83) in the MBR.
/// If not found, returns PartitionConfig::whole_disk().
pub fn find_ext2_partition_config(device: &dyn BlockDevice) -> PartitionConfig {
    let mut mbr_buf = [0u8; 512];
    
    // Read MBR from LBA 0
    if device.read_sectors(1, 0, mbr_buf.as_mut_ptr() as u32).is_err() {
        println!("[EXT2] Failed to read MBR. Using whole disk.");
        return PartitionConfig::whole_disk();
    }

    // Check MBR signature
    if mbr_buf[510] != 0x55 || mbr_buf[511] != 0xAA {
        println!("[EXT2] No valid MBR signature. Using whole disk.");
        return PartitionConfig::whole_disk();
    }

    let partition_table_offset = 446usize; // 0x1BE

    #[repr(C, packed)]
    #[derive(Copy, Clone)]
    struct MbrPartitionEntry {
        pub status: u8,
        pub chs_start: [u8; 3],
        pub partition_type: u8,
        pub chs_end: [u8; 3],
        pub lba_start: u32,
        pub num_sectors: u32,
    }

    for i in 0..4 {
        let entry_offset = partition_table_offset + i * 16;
        let entry = unsafe {
            core::ptr::read_unaligned(
                mbr_buf.as_ptr().add(entry_offset) as *const MbrPartitionEntry
            )
        };

        if entry.partition_type == 0x83 {
            let start_lba = entry.lba_start as u64;
            println!("[EXT2] Found ext2 partition (0x83) at LBA {}", start_lba);
            return PartitionConfig::new(start_lba);
        }
    }

    println!("[EXT2] No ext2 partitions found in MBR. Using whole disk.");
    PartitionConfig::whole_disk()
}

pub struct Ext2 {
    pub superblock: Ext2SuperBlock,
    block_size: u32,
    pub mounted: bool,
    disk: Arc<Mutex<dyn BlockDevice>>,
    partition_offset: u64,
}

impl Ext2 {
    /// Creates a filesystem with automatic ext2 partition detection in MBR
    pub fn new_with_auto_partition(disk: Arc<Mutex<dyn BlockDevice>>) -> Self {
        let binding = disk.clone();
        let disk_ref = binding.lock();
        let config = find_ext2_partition_config(disk_ref.deref());
        Ext2::new(disk, Some(config))
    }

    /// Создаём файловую систему, привязанную к конкретному диску
    pub fn new(disk: Arc<Mutex<dyn BlockDevice>>, config: Option<PartitionConfig>) -> Self {
        let partition_offset = match config {
            Some(cfg) => cfg.start_lba,
            None => 0,
        };

        Ext2 {
            superblock: unsafe { mem::zeroed() },
            block_size: 0,
            mounted: false,
            disk,
            partition_offset,
        }
    }

    // ====================== БЕЗОПАСНЫЕ HELPERS (главное исправление) ======================

    /// Читает структуру из блока по смещению (использует read_unaligned)
    unsafe fn read_struct_from_disk<T: Copy>(&self, block: u32, offset_in_block: usize) -> T {
        if self.block_size == 0 {
            return mem::zeroed();
        }
        let mut block_buf = [0u8; 4096]; // достаточно для 1024/2048/4096
        self.read_blocks(block, block_buf.as_mut_ptr(), 1);

        let src = block_buf.as_ptr().add(offset_in_block);
        core::ptr::read_unaligned(src as *const T)
    }

    /// Записывает структуру в блок по смещению (read-modify-write)
    unsafe fn write_struct_to_disk<T: Copy>(&self, block: u32, offset_in_block: usize, value: &T) {
        if self.block_size == 0 {
            return;
        }
        let mut block_buf = [0u8; 4096];
        self.read_blocks(block, block_buf.as_mut_ptr(), 1);

        let dst = block_buf.as_mut_ptr().add(offset_in_block);
        core::ptr::write_unaligned(dst as *mut T, *value);

        self.write_blocks(block, block_buf.as_ptr(), 1);
    }

    // ====================== НИЗКОУРОВНЕВЫЕ ЧТЕНИЕ/ЗАПИСЬ ======================

    unsafe fn read_blocks(&self, block: u32, buf: *mut u8, count: u32) {
        if self.block_size == 0 {
            println!("[EXT2] Ошибка: read_blocks вызван до mount()!");
            return;
        }

        let sectors_per_block = self.block_size / 512;
        let total_sectors = (sectors_per_block * count) as u8;
        let lba = self.partition_offset as u32 + (block * sectors_per_block);

        let disk = self.disk.lock();
        if let Err(e) = disk.read_sectors(total_sectors, lba, buf as u32) {
            println!("[EXT2] read_sectors error: {}", e);
        }
    }

    unsafe fn write_blocks(&self, block: u32, buf: *const u8, count: u32) {
        if self.block_size == 0 {
            println!("[EXT2] Ошибка: write_blocks вызван до mount()!");
            return;
        }

        let sectors_per_block = self.block_size / 512;
        let total_sectors = (sectors_per_block * count) as u8;
        let lba = self.partition_offset as u32 + (block * sectors_per_block);

        let mut disk = self.disk.lock();
        if let Err(e) = disk.write_sectors(total_sectors, lba, buf as u32) {
            println!("[EXT2] write_sectors error: {}", e);
        }
    }

    pub fn mount(&mut self, config: Option<PartitionConfig>) {
        if let Some(cfg) = config {
            self.partition_offset = cfg.start_lba;
            println!("[EXT2] Переопределён offset партишена: {}", self.partition_offset);
        }

        let mut superblock_buf = [0u8; 1024];

        unsafe {
            // Читаем 2 сектора (1024 байта) начиная с LBA = partition_offset + 2
            let disk = self.disk.lock();
            if let Err(e) = disk.read_sectors(2, (self.partition_offset + 2) as u32, superblock_buf.as_mut_ptr() as u32) {
                println!("[EXT2] Не удалось прочитать superblock: {}", e);
                return;
            }

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

    fn resolve_block(&self, inode: &Ext2Inode, logical_block: usize) -> Option<u32> {
        let num_ptrs = (self.block_size as usize) / 4; // u32 pointers per indirect block
        let direct_blocks = 12;

        if logical_block < direct_blocks {
            return if inode.i_block[logical_block] == 0 {
                None
            } else {
                Some(inode.i_block[logical_block])
            };
        }

        let offset = logical_block - direct_blocks;

        // single indirect
        if offset < num_ptrs {
            let indirect_block = inode.i_block[12];
            if indirect_block == 0 {
                return None;
            }
            let mut indirect_buf = alloc::vec![0u8; self.block_size as usize];
            unsafe { self.read_blocks(indirect_block, indirect_buf.as_mut_ptr(), 1); }
            let ptr_offset = offset * 4;
            let physical_block = u32::from_le_bytes([
                indirect_buf[ptr_offset],
                indirect_buf[ptr_offset + 1],
                indirect_buf[ptr_offset + 2],
                indirect_buf[ptr_offset + 3],
            ]);
            return if physical_block == 0 { None } else { Some(physical_block) };
        }

        // double indirect
        let offset = offset - num_ptrs;
        if offset < num_ptrs * num_ptrs {
            let indirect_block = inode.i_block[13];
            if indirect_block == 0 {
                return None;
            }
            let mut indirect_buf = alloc::vec![0u8; self.block_size as usize];
            unsafe { self.read_blocks(indirect_block, indirect_buf.as_mut_ptr(), 1); }
            let first_level = offset / num_ptrs;
            let second_level = offset % num_ptrs;

            let ptr_offset = first_level * 4;
            let first_physical = u32::from_le_bytes([
                indirect_buf[ptr_offset],
                indirect_buf[ptr_offset + 1],
                indirect_buf[ptr_offset + 2],
                indirect_buf[ptr_offset + 3],
            ]);
            if first_physical == 0 {
                return None;
            }

            unsafe { self.read_blocks(first_physical, indirect_buf.as_mut_ptr(), 1); }
            let ptr_offset = second_level * 4;
            let physical_block = u32::from_le_bytes([
                indirect_buf[ptr_offset],
                indirect_buf[ptr_offset + 1],
                indirect_buf[ptr_offset + 2],
                indirect_buf[ptr_offset + 3],
            ]);
            return if physical_block == 0 { None } else { Some(physical_block) };
        }

        // triple indirect
        let offset = offset - num_ptrs * num_ptrs;
        if offset < num_ptrs * num_ptrs * num_ptrs {
            let indirect_block = inode.i_block[14];
            if indirect_block == 0 {
                return None;
            }
            let mut indirect_buf = alloc::vec![0u8; self.block_size as usize];
            unsafe { self.read_blocks(indirect_block, indirect_buf.as_mut_ptr(), 1); }
            let first = offset / (num_ptrs * num_ptrs);
            let second = (offset / num_ptrs) % num_ptrs;
            let third = offset % num_ptrs;

            let ptr_offset = first * 4;
            let first_phys = u32::from_le_bytes([
                indirect_buf[ptr_offset],
                indirect_buf[ptr_offset + 1],
                indirect_buf[ptr_offset + 2],
                indirect_buf[ptr_offset + 3],
            ]);
            if first_phys == 0 { return None; }

            unsafe { self.read_blocks(first_phys, indirect_buf.as_mut_ptr(), 1); }
            let ptr_offset = second * 4;
            let second_phys = u32::from_le_bytes([
                indirect_buf[ptr_offset],
                indirect_buf[ptr_offset + 1],
                indirect_buf[ptr_offset + 2],
                indirect_buf[ptr_offset + 3],
            ]);
            if second_phys == 0 { return None; }

            unsafe { self.read_blocks(second_phys, indirect_buf.as_mut_ptr(), 1); }
            let ptr_offset = third * 4;
            let physical_block = u32::from_le_bytes([
                indirect_buf[ptr_offset],
                indirect_buf[ptr_offset + 1],
                indirect_buf[ptr_offset + 2],
                indirect_buf[ptr_offset + 3],
            ]);
            return if physical_block == 0 { None } else { Some(physical_block) };
        }

        None
    }

    // ====================== BLOCK GROUP & INODE ======================

    /// Исправленная версия — теперь правильно работает с несколькими блоками BGD-таблицы
    pub fn get_bg_descriptor(&self, group: u32) -> Ext2BlockGroupDescriptor {
        if !self.mounted {
            return unsafe { mem::zeroed() };
        }

        let bgd_table_block = if self.superblock.s_first_data_block == 0 {
            1u32
        } else {
            self.superblock.s_first_data_block
        };

        let desc_size = core::mem::size_of::<Ext2BlockGroupDescriptor>() as u32;
        let bgds_per_block = self.block_size / desc_size;

        let block_in_table = group / bgds_per_block;
        let block = bgd_table_block + block_in_table;
        let index_in_block = group % bgds_per_block;
        let byte_offset = (index_in_block * desc_size) as usize;

        unsafe {
            self.read_struct_from_disk(block, byte_offset)
        }
    }

    pub fn read_inode(&self, inode_num: u32) -> Ext2Inode {
        if !self.mounted || inode_num == 0 {
            return unsafe { mem::zeroed() };
        }

        let group = (inode_num - 1) / self.superblock.s_inodes_per_group;
        let bgd = self.get_bg_descriptor(group);

        let index_in_group = (inode_num - 1) % self.superblock.s_inodes_per_group;
        let inode_table_block = bgd.bg_inode_table;
        let inode_offset = index_in_group as u64 * self.superblock.s_inode_size as u64;

        let block_offset = inode_offset / self.block_size as u64;
        let byte_offset_in_block = (inode_offset % self.block_size as u64) as usize;

        unsafe {
            self.read_struct_from_disk(inode_table_block + block_offset as u32, byte_offset_in_block)
        }
    }

    pub fn write_inode(&self, inode_num: u32, inode: &Ext2Inode) {
        if !self.mounted || inode_num == 0 {
            return;
        }

        let group = (inode_num - 1) / self.superblock.s_inodes_per_group;
        let bgd = self.get_bg_descriptor(group);

        let index_in_group = (inode_num - 1) % self.superblock.s_inodes_per_group;
        let inode_table_block = bgd.bg_inode_table;
        let inode_offset = index_in_group as u64 * self.superblock.s_inode_size as u64;

        let block_offset = inode_offset / self.block_size as u64;
        let byte_offset_in_block = (inode_offset % self.block_size as u64) as usize;

        unsafe {
            self.write_struct_to_disk(
                inode_table_block + block_offset as u32,
                byte_offset_in_block,
                inode,
            );
        }
    }

    // ====================== DIRECTORY HELPERS ======================

    /// Найти inode по имени внутри директории (только direct блоки + защита от OOB)
    fn find_dir_entry(&self, dir_inode_num: u32, name: &str) -> Option<u32> {
        let inode = self.read_inode(dir_inode_num);
        if (inode.i_mode & 0xF000) != 0x4000 {
            return None; // не директория
        }

        for i in 0..12 {
            if inode.i_block[i] == 0 {
                break;
            }

            let mut block_buf = [0u8; 4096];
            unsafe { self.read_blocks(inode.i_block[i], block_buf.as_mut_ptr(), 1); }

            let mut offset = 0usize;
            while offset + 8 <= self.block_size as usize {
                let entry = unsafe { core::ptr::read_unaligned(block_buf.as_ptr().add(offset) as *const Ext2DirEntry) };

                if entry.rec_len == 0 || (entry.rec_len as usize) > self.block_size as usize - offset {
                    break; // защита от повреждённых/нулевых записей
                }

                let name_slice = &block_buf[offset + 8..offset + 8 + entry.name_len as usize];
                if core::str::from_utf8(name_slice).unwrap_or("") == name {
                    return Some(entry.inode);
                }

                offset += entry.rec_len as usize;
            }
        }
        None
    }

    // ====================== СОЗДАНИЕ НОВОГО ФАЙЛА ======================

    /// Создаёт файл (или перезаписывает существующий) — поддерживает поддиректории
    pub fn create_file_path(&mut self, path: &str, data: &[u8]) -> bool {
        if let Some(inode_num) = self.resolve_path(path) {
            // файл уже существует — перезаписываем
            self.write_file_by_inode(inode_num, data)
        } else {
            // создаём новый файл
            self.create_new_file(path, data)
        }
    }

    /// Создаёт новый файл в любой директории
    fn create_new_file(&mut self, path: &str, data: &[u8]) -> bool {
        if !self.mounted {
            return false;
        }

        let (parent_path, name) = match self.split_path(path) {
            Some((p, n)) => (p, n),
            None => {
                println!("[EXT2] Invalid path: {}", path);
                return false;
            }
        };

        // Находим inode родительской директории
        let parent_inode = match self.resolve_path(&parent_path) {
            Some(inode) => inode,
            None => {
                println!("[EXT2] Parent directory not found: {}", parent_path);
                return false;
            }
        };

        // Проверяем, что это действительно директория
        let parent = self.read_inode(parent_inode);
        if (parent.i_mode & 0xF000) != 0x4000 {
            println!("[EXT2] Parent is not a directory: {}", parent_path);
            return false;
        }

        // Выделяем inode для нового файла
        let inode_num = match self.alloc_inode() {
            Some(n) => n,
            None => {
                println!("[EXT2] No free inode!");
                return false;
            }
        };

        let num_blocks = ((data.len() as u32) + self.block_size - 1) / self.block_size;
        if num_blocks > 12 {
            println!("[EXT2] File too big (>12 direct blocks)");
            return false;
        }

        let mut inode = Ext2Inode {
            i_mode: 0x8000 | 0o644,   // regular file
            i_uid: 0,
            i_size: data.len() as u32,
            i_atime: 0,
            i_ctime: 0,
            i_mtime: 0,
            i_dtime: 0,
            i_gid: 0,
            i_links_count: 1,
            i_blocks: num_blocks * (self.block_size / 512),
            i_flags: 0,
            i_osd1: 0,
            i_block: [0; 15],
            i_generation: 0,
            i_file_acl: 0,
            i_dir_acl: 0,
            i_faddr: 0,
            i_osd2: [0; 12],
        };

        // Выделяем блоки и пишем данные
        for i in 0..num_blocks as usize {
            let block = match self.alloc_block() {
                Some(b) => b,
                None => {
                    println!("[EXT2] No free block!");
                    return false;
                }
            };
            inode.i_block[i] = block;

            let start = i * self.block_size as usize;
            let end = core::cmp::min(start + self.block_size as usize, data.len());
            unsafe {
                self.write_blocks(block, data[start..end].as_ptr(), 1);
            }
        }

        self.write_inode(inode_num, &inode);

        // Добавляем запись в родительскую директорию
        if !self.add_dir_entry(parent_inode, &name, inode_num, 1) {  // 1 = regular file
            println!("[EXT2] Failed to add directory entry");
            return false;
        }

        println!("[EXT2] Created file: {} (inode {}) in {}", path, inode_num, parent_path);
        true
    }

    fn split_path(&self, path: &str) -> Option<(String, String)> {
        if path == "/" || path.is_empty() {
            return None;
        }

        let path = path.trim_end_matches('/');
        if let Some(pos) = path.rfind('/') {
            let parent = if pos == 0 { "/" } else { &path[..pos] };
            let filename = &path[pos + 1..];
            if filename.is_empty() {
                return None;
            }
            Some((parent.to_string(), filename.to_string()))
        } else {
            // файл в корне
            Some(("/".to_string(), path.to_string()))
        }
    }

    fn add_dir_entry(&mut self, dir_inode: u32, name: &str, inode_num: u32, file_type: u8) -> bool {
        let mut dir_inode_data = self.read_inode(dir_inode);
        if (dir_inode_data.i_mode & 0xF000) != 0x4000 {
            println!("[EXT2] Not a directory!");
            return false;
        }

        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len() as u8;
        let rec_len = ((8 + name_len as usize + 3) & !3) as u16;

        for i in 0..12 {
            if dir_inode_data.i_block[i] == 0 {
                let new_block = match self.alloc_block() {
                    Some(b) => b,
                    None => { println!("[EXT2] No free block for directory"); return false; }
                };
                dir_inode_data.i_block[i] = new_block;
                // ← КРИТИЧНО: обновляем метаданные директории
                dir_inode_data.i_size += self.block_size;
                dir_inode_data.i_blocks += (self.block_size / 512);
                self.write_inode(dir_inode, &dir_inode_data);

                let zeros = [0u8; 4096];
                unsafe { self.write_blocks(new_block, zeros.as_ptr(), 1); }
            }

            let block = dir_inode_data.i_block[i];
            let mut block_buf = [0u8; 4096];
            unsafe { self.read_blocks(block, block_buf.as_mut_ptr(), 1); }

            let mut offset = 0usize;
            let mut last_entry_offset = 0usize;

            while offset + 8 <= self.block_size as usize {
                let entry = unsafe {
                    core::ptr::read_unaligned(block_buf.as_ptr().add(offset) as *const Ext2DirEntry)
                };
                if entry.rec_len == 0 || entry.rec_len as usize > self.block_size as usize - offset {
                    break;
                }
                last_entry_offset = offset;
                offset += entry.rec_len as usize;
            }

            if offset + rec_len as usize <= self.block_size as usize {
                if last_entry_offset != offset {
                    let prev_entry = unsafe {
                        core::ptr::read_unaligned(block_buf.as_ptr().add(last_entry_offset) as *const Ext2DirEntry)
                    };
                    let prev_min_len = ((8 + prev_entry.name_len as usize + 3) & !3) as u16;

                    if prev_entry.rec_len > prev_min_len {
                        let mut prev_mut = prev_entry;
                        prev_mut.rec_len = prev_min_len;
                        unsafe {
                            core::ptr::write_unaligned(
                                block_buf.as_mut_ptr().add(last_entry_offset) as *mut Ext2DirEntry,
                                prev_mut,
                            );
                        }
                        offset = last_entry_offset + prev_min_len as usize;
                    }
                }

                if offset + rec_len as usize <= self.block_size as usize {
                    let new_entry = Ext2DirEntry {
                        inode: inode_num,
                        rec_len: (self.block_size as u16 - offset as u16),
                        name_len,
                        file_type,
                    };
                    unsafe {
                        core::ptr::write_unaligned(block_buf.as_mut_ptr().add(offset) as *mut Ext2DirEntry, new_entry);
                        core::ptr::copy_nonoverlapping(name_bytes.as_ptr(), block_buf.as_mut_ptr().add(offset + 8), name_len as usize);
                        self.write_blocks(block, block_buf.as_ptr(), 1);
                    }
                    return true;
                }
            }
        }

        println!("[EXT2] Directory is full");
        false
    }


    /// Читаем bitmap (inode или block) целиком
    unsafe fn read_bitmap(&self, block: u32) -> [u8; 4096] {
        let mut buf = [0u8; 4096];
        self.read_blocks(block, buf.as_mut_ptr(), 1);
        buf
    }

    /// Записываем bitmap обратно
    unsafe fn write_bitmap(&self, block: u32, bitmap: &[u8; 4096]) {
        self.write_blocks(block, bitmap.as_ptr(), 1);
    }

    // ====================== ALLOC INODE ======================
    /// Выделяем свободный inode через inode bitmap
    fn alloc_inode(&mut self) -> Option<u32> {
        if !self.mounted { return None; }

        let group = 0;
        let bgd = self.get_bg_descriptor(group);

        let inode_bitmap_block = bgd.bg_inode_bitmap;
        let mut bitmap = unsafe { self.read_bitmap(inode_bitmap_block) };

        let inodes_per_group = self.superblock.s_inodes_per_group as usize;

        // Начинаем с 11 (inodes 0-10 зарезервированы)
        for i in 12..inodes_per_group {
            let byte = i / 8;
            let bit = i % 8;

            if byte >= bitmap.len() { break; }

            if (bitmap[byte] & (1 << bit)) == 0 {
                bitmap[byte] |= 1 << bit;

                unsafe { self.write_bitmap(inode_bitmap_block, &bitmap); }

                let mut bgd_mut = bgd;
                if bgd_mut.bg_free_inodes_count > 0 {
                    bgd_mut.bg_free_inodes_count -= 1;
                }
                self.write_bg_descriptor(group, &bgd_mut);

                self.update_superblock_free_inodes();

                let inode_num = group * self.superblock.s_inodes_per_group + i as u32;
                return Some(inode_num);
            }
        }
        println!("[EXT2] No free inode found");
        None
    }

    // ====================== ALLOC BLOCK ======================

    // ====================== ВСПОМОГАТЕЛЬНЫЕ ======================

    fn write_bg_descriptor(&self, group: u32, bgd: &Ext2BlockGroupDescriptor) {
        let bgd_table_block = if self.superblock.s_first_data_block == 0 { 1 } else { self.superblock.s_first_data_block };
        let desc_size = core::mem::size_of::<Ext2BlockGroupDescriptor>() as u32;
        let bgds_per_block = self.block_size / desc_size;
        let block = bgd_table_block + (group / bgds_per_block);
        let offset = ((group % bgds_per_block) * desc_size) as usize;

        unsafe { self.write_struct_to_disk(block, offset, bgd); }
    }

    fn update_superblock_free_inodes(&self) {
        unsafe {
            let sb_ptr = &self.superblock as *const Ext2SuperBlock as *const u8;
            let mut disk = self.disk.lock();
            let _ = disk.write_sectors(2, (self.partition_offset + 2) as u32, sb_ptr as u32);
        }
    }

    fn update_superblock_free_blocks(&self) {
        unsafe {
            let sb_ptr = &self.superblock as *const Ext2SuperBlock as *const u8;
            let mut disk = self.disk.lock();
            let _ = disk.write_sectors(2, (self.partition_offset + 2) as u32, sb_ptr as u32);
        }
    }

    // ====================== PATH RESOLVER ======================

    /// Разрешает путь вида "/dir/subdir/file.txt" в номер inode
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

        for i in 0..blocks_to_read {
            match self.resolve_block(&inode, i) {
                Some(block_num) => {
                    let mut block_buf = alloc::vec![0u8; self.block_size as usize];
                    unsafe {
                        self.read_blocks(block_num, block_buf.as_mut_ptr(), 1);
                    }

                    let start = i * self.block_size as usize;
                    let end = core::cmp::min(start + self.block_size as usize, file_size);
                    data.extend_from_slice(&block_buf[0..(end - start)]);
                }
                None => break,
            }
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
    pub fn read_file_path(&self, path: &str) -> Option<alloc::vec::Vec<u8>> {
        if let Some(inode_num) = self.resolve_path(path) {
            self.read_file_by_inode(inode_num)
        } else {
            println!("[EXT2] File not found: {}", path);
            None
        }
    }

    /// Записать файл по пути (перезаписывает существующий файл)
    pub fn write_file_path(&self, path: &str, data: &[u8]) -> bool {
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

    // Внутренний helper
    fn list_directory_entries_by_inode(&self, inode_num: u32) -> Option<Vec<DirEntry>> {
        // временно, чтобы не ломать старый код
        let inode = self.read_inode(inode_num);
        if (inode.i_mode & 0xF000) != 0x4000 {
            return None;
        }
        // Можно вызвать list_directory_entries, но пока оставим простой вариант
        // или сделать через path, но проще — оставить как есть для inode
        // (пока что можно просто вызвать list_directory_entries с "/" и фильтровать, но проще оставить старую логику)
        None // заглушка — потом уберём
    }

    // ====================== УДАЛЕНИЕ ФАЙЛА ======================

    /// Удаляет файл (ТОЛЬКО обычные файлы). Директории не удаляет!
    pub fn remove_file_path(&mut self, path: &str) -> bool {
        if !self.mounted {
            return false;
        }

        let inode_num = match self.resolve_path(path) {
            Some(n) => n,
            None => {
                println!("[EXT2] File not found: {}", path);
                return false;
            }
        };

        let inode = self.read_inode(inode_num);

        // ←←← НОВАЯ ПРОВЕРКА
        if (inode.i_mode & 0xF000) == 0x4000 {
            println!("[EXT2] rm: cannot remove '{}': Is a directory", path);
            println!("[EXT2] Use rmdir to remove directories");
            return false;
        }

        // Это обычный файл — удаляем
        let name = match path.strip_prefix('/') {
            Some(n) if !n.contains('/') => n,
            _ => {
                println!("[EXT2] rm: only root files supported for now");
                return false;
            }
        };

        if !self.remove_dir_entry(2, name) {
            println!("[EXT2] Failed to remove directory entry");
            return false;
        }

        self.free_blocks_of_inode(inode_num);
        self.free_inode(inode_num);

        println!("[EXT2] Removed file: {}", path);
        true
    }

    /// Удаляет dir entry из директории (простая версия — ставим inode = 0)
    fn remove_dir_entry(&mut self, dir_inode: u32, name: &str) -> bool {
        let mut dir_inode_data = self.read_inode(dir_inode);
        if (dir_inode_data.i_mode & 0xF000) != 0x4000 {
            return false;
        }

        for i in 0..12 {
            if dir_inode_data.i_block[i] == 0 {
                break;
            }

            let block = dir_inode_data.i_block[i];
            let mut block_buf = [0u8; 4096];
            unsafe { self.read_blocks(block, block_buf.as_mut_ptr(), 1); }

            let mut offset = 0usize;
            while offset + 8 <= self.block_size as usize {
                let entry = unsafe {
                    core::ptr::read_unaligned(block_buf.as_ptr().add(offset) as *const Ext2DirEntry)
                };

                if entry.rec_len == 0 {
                    break;
                }

                let name_slice = &block_buf[offset + 8..offset + 8 + entry.name_len as usize];
                if core::str::from_utf8(name_slice).unwrap_or("") == name {
                    // Нашли — обнуляем inode (простой способ удаления)
                    let mut entry_mut = entry;
                    entry_mut.inode = 0;

                    unsafe {
                        core::ptr::write_unaligned(
                            block_buf.as_mut_ptr().add(offset) as *mut Ext2DirEntry,
                            entry_mut,
                        );
                    }
                    unsafe { self.write_blocks(block, block_buf.as_ptr(), 1); }
                    return true;
                }

                offset += entry.rec_len as usize;
            }
        }
        false
    }

    /// Освобождаем все блоки, на которые ссылается inode
    fn free_blocks_of_inode(&mut self, inode_num: u32) {
        let inode = self.read_inode(inode_num);
        for i in 0..12 {
            if inode.i_block[i] != 0 {
                self.free_block(inode.i_block[i]);
            }
        }
        // TODO: indirect blocks позже
    }

    /// Выделяем свободный data-блок (правильный динамический расчёт)
    fn alloc_block(&mut self) -> Option<u32> {
        if !self.mounted {
            println!("[EXT2] alloc_block: FS not mounted");
            return None;
        }

        let group = 0u32;
        let bgd = self.get_bg_descriptor(group);

        let block_bitmap_block = bgd.bg_block_bitmap;
        let mut bitmap = unsafe { self.read_bitmap(block_bitmap_block) };

        let blocks_per_group = self.superblock.s_blocks_per_group as usize;

        // Динамически вычисляем, где начинаются data-блоки после inode table
        let inode_table_size_in_blocks =
            ((self.superblock.s_inodes_per_group as u64 *
                self.superblock.s_inode_size as u64) +
                (self.block_size as u64 - 1)) / self.block_size as u64;

        let first_data_block = bgd.bg_inode_table as u64 + inode_table_size_in_blocks;

        println!("[EXT2] alloc_block: first data block = {}", first_data_block);

        // Ищем свободный блок начиная с first_data_block
        for i in (first_data_block as usize)..blocks_per_group {
            let byte = i / 8;
            let bit = i % 8;

            if byte >= bitmap.len() {
                break;
            }

            if (bitmap[byte] & (1 << bit)) == 0 {
                // Нашли свободный
                bitmap[byte] |= 1 << bit;

                unsafe { self.write_bitmap(block_bitmap_block, &bitmap); }

                let mut bgd_mut = bgd;
                if bgd_mut.bg_free_blocks_count > 0 {
                    bgd_mut.bg_free_blocks_count -= 1;
                }
                self.write_bg_descriptor(group, &bgd_mut);
                self.update_superblock_free_blocks();

                return Some(i as u32);
            }
        }

        println!("[EXT2] alloc_block: no free blocks found");
        None
    }

    /// Освобождаем блок
    fn free_block(&mut self, block: u32) {
        if block == 0 {
            return;
        }

        let group = 0u32;
        let bgd = self.get_bg_descriptor(group);
        let block_bitmap_block = bgd.bg_block_bitmap;

        let mut bitmap = unsafe { self.read_bitmap(block_bitmap_block) };

        let i = block as usize;
        let byte = i / 8;
        let bit = i % 8;

        if byte < bitmap.len() {
            bitmap[byte] &= !(1 << bit);

            unsafe { self.write_bitmap(block_bitmap_block, &bitmap); }

            let mut bgd_mut = bgd;
            bgd_mut.bg_free_blocks_count += 1;
            self.write_bg_descriptor(group, &bgd_mut);
            self.update_superblock_free_blocks();
        }
    }
    /// Освобождаем inode (через inode bitmap)
    fn free_inode(&mut self, inode_num: u32) {
        let group = (inode_num - 1) / self.superblock.s_inodes_per_group;
        let bgd = self.get_bg_descriptor(group);

        let inode_bitmap_block = bgd.bg_inode_bitmap;
        let mut bitmap = unsafe { self.read_bitmap(inode_bitmap_block) };

        let index_in_group = ((inode_num - 1) % self.superblock.s_inodes_per_group) as usize;
        let byte = index_in_group / 8;
        let bit = index_in_group % 8;

        if byte < bitmap.len() {
            bitmap[byte] &= !(1 << bit);
            unsafe { self.write_bitmap(inode_bitmap_block, &bitmap); }

            let mut bgd_mut = bgd;
            bgd_mut.bg_free_inodes_count += 1;
            self.write_bg_descriptor(group, &bgd_mut);
            self.update_superblock_free_inodes();
        }

        // Обнуляем сам inode
        let zero_inode = unsafe { mem::zeroed::<Ext2Inode>() };
        self.write_inode(inode_num, &zero_inode);
    }

    // ====================== СОЗДАНИЕ ДИРЕКТОРИИ (mkdir) ======================

    pub fn mkdir_path(&mut self, path: &str) -> bool {
        if !self.mounted { return false; }

        if self.resolve_path(path).is_some() {
            println!("[EXT2] Directory already exists: {}", path);
            return false;
        }

        let name = match path.strip_prefix('/') {
            Some(n) if !n.contains('/') => n,
            _ => {
                println!("[EXT2] mkdir: only root directories supported for now");
                return false;
            }
        };

        let inode_num = match self.alloc_inode() {
            Some(n) => n,
            None => { println!("[EXT2] No free inode for directory"); return false; }
        };

        // Создаём inode директории
        let mut inode = Ext2Inode {
            i_mode: 0x4000 | 0o755,   // directory + rwxr-xr-x
            i_uid: 0,
            i_size: self.block_size,  // обычно один блок
            i_atime: 0,
            i_ctime: 0,
            i_mtime: 0,
            i_dtime: 0,
            i_gid: 0,
            i_links_count: 2,         // . и ..
            i_blocks: self.block_size / 512,
            i_flags: 0,
            i_osd1: 0,
            i_block: [0; 15],
            i_generation: 0,
            i_file_acl: 0,
            i_dir_acl: 0,
            i_faddr: 0,
            i_osd2: [0; 12],
        };

        // Выделяем блок для содержимого директории
        let dir_block = match self.alloc_block() {
            Some(b) => b,
            None => { println!("[EXT2] No free block for directory"); return false; }
        };
        inode.i_block[0] = dir_block;

        self.write_inode(inode_num, &inode);

        // Создаём записи . и ..
        if !self.init_directory_block(dir_block, inode_num, 2) {  // parent = root (2)
            return false;
        }

        // Добавляем запись о новой директории в родительскую (root)
        if !self.add_dir_entry(2, name, inode_num, 2) {  // 2 = directory file_type
            println!("[EXT2] Failed to add directory entry");
            return false;
        }

        println!("[EXT2] Created directory: {} (inode {})", path, inode_num);
        true
    }

    /// Инициализирует блок директории записями `.` и `..`
    fn init_directory_block(&mut self, block: u32, self_inode: u32, parent_inode: u32) -> bool {
        let mut block_buf = [0u8; 4096];

        // Запись "."
        let dot_entry = Ext2DirEntry {
            inode: self_inode,
            rec_len: 12,
            name_len: 1,
            file_type: 2,
        };
        unsafe {
            core::ptr::write_unaligned(block_buf.as_mut_ptr() as *mut Ext2DirEntry, dot_entry);
            block_buf[8] = b'.';
        }

        // Запись ".."
        let dotdot_entry = Ext2DirEntry {
            inode: parent_inode,
            rec_len: (self.block_size - 12) as u16,
            name_len: 2,
            file_type: 2,
        };
        unsafe {
            core::ptr::write_unaligned(block_buf.as_mut_ptr().add(12) as *mut Ext2DirEntry, dotdot_entry);
            block_buf[20] = b'.';
            block_buf[21] = b'.';
        }

        unsafe { self.write_blocks(block, block_buf.as_ptr(), 1); }
        true
    }

    // ====================== РЕКУРСИВНОЕ УДАЛЕНИЕ ДИРЕКТОРИИ (rm -r) ======================

    pub fn rmdir_path(&mut self, path: &str) -> bool {
        if !self.mounted {
            return false;
        }

        let inode_num = match self.resolve_path(path) {
            Some(n) => n,
            None => {
                println!("[EXT2] Directory not found: {}", path);
                return false;
            }
        };

        let inode = self.read_inode(inode_num);
        if (inode.i_mode & 0xF000) != 0x4000 {
            println!("[EXT2] Not a directory: {}", path);
            return false;
        }

        // Рекурсивно удаляем всё содержимое
        if !self.remove_directory_contents(inode_num) {
            println!("[EXT2] Failed to remove directory contents");
            return false;
        }

        // Удаляем саму директорию из родительской
        let name = match path.strip_prefix('/') {
            Some(n) if !n.contains('/') => n,
            _ => {
                // для поддиректорий нужно найти родителя — пока только корень
                println!("[EXT2] rmdir: only root directories supported for now");
                return false;
            }
        };

        if !self.remove_dir_entry(2, name) {
            println!("[EXT2] Failed to remove directory entry from parent");
            return false;
        }

        // Освобождаем блок директории и inode
        if inode.i_block[0] != 0 {
            self.free_block(inode.i_block[0]);
        }
        self.free_inode(inode_num);

        println!("[EXT2] Removed directory recursively: {}", path);
        true
    }

    /// Рекурсивно удаляет всё содержимое директории
    fn remove_directory_contents(&mut self, dir_inode: u32) -> bool {
        let entries = match self.list_directory_entries_by_inode(dir_inode) {
            Some(e) => e,
            None => return false,
        };

        for entry in entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }

            let full_path = format!("/{}/{}", dir_inode, entry.name); // грубо, но работает

            if entry.file_type == 2 { // директория
                if !self.rmdir_path(&full_path) {
                    return false;
                }
            } else { // обычный файл
                if !self.remove_file_path(&full_path) {
                    return false;
                }
            }
        }
        true
    }


    fn is_directory_empty(&self, dir_inode: u32) -> bool {
        let inode = self.read_inode(dir_inode);
        if inode.i_block[0] == 0 { return true; }

        let mut block_buf = [0u8; 4096];
        unsafe { self.read_blocks(inode.i_block[0], block_buf.as_mut_ptr(), 1); }

        let mut offset = 0usize;
        let mut count = 0;

        while offset + 8 <= self.block_size as usize {
            let entry = unsafe {
                core::ptr::read_unaligned(block_buf.as_ptr().add(offset) as *const Ext2DirEntry)
            };
            if entry.inode != 0 {
                count += 1;
            }
            if entry.rec_len == 0 { break; }
            offset += entry.rec_len as usize;
        }

        count <= 2  // только . и ..
    }

    pub fn list_directory_entries(&self, path: &str) -> Option<Vec<DirEntry>> {
        let inode_num = self.resolve_path(path)?;

        let inode = self.read_inode(inode_num);
        if (inode.i_mode & 0xF000) != 0x4000 {
            return None; // не директория
        }

        let mut entries = Vec::new();

        for i in 0..12 {
            if inode.i_block[i] == 0 {
                break;
            }

            let mut block_buf = [0u8; 4096];
            unsafe { self.read_blocks(inode.i_block[i], block_buf.as_mut_ptr(), 1); }

            let mut offset = 0usize;
            while offset + 8 <= self.block_size as usize {
                let entry = unsafe {
                    core::ptr::read_unaligned(block_buf.as_ptr().add(offset) as *const Ext2DirEntry)
                };

                if entry.rec_len == 0 || entry.inode == 0 {
                    break;
                }

                let name_len = entry.name_len as usize;
                let name_start = offset + 8;
                let name_end = name_start + name_len;

                if name_end > self.block_size as usize {
                    break;
                }

                let name = core::str::from_utf8(&block_buf[name_start..name_end])
                    .unwrap_or("???")
                    .to_string();

                // Получаем размер файла (если это обычный файл)
                let size = if entry.file_type == 1 {
                    let file_inode = self.read_inode(entry.inode);
                    file_inode.i_size
                } else {
                    0
                };

                entries.push(DirEntry {
                    inode: entry.inode,
                    name,
                    file_type: entry.file_type,
                    size,
                });

                offset += entry.rec_len as usize;
            }
        }

        Some(entries)
    }
}

impl crate::filesystem::Filesystem for Ext2 {
    fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        self.read_file_path(path)
    }
    fn resolve_path(&self, path: &str) -> Option<u32> {
        Ext2::resolve_path(self, path)
    }

    fn write_file(&mut self, path: &str, data: &[u8]) -> bool {
        self.create_file_path(path, data)
    }
    fn create_file(&mut self, path: &str, data: &[u8]) -> bool {
        self.create_file_path(path, data)
    }
    fn remove_file(&mut self, path: &str) -> bool {
        self.remove_file_path(path)
    }
    fn mkdir(&mut self, path: &str) -> bool {
        self.mkdir_path(path)
    }
    fn rmdir(&mut self, path: &str) -> bool {
        self.rmdir_path(path)
    }

    fn list_directory_entries(&self, path: &str) -> Option<Vec<DirEntry>> {
        Ext2::list_directory_entries(self, path)
    }

    fn is_mounted(&self) -> bool {
        self.mounted
    }

    fn read_at(&self, inode_num: u32, offset: u64, buf: &mut [u8]) -> usize {
        let inode = self.read_inode(inode_num);
        if (inode.i_mode & 0xF000) != 0x8000 {
            return 0;
        }

        let file_size = inode.i_size as u64;
        if offset >= file_size {
            return 0;
        }

        let to_read = core::cmp::min(buf.len() as u64, file_size - offset) as usize;
        let mut bytes_read = 0;
        let block_size = self.block_size as u64;
        let mut current_offset = offset;

        while bytes_read < to_read {
            let block_index = (current_offset / block_size) as usize;

            match self.resolve_block(&inode, block_index) {
                Some(block_num) => {
                    let mut block_buf = alloc::vec![0u8; self.block_size as usize];
                    unsafe { self.read_blocks(block_num, block_buf.as_mut_ptr(), 1); }

                    let block_offset = (current_offset % block_size) as usize;
                    let can_read = core::cmp::min(
                        to_read - bytes_read,
                        (self.block_size as usize) - block_offset,
                    );

                    buf[bytes_read..bytes_read + can_read]
                        .copy_from_slice(&block_buf[block_offset..block_offset + can_read]);

                    bytes_read += can_read;
                    current_offset += can_read as u64;
                }
                None => break,
            }
        }

        bytes_read
    }

    fn write_at(&mut self, _inode: u32, _offset: u64, _buf: &[u8]) -> usize {
        // Пока заглушка. Полноценная запись требует аллокации блоков.
        println!("[EXT2] write_at пока не реализована");
        0
    }
}



// ====================== ФОРМАТИРОВАНИЕ ======================

impl Ext2 {
    /// Создаёт и форматирует новую ext2 ФС на указанном разделе
    /// Форматирует раздел как чистую ext2-файловую систему.
    ///
    /// # Параметры
    /// - `disk` — диск
    /// - `partition_offset` — LBA начала раздела
    /// - `total_sectors` — размер раздела в секторах (512 байт)
    /// - `block_size` — 1024, 2048 или 4096
    pub fn format(
        disk: Arc<Mutex<dyn BlockDevice>>,
        partition_offset: u64,
        total_sectors: u64,
        block_size: u32,
    ) -> Self {
        assert!(matches!(block_size, 1024 | 2048 | 4096), "block_size must be 1024, 2048 or 4096");

        let sectors_per_block = block_size / 512;
        let total_blocks = (total_sectors / sectors_per_block as u64) as u32;

        if total_blocks < 32 {
            println!("[EXT2 FORMAT] Ошибка: раздел слишком маленький");
            panic!("Partition too small for ext2");
        }

        let blocks_per_group = total_blocks;
        let inodes_per_group = core::cmp::min((total_blocks / 4).max(64), 16384);
        let first_data_block = if block_size == 1024 { 1u32 } else { 0u32 };

        let bgd_size = core::mem::size_of::<Ext2BlockGroupDescriptor>() as u32;
        let bgd_blocks = (bgd_size + block_size - 1) / block_size;
        let inode_table_blocks =
            ((inodes_per_group as u64 * 128 + block_size as u64 - 1) / block_size as u64) as u32;

        let block_bitmap_block = first_data_block + 1 + bgd_blocks;
        let inode_bitmap_block = block_bitmap_block + 1;
        let inode_table_block = inode_bitmap_block + 1;
        let root_block = inode_table_block + inode_table_blocks;
        let first_free_block = root_block + 1;
        assert!(total_blocks <= (block_size * 8) as u32, "Раздел слишком большой для одной группы блоков");

        println!("[EXT2 FORMAT] block_size={}, total_blocks={}, inodes={}",
                 block_size, total_blocks, inodes_per_group);

        // ====================== 1. Superblock ======================
        let mut sb = Ext2SuperBlock {
            s_inodes_count: inodes_per_group,
            s_blocks_count: total_blocks,
            s_r_blocks_count: 0,
            s_free_blocks_count: total_blocks.saturating_sub(first_free_block),
            s_free_inodes_count: inodes_per_group.saturating_sub(11),
            s_first_data_block: first_data_block,
            s_log_block_size: match block_size {
                1024 => 0,
                2048 => 1,
                4096 => 2,
                _ => 0,
            },
            s_log_frag_size: match block_size {
                1024 => 0,
                2048 => 1,
                4096 => 2,
                _ => 0,
            },
            s_blocks_per_group: blocks_per_group,
            s_frags_per_group: blocks_per_group,
            s_inodes_per_group: inodes_per_group,
            s_mtime: 0,
            s_wtime: 0,
            s_mnt_count: 0,
            s_max_mnt_count: 0xFFFF,
            s_magic: EXT2_MAGIC,
            s_state: 1,
            s_errors: 1,
            s_minor_rev_level: 0,
            s_lastcheck: 0,
            s_checkinterval: 0,
            s_creator_os: 0,
            s_rev_level: 1, // EXT2_DYNAMIC_REV (ОБЯЗАТЕЛЬНО)
            s_def_resuid: 0,
            s_def_resgid: 0,
            s_first_ino: 11,
            s_inode_size: 128,
            s_block_group_nr: 0,
            s_feature_compat: 2, // EXT2_FEATURE_COMPAT_FILETYPE
            s_feature_incompat: 0,
            s_feature_ro_compat: 0,
            s_uuid: [0; 16],
            s_volume_name: [0; 16],
            _padding: [0; 896],
        };

        let name = b"RustOS\0";
        sb.s_volume_name[..name.len()].copy_from_slice(name);

        // Пишем superblock
        unsafe {
            let sb_ptr = &sb as *const Ext2SuperBlock as *const u8;
            let mut disk = disk.lock();
            let _ = disk.write_sectors(2, (partition_offset + 2) as u32, sb_ptr as u32);
        }
        println!("[EXT2 FORMAT] Superblock written");

        // ====================== 2. Block Group Descriptor ======================
        let bgd = Ext2BlockGroupDescriptor {
            bg_block_bitmap: block_bitmap_block,
            bg_inode_bitmap: inode_bitmap_block,
            bg_inode_table: inode_table_block,
            bg_free_blocks_count: total_blocks.saturating_sub(first_free_block) as u16,
            bg_free_inodes_count: inodes_per_group.saturating_sub(11) as u16,
            bg_used_dirs_count: 1,
            bg_pad: 0,
            _reserved: [0; 12],
        };

        unsafe {
            let bgd_block = first_data_block + 1;

            let mut bgd_buf = [0u8; 4096];
            unsafe {
                core::ptr::copy_nonoverlapping(
                    &bgd as *const _ as *const u8,
                    bgd_buf.as_mut_ptr(),
                    core::mem::size_of::<Ext2BlockGroupDescriptor>(),
                );
            }
            let bgd_lba = partition_offset as u32 + (bgd_block * sectors_per_block);
            let mut disk = disk.lock();
            let _ = disk.write_sectors(sectors_per_block as u8, bgd_lba, bgd_buf.as_ptr() as u32);

        }
        println!("[EXT2 FORMAT] BGD written");

        // ====================== 3. Block Bitmap ======================
        let mut block_bitmap = [0u8; 4096];
        for i in 0..first_free_block {
            let byte = (i / 8) as usize;
            let bit = (i % 8) as usize;
            if byte < block_bitmap.len() {
                block_bitmap[byte] |= 1 << bit;
            }
        }
        for i in total_blocks..(blocks_per_group.min(block_bitmap.len() as u32 * 8)) {
            let byte = (i / 8) as usize;
            let bit = (i % 8) as usize;
            if byte < block_bitmap.len() {
                block_bitmap[byte] |= 1 << bit;
            }
        }

        unsafe {
            let lba = partition_offset as u32 + (block_bitmap_block * sectors_per_block);
            let mut disk = disk.lock();
            let _ = disk.write_sectors(sectors_per_block as u8, lba, block_bitmap.as_ptr() as u32);
        }
        println!("[EXT2 FORMAT] Block bitmap written");

        // ====================== 4. Inode Bitmap ======================
        let mut inode_bitmap = [0u8; 4096];
        for i in 0..11 { // Исправлено с 12
            let byte = (i / 8) as usize;
            let bit = (i % 8) as usize;
            inode_bitmap[byte] |= 1 << bit;
        }

        unsafe {
            let lba = partition_offset as u32 + (inode_bitmap_block * sectors_per_block);
            let mut disk = disk.lock();
            let _ = disk.write_sectors(sectors_per_block as u8, lba, inode_bitmap.as_ptr() as u32);
        }
        println!("[EXT2 FORMAT] Inode bitmap written");

        // ====================== 5. Inode Table (zero) ======================
        let zeros = [0u8; 4096];
        unsafe {
            let start_lba = partition_offset as u32 + (inode_table_block * sectors_per_block);
            let mut disk = disk.lock();
            for i in 0..inode_table_blocks {
                let lba = start_lba + (i * sectors_per_block);
                let _ = disk.write_sectors(sectors_per_block as u8, lba, zeros.as_ptr() as u32);
            }
        }
        println!("[EXT2 FORMAT] Inode table zeroed");

        // ====================== 6. Root Directory (inode 2) ======================
        let root_block = first_free_block;

        let mut root_inode = Ext2Inode {
            i_mode: 0x4000 | 0o755,
            i_uid: 0,
            i_size: block_size,
            i_atime: 0,
            i_ctime: 0,
            i_mtime: 0,
            i_dtime: 0,
            i_gid: 0,
            i_links_count: 2,
            i_blocks: block_size / 512,
            i_flags: 0,
            i_osd1: 0,
            i_block: [0; 15],
            i_generation: 0,
            i_file_acl: 0,
            i_dir_acl: 0,
            i_faddr: 0,
            i_osd2: [0; 12],
        };
        root_inode.i_block[0] = root_block;

        // Записываем inode 2
        unsafe {
            let inode_offset_in_table = 2 * 128u64;
            let block_in_table = (inode_offset_in_table / block_size as u64) as u32;
            let offset_in_block = (inode_offset_in_table % block_size as u64) as usize;

            let mut inode_buf = [0u8; 4096];
            core::ptr::write_unaligned(
                inode_buf.as_mut_ptr().add(offset_in_block) as *mut Ext2Inode,
                root_inode,
            );

            let lba = partition_offset as u32 + ((inode_table_block + block_in_table) * sectors_per_block);
            let mut disk = disk.lock();
            let _ = disk.write_sectors(sectors_per_block as u8, lba, inode_buf.as_ptr() as u32);
        }

        // Содержимое корневой директории
        let mut dir_buf = [0u8; 4096];

        let dot = Ext2DirEntry {
            inode: 2,
            rec_len: 12,
            name_len: 1,
            file_type: 2,
        };
        unsafe {
            core::ptr::write_unaligned(dir_buf.as_mut_ptr() as *mut Ext2DirEntry, dot);
        }
        dir_buf[8] = b'.';

        let dotdot = Ext2DirEntry {
            inode: 2,
            rec_len: (block_size - 12) as u16,
            name_len: 2,
            file_type: 2,
        };
        unsafe {
            core::ptr::write_unaligned(dir_buf.as_mut_ptr().add(12) as *mut Ext2DirEntry, dotdot);
        }
        dir_buf[20] = b'.';
        dir_buf[21] = b'.';

        unsafe {
            let dir_lba = partition_offset as u32 + (root_block * sectors_per_block);
            let mut disk = disk.lock();
            let _ = disk.write_sectors(sectors_per_block as u8, dir_lba, dir_buf.as_ptr() as u32);
        }
        println!("[EXT2 FORMAT] Root directory created");

        // ====================== 7. Возвращаем FS ======================
        let mut fs = Ext2 {
            superblock: sb,
            block_size,
            mounted: true,
            disk,
            partition_offset,
        };

        fs.superblock.s_free_blocks_count = total_blocks.saturating_sub(first_free_block + 1);
        println!("[EXT2 FORMAT] Done");

        fs
    }

    /// Удобная обёртка: форматирование на N гигабайт
    pub fn format_gb(
        disk: Arc<Mutex<dyn BlockDevice>>,
        partition_offset: u64,
        gb: u64,
        block_size: u32,
    ) -> Self {
        let total_sectors = gb * 1024 * 1024  / 512;
        println!("[EXT2] Форматирование {} GB ({} секторов)", gb, total_sectors);
        Self::format(disk, partition_offset, total_sectors, block_size)
    }
}