use alloc::vec::Vec;
// fs/ext2.rs
use core::mem;
use crate::drivers::disk::{Disk};
use crate::{print, println};

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

    /// Записываем блоки ext2
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
        if !self.mounted {
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
        if !self.mounted {
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

    /// Создаёт новый файл (или перезаписывает существующий)
    pub fn create_file_path(&mut self, path: &str, data: &[u8]) -> bool {
        if let Some(inode_num) = self.resolve_path(path) {
            // файл уже есть — просто пишем
            self.write_file_by_inode(inode_num, data)
        } else {
            // файла нет — создаём новый
            self.create_new_file(path, data)
        }
    }

    /// Создаёт новый файл (пока только в корне!)
    fn create_new_file(&mut self, path: &str, data: &[u8]) -> bool {
        if !self.mounted {
            return false;
        }

        // Поддерживаем только файлы в корне на данном этапе
        let name = match path.strip_prefix('/') {
            Some(n) if !n.contains('/') => n,
            _ => {
                println!("[EXT2] create_file: only files in root supported for now (/filename)");
                return false;
            }
        };

        if name.is_empty() {
            return false;
        }

        // 1. Ищем свободный inode (очень простой алгоритм — начинаем с 11)
        let inode_num = self.alloc_inode();
        if inode_num.is_none() {
            println!("[EXT2] No free inode!");
            return false;
        }
        let inode_num = inode_num.unwrap();

        // 2. Создаём inode
        let mut inode = Ext2Inode {
            i_mode: 0x8000 | 0o644,     // regular file + rw-r--r--
            i_uid: 0,
            i_size: data.len() as u32,
            i_atime: 0,
            i_ctime: 0,
            i_mtime: 0,
            i_dtime: 0,
            i_gid: 0,
            i_links_count: 1,
            i_blocks: ((data.len() as u32 + self.block_size - 1) / self.block_size) * (self.block_size / 512),
            i_flags: 0,
            i_osd1: 0,
            i_block: [0; 15],
            i_generation: 0,
            i_file_acl: 0,
            i_dir_acl: 0,
            i_faddr: 0,
            i_osd2: [0; 12],
        };

        // 3. Выделяем блок(и) и пишем данные (только direct блоки)
        let num_blocks = ((data.len() as u32) + self.block_size - 1) / self.block_size;
        if num_blocks > 12 {
            println!("[EXT2] File too big for simple implementation");
            return false;
        }

        for i in 0..num_blocks as usize {
            // TODO: нормальное выделение блоков через bitmap
            // Пока берём "следующий" блок после inode table (очень грубо)
            let block = 100 + i as u32; // грубый хак, потом заменим
            inode.i_block[i] = block;

            let start = i * self.block_size as usize;
            let end = core::cmp::min(start + self.block_size as usize, data.len());
            unsafe {
                self.write_blocks(block, data[start..end].as_ptr(), 1);
            }
        }

        // 4. Записываем inode
        self.write_inode(inode_num, &inode);

        // 5. Добавляем запись в корневую директорию
        if !self.add_dir_entry(2, name, inode_num, 1) {  // 1 = regular file
            println!("[EXT2] Failed to add directory entry");
            return false;
        }

        println!("[EXT2] Created new file: {} (inode {})", path, inode_num);
        true
    }

    fn add_dir_entry(&mut self, dir_inode: u32, name: &str, inode_num: u32, file_type: u8) -> bool {
        let mut dir_inode_data = self.read_inode(dir_inode);
        if (dir_inode_data.i_mode & 0xF000) != 0x4000 {
            println!("[EXT2] Not a directory!");
            return false;
        }

        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len() as u8;
        let rec_len = ((8 + name_len + 3) & !3) as u16; // выравнивание 4 байта

        // Проходим по всем 12 direct блокам директории
        for i in 0..12 {
            if dir_inode_data.i_block[i] == 0 {
                // Нужно выделить новый блок для директории
                let new_block = match self.alloc_block() {
                    Some(b) => b,
                    None => {
                        println!("[EXT2] No free block for directory");
                        return false;
                    }
                };
                dir_inode_data.i_block[i] = new_block;
                self.write_inode(dir_inode, &dir_inode_data);
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
                offset += entry.rec_len as usize;
            }

            // Если в этот блок влезает новая запись
            if offset + rec_len as usize <= self.block_size as usize {
                let new_entry = Ext2DirEntry {
                    inode: inode_num,
                    rec_len,
                    name_len,
                    file_type,
                };

                unsafe {
                    core::ptr::write_unaligned(
                        block_buf.as_mut_ptr().add(offset) as *mut Ext2DirEntry,
                        new_entry,
                    );
                    core::ptr::copy_nonoverlapping(
                        name_bytes.as_ptr(),
                        block_buf.as_mut_ptr().add(offset + 8),
                        name_len as usize,
                    );
                }

                unsafe { self.write_blocks(block, block_buf.as_ptr(), 1); }
                return true;
            }
        }

        println!("[EXT2] Directory is full (all 12 direct blocks used)");
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

        let group = 0; // пока только первая группа (достаточно для начала)
        let bgd = self.get_bg_descriptor(group);

        let inode_bitmap_block = bgd.bg_inode_bitmap;
        let mut bitmap = unsafe { self.read_bitmap(inode_bitmap_block) };

        let inodes_per_group = self.superblock.s_inodes_per_group as usize;

        for i in 1..inodes_per_group {  // пропускаем inode 0
            let byte = i / 8;
            let bit = i % 8;

            if byte >= bitmap.len() { break; }

            if (bitmap[byte] & (1 << bit)) == 0 {
                // нашли свободный
                bitmap[byte] |= 1 << bit; // помечаем как занятый

                unsafe { self.write_bitmap(inode_bitmap_block, &bitmap); }

                // обновляем счётчики
                let mut bgd_mut = bgd;
                bgd_mut.bg_free_inodes_count -= 1;
                self.write_bg_descriptor(group, &bgd_mut);

                self.update_superblock_free_inodes();

                return Some((group * self.superblock.s_inodes_per_group + i as u32) as u32);
            }
        }
        None
    }

    // ====================== ALLOC BLOCK ======================

    /// Выделяем свободный блок через block bitmap
    fn alloc_block(&mut self) -> Option<u32> {
        if !self.mounted { return None; }

        let group = 0;
        let bgd = self.get_bg_descriptor(group);

        let block_bitmap_block = bgd.bg_block_bitmap;
        let mut bitmap = unsafe { self.read_bitmap(block_bitmap_block) };

        let blocks_per_group = self.superblock.s_blocks_per_group as usize;

        for i in 0..blocks_per_group {
            let byte = i / 8;
            let bit = i % 8;

            if byte >= bitmap.len() { break; }

            if (bitmap[byte] & (1 << bit)) == 0 {
                bitmap[byte] |= 1 << bit;

                unsafe { self.write_bitmap(block_bitmap_block, &bitmap); }

                let mut bgd_mut = bgd;
                bgd_mut.bg_free_blocks_count -= 1;
                self.write_bg_descriptor(group, &bgd_mut);

                self.update_superblock_free_blocks();

                // реальный номер блока
                let real_block = bgd.bg_block_bitmap + i as u32; // грубо, но работает для первой группы
                return Some(real_block);
            }
        }
        None
    }

    // ====================== ВСПОМОГАТЕЛЬНЫЕ ======================

    fn write_bg_descriptor(&self, group: u32, bgd: &Ext2BlockGroupDescriptor) {
        let bgd_table_block = if self.superblock.s_first_data_block == 0 { 1 } else { self.superblock.s_first_data_block };
        let desc_size = core::mem::size_of::<Ext2BlockGroupDescriptor>() as u32;
        let bgds_per_block = self.block_size / desc_size;
        let block = bgd_table_block + (group / bgds_per_block);
        let offset = ((group % bgds_per_block) * desc_size) as usize;

        unsafe { self.write_struct_to_disk(block, offset, bgd); }
    }

    fn update_superblock_free_inodes(&mut self) {
        let mut sb = self.superblock;
        sb.s_free_inodes_count = sb.s_free_inodes_count.saturating_sub(1);
        // записываем обратно (грубо, но работает)
        unsafe {
            let mut buf = [0u8; 1024];
            core::ptr::copy_nonoverlapping(&sb as *const _ as *const u8, buf.as_mut_ptr(), 1024);
            self.disk.write(buf.as_ptr() as *const u32, 2, 2);
            self.superblock = sb;
        }
    }

    fn update_superblock_free_blocks(&mut self) {
        let mut sb = self.superblock;
        sb.s_free_blocks_count = sb.s_free_blocks_count.saturating_sub(1);
        unsafe {
            let mut buf = [0u8; 1024];
            core::ptr::copy_nonoverlapping(&sb as *const _ as *const u8, buf.as_mut_ptr(), 1024);
            self.disk.write(buf.as_ptr() as *const u32, 2, 2);
            self.superblock = sb;
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

        for i in 0..core::cmp::min(blocks_to_read, 12) {
            if inode.i_block[i] == 0 {
                break;
            }

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

    // ====================== DEBUG ======================

    pub fn list_directory(&self, inode_num: u32) {
        let inode = self.read_inode(inode_num);
        if (inode.i_mode & 0xF000) != 0x4000 {
            println!("[EXT2] Not a directory!");
            return;
        }

        println!("[EXT2] Directory listing for inode {}:", inode_num);
        for i in 0..12 {
            if inode.i_block[i] == 0 {
                break;
            }

            let mut block_buf = [0u8; 4096];
            unsafe { self.read_blocks(inode.i_block[i], block_buf.as_mut_ptr(), 1); }

            let mut offset = 0usize;
            while offset < self.block_size as usize {
                let entry = unsafe { core::ptr::read_unaligned(block_buf.as_ptr().add(offset) as *const Ext2DirEntry) };
                if entry.inode == 0 { break; }

                let name_start = offset + 8;
                let name_end = name_start + entry.name_len as usize;
                let name = core::str::from_utf8(&block_buf[name_start..name_end]).unwrap_or("???");
                let inode = entry.inode;
                let file_type = entry.file_type;
                println!("  [{:4}] {:<20} type={}", inode, name, file_type);

                offset += entry.rec_len as usize;
            }
        }
    }

    /// Удобный дебаг: list_directory по пути
    pub fn list_directory_path(&self, path: &str) {
        if let Some(inode) = self.resolve_path(path) {
            self.list_directory(inode);
            println!("[EXT2] Directory listing complete");
        } else {
            println!("[EXT2] Directory not found: {}", path);
        }
    }

    // ====================== УДАЛЕНИЕ ФАЙЛА ======================

    /// Удаляет файл по пути (пока только в корне)
    pub fn remove_file_path(&mut self, path: &str) -> bool {
        if !self.mounted {
            return false;
        }

        let name = match path.strip_prefix('/') {
            Some(n) if !n.contains('/') => n,
            _ => {
                println!("[EXT2] remove_file: only root files supported for now (/filename)");
                return false;
            }
        };

        // 1. Находим inode файла
        let inode_num = match self.resolve_path(path) {
            Some(n) => n,
            None => {
                println!("[EXT2] File not found: {}", path);
                return false;
            }
        };

        // 2. Удаляем запись из корневой директории
        if !self.remove_dir_entry(2, name) {
            println!("[EXT2] Failed to remove directory entry");
            return false;
        }

        // 3. Освобождаем блоки файла
        self.free_blocks_of_inode(inode_num);

        // 4. Освобождаем inode
        self.free_inode(inode_num);

        println!("[EXT2] Removed file: {} (inode {})", path, inode_num);
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

    /// Освобождаем один блок (через block bitmap)
    fn free_block(&mut self, block: u32) {
        if block == 0 { return; }

        let group = 0; // пока только первая группа
        let bgd = self.get_bg_descriptor(group);
        let block_bitmap_block = bgd.bg_block_bitmap;

        let mut bitmap = unsafe { self.read_bitmap(block_bitmap_block) };

        let bit_index = (block - bgd.bg_block_bitmap) as usize; // грубый расчёт
        let byte = bit_index / 8;
        let bit = bit_index % 8;

        if byte < bitmap.len() {
            bitmap[byte] &= !(1 << bit); // помечаем как свободный
            unsafe { self.write_bitmap(block_bitmap_block, &bitmap); }

            // обновляем счётчики
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
}

impl crate::filesystem::Filesystem for Ext2 {
    fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        self.read_file_path(path)
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

    fn list_directory(&self, path: &str) {
        self.list_directory_path(path);
    }

    fn is_mounted(&self) -> bool {
        self.mounted
    }
}