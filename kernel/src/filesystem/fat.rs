//FAT16 FILESYSTEM IMPLEMENTATION

use crate::drivers::disk::DISK;
use core::mem;
use crate::{print, println};
use crate::multitasking::mutex::Mutex;

pub static mut FAT: Mutex<FatDriver> = Mutex::new(FatDriver {
    header: NULL_HEADER,
    entries: [NULL_ENTRY; ENTRY_COUNT],
    table: [0; FAT_SIZE],
    buffer: [0; 2048],
});

const ENTRY_COUNT: usize = 1024;
const FAT_START: u16 = 36864;

const FAT_SIZE: usize = 32768;   // 64 КБ — с запасом хватит для sectors_per_fat до ~250
//FAT16 header
#[derive(Copy, Clone, Debug)]
#[repr(C, packed)]
pub struct Header {
    boot_jump_instructions: [u8; 3],

    //bios parameter block
    oem_identifier: [u8; 8],
    bytes_per_sector: u16,
    sectors_per_cluster: u8,
    reserved_sectors: u16,
    fat_count: u8,
    dir_entries_count: u16,
    total_sectors: u16,
    media_descriptor_type: u8,
    sectors_per_fat: u16,
    sectors_per_track: u16,
    heads: u16,
    hidden_sectors: u32,
    large_sector_count: u32,

    //extended boot record
    drive_number: u8,
    reserved: u8,
    signature: u8,
    volume_id: u32,
    volume_label: [u8; 11],
    system_id: [u8; 8],
    zero: [u8; 460], //needed to make struct 512 bytes big
}

static NULL_HEADER: Header = Header {
    boot_jump_instructions: [0; 3],

    oem_identifier: [0; 8],
    bytes_per_sector: 0,
    sectors_per_cluster: 0,
    reserved_sectors: 0,
    fat_count: 0,
    dir_entries_count: 0,
    total_sectors: 0,
    media_descriptor_type: 0,
    sectors_per_fat: 0,
    sectors_per_track: 0,
    heads: 0,
    hidden_sectors: 0,
    large_sector_count: 0,

    drive_number: 0,
    reserved: 0,
    signature: 0,
    volume_id: 0,
    volume_label: [0; 11],
    system_id: [0; 8],
    zero: [0; 460],
};

//FAT file entry struct
#[derive(Copy, Clone, Debug)]
#[repr(C, packed)]
pub struct Entry {
    pub name: [u8; 11],
    attributes: u8,
    reserved: u8,
    created_time_tenths: u8,
    created_time: u16,
    created_date: u16,
    accessed_date: u16,
    first_cluster_high: u16,
    modified_time: u16,
    modified_date: u16,
    first_cluster_low: u16,
    size: u32,
}

static NULL_ENTRY: Entry = Entry {
    name: [0; 11],
    attributes: 0,
    reserved: 0,
    created_time_tenths: 0,
    created_time: 0,
    created_date: 0,
    accessed_date: 0,
    first_cluster_high: 0,
    modified_time: 0,
    modified_date: 0,
    first_cluster_low: 0,
    size: 0,
};

#[derive(Copy, Clone)]
pub struct FatDriver {
    pub header: Header,
    pub entries: [Entry; ENTRY_COUNT],
    //the root directory is an array of file entries
    pub table: [u16; FAT_SIZE],
    pub buffer: [u8; 2048],
}

impl FatDriver {
    //get header address and overwrite that mem location with data from boot sector
    pub fn load_header(&mut self) {
        let target = &mut self.header as *mut Header;

        let lba: u64 = FAT_START as u64;
        let sectors: u16 = 1;

        unsafe {
            DISK.read(target, lba, sectors);
        }
    }

    //get entries array address and overwrite that mem location with data from root directory
    //calculate size and position of root direcotry based on data from header
    pub fn load_entries(&mut self) {
        let target = &mut self.entries as *mut Entry;

        let entry_size = mem::size_of::<Entry>() as u16;

        let lba: u64 = FAT_START as u64
            + (self.header.reserved_sectors
                + self.header.sectors_per_fat * self.header.fat_count as u16) as u64;

        let size: u16 = entry_size * self.header.dir_entries_count;
        let sectors: u16 = size / self.header.bytes_per_sector;

        unsafe {
            DISK.read(target, lba, sectors);
        }
    }

    //list each entry in root direcotry
    //list each entry in root directory (теперь игнорирует удалённые файлы)
    pub fn list_entries(&self) {
        println!("Listing root directory entries:");

        println!("Name          Size          Cluster number");

        for i in 0..ENTRY_COUNT {
            let name0 = self.entries[i].name[0];
            if name0 == 0 || name0 == 0xE5 {
                continue; // пропускаем пустые и удалённые записи
            }

            //print name
            for c in self.entries[i].name {
                print!("{}", c as char);
            }
            //print size
            let size = self.entries[i].size;
            print!("   {} bytes", size);

            //print cluster
            let cluster = self.entries[i].first_cluster_low;
            print!("     {}", cluster);
            println!();
        }
    }

    // === ВСПОМОГАТЕЛЬНЫЕ ФУНКЦИИ ===

    // Преобразует "file.txt" → [u8;11] в формате 8.3 FAT (uppercase + пробелы)
    fn string_to_fat_name(filename: &str) -> [u8; 11] {
        let mut fat_name = [b' '; 11];
        let upper = filename.to_ascii_uppercase();
        let bytes = upper.as_bytes();

        if let Some(dot_pos) = bytes.iter().position(|&b| b == b'.') {
            // имя (до 8 символов)
            for (i, &b) in bytes.iter().take(8).take(dot_pos).enumerate() {
                fat_name[i] = b;
            }
            // расширение (3 символа)
            let ext_start = dot_pos + 1;
            for (i, &b) in bytes.iter().skip(ext_start).take(3).enumerate() {
                fat_name[8 + i] = b;
            }
        } else {
            // без расширения
            for (i, &b) in bytes.iter().take(11).enumerate() {
                fat_name[i] = b;
            }
        }
        fat_name
    }

    // Поиск свободного слота в root directory (0x00 или 0xE5 = свободно/удалено)
    pub fn find_free_entry(&mut self) -> Option<usize> {
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if entry.name[0] == 0x00 || entry.name[0] == 0xE5 {
                return Some(i);
            }
        }
        None
    }

    // Поиск следующего свободного кластера начиная с указанного номера
    pub fn find_free_cluster(&self, start: u16) -> Option<u16> {
        let start = start.max(2);
        for i in start..self.table.len() as u16 {
            if self.table[i as usize] == 0 {
                return Some(i);
            }
        }
        None
    }

    // Выделение цепочки кластеров и связывание их в FAT
    pub fn allocate_clusters(&mut self, num_clusters: usize) -> Option<u16> {
        if num_clusters == 0 {
            return None;
        }

        let mut first_cluster: Option<u16> = None;
        let mut prev_cluster: u16 = 0;

        for i in 0..num_clusters {
            // Для первого кластера ищем с 2, для остальных — после предыдущего
            let start_search = if i == 0 { 2u16 } else { prev_cluster + 1 };

            let cluster = match self.find_free_cluster(start_search) {
                Some(c) => c,
                None => return None, // нет свободного места
            };

            if first_cluster.is_none() {
                first_cluster = Some(cluster);
            }

            // Связываем предыдущий кластер с текущим
            if prev_cluster != 0 {
                self.table[prev_cluster as usize] = cluster;
            }

            prev_cluster = cluster;
        }

        // Последний кластер — конец файла
        if prev_cluster != 0 {
            self.table[prev_cluster as usize] = 0xFFFF;
        }

        first_cluster
    }

    // LBA кластера данных
    fn cluster_to_lba(&self, cluster: u16) -> u64 {
        let root_dir_sectors = ((self.header.dir_entries_count as u32 * 32) / self.header.bytes_per_sector as u32) as u64;

        let data_start_lba = FAT_START as u64
            + self.header.reserved_sectors as u64
            + (self.header.sectors_per_fat as u64 * self.header.fat_count as u64)
            + root_dir_sectors;

        data_start_lba + ((cluster as u64 - 2) * self.header.sectors_per_cluster as u64)
    }

    // Запись данных в цепочку кластеров
    fn write_data_to_clusters(&self, mut cluster: u16, data: &[u8]) {
        let cluster_size = self.header.sectors_per_cluster as usize * self.header.bytes_per_sector as usize;
        let mut offset = 0;

        while offset < data.len() && cluster != 0xFFFF {
            let chunk_len = (data.len() - offset).min(cluster_size);
            let chunk = &data[offset..offset + chunk_len];

            let lba = self.cluster_to_lba(cluster);

            // Пишем целый кластер (последний может быть частично заполнен — остаток останется как есть)
            unsafe {
                DISK.write(chunk.as_ptr() as *const u32, lba, self.header.sectors_per_cluster as u16);
            }

            offset += chunk_len;
            cluster = self.table[cluster as usize];
        }
    }

    pub fn delete_file(&mut self, filename: &str) -> bool {

        self.load_table();

        let fat_name = Self::string_to_fat_name(filename);

        let mut entry_index = None;
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.name == fat_name {
                entry_index = Some(i);
                break;
            }
        }

        let entry_index = match entry_index {
            Some(idx) => idx,
            None => {
                println!("[FAT WARN] File '{}' not found", filename);
                return false;
            }
        };

        let first_cluster = self.entries[entry_index].first_cluster_low;

        // Освобождаем цепочку кластеров
        if first_cluster >= 2 {
            let mut cluster = first_cluster;
            while cluster != 0xFFFF && cluster != 0 {
                let next = self.table[cluster as usize];
                self.table[cluster as usize] = 0;
                cluster = next;
            }
        }

        // Помечаем как удалённый + чистим данные
        self.entries[entry_index].name[0] = 0xE5;
        self.entries[entry_index].size = 0;
        self.entries[entry_index].first_cluster_low = 0;

        self.save_table();
        self.save_entries();

        println!("[FAT OK] File '{}' successfully deleted", filename);
        true
    }

    // === ОСНОВНОЙ МЕТОД: СОЗДАНИЕ + ЗАПИСЬ ФАЙЛА ===
    // Если файл уже существует — перезаписывает его
    pub fn write_file(&mut self, filename: &str, data: &[u8]) -> bool {
        // if !DISK.enabled {
        //     println!("[FAT ERROR] Disk not enabled");
        //     return false;
        // }

        self.load_table();   // загружаем актуальную таблицу

        let fat_name = Self::string_to_fat_name(filename);

        // 1. Ищем существующий файл (иммутабельный поиск)
        let mut entry_index = None;
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.name == fat_name {
                entry_index = Some(i);
                break;
            }
        }

        // 2. Если файла нет — ищем свободный слот
        if entry_index.is_none() {
            entry_index = self.find_free_entry();
        }

        let entry_index = match entry_index {
            Some(idx) => idx,
            None => {
                println!("[FAT ERROR] No free directory entry");
                return false;
            }
        };

        // 3. Сколько кластеров нужно?
        let bytes_per_cluster = self.header.sectors_per_cluster as usize * self.header.bytes_per_sector as usize;
        let num_clusters = if data.is_empty() { 0 } else { (data.len() + bytes_per_cluster - 1) / bytes_per_cluster };

        // 4. Выделяем кластеры (теперь borrow'ов нет)
        let first_cluster = if num_clusters > 0 {
            match self.allocate_clusters(num_clusters) {
                Some(c) => c,
                None => {
                    println!("[FAT ERROR] Not enough free space");
                    return false;
                }
            }
        } else {
            0
        };

        // 5. Записываем данные в кластеры
        if num_clusters > 0 {
            self.write_data_to_clusters(first_cluster, data);
        }

        // 6. Заполняем/обновляем запись в директории (только сейчас)
        self.entries[entry_index] = Entry {
            name: fat_name,
            attributes: 0x20,                    // Archive
            reserved: 0,
            created_time_tenths: 0,
            created_time: 0,
            created_date: 0x21C0,                // dummy дата
            accessed_date: 0x21C0,
            first_cluster_high: 0,
            modified_time: 0,
            modified_date: 0x21C0,
            first_cluster_low: first_cluster,
            size: data.len() as u32,
        };

        // 7. Сохраняем изменения на диск
        self.save_table();
        self.save_entries();

        println!("[FAT OK] File '{}' written ({} bytes, start cluster {})",
                 filename, data.len(), first_cluster);
        true
    }

    //load file allocation table
    //load full FAT table (было только 1 сектор)
    pub fn load_table(&mut self) {
        let target = &mut self.table as *mut u16;

        let lba: u64 = FAT_START as u64 + self.header.reserved_sectors as u64;
        let sectors: u16 = self.header.sectors_per_fat;   // ← теперь полный FAT
        unsafe {
            DISK.read(target, lba, sectors);
        }
    }

    //save full FAT table
    pub fn save_table(&self) {
        let source = &self.table as *const u16;
        let lba: u64 = FAT_START as u64 + self.header.reserved_sectors as u64;
        let sectors: u16 = self.header.sectors_per_fat;

        unsafe {
            DISK.write(source, lba, sectors);
        }
    }

    //save header (boot sector) back to disk
    pub fn save_header(&self) {
        let source = &self.header as *const Header;
        let lba: u64 = FAT_START as u64;
        unsafe {
            DISK.write(source, lba, 1);
        }
    }


    //save root directory entries back to disk
    pub fn save_entries(&self) {
        let source = &self.entries as *const Entry;
        let lba: u64 = FAT_START as u64
            + (self.header.reserved_sectors
            + self.header.sectors_per_fat * self.header.fat_count as u16) as u64;

        let entry_size = mem::size_of::<Entry>() as u16;
        let size: u16 = entry_size * self.header.dir_entries_count;
        let sectors: u16 = size / self.header.bytes_per_sector;

        unsafe {
            DISK.write(source, lba, sectors);
        }
    }

    //read first cluster of file to buffer
    pub fn read_file_to_buffer(&self, entry: &Entry) {
        let target = self.buffer.as_ptr() as *mut u8;

        let data_lba: u64 = FAT_START as u64
            + (self.header.reserved_sectors
                + self.header.sectors_per_fat * self.header.fat_count as u16
                + 32) as u64;
        let lba: u64 = data_lba
            + ((entry.first_cluster_low - 2) * self.header.sectors_per_cluster as u16) as u64;

        let sectors: u16 = self.header.sectors_per_cluster as u16;

        unsafe {
            DISK.read(target, lba, sectors);
        }
    }

    //read file reading one cluster at time
    pub fn read_file_to_target(&self, entry: &Entry, target: *mut u32) {
        let mut next_cluster = entry.first_cluster_low;
        let mut current_target = target;

        //loop cluster read, until it reaches 0xffff in fat
        loop {
            let data_lba: u64 = FAT_START as u64
                + (self.header.reserved_sectors
                    + self.header.sectors_per_fat * self.header.fat_count as u16
                    + 32) as u64;

            let lba: u64 =
                data_lba + ((next_cluster - 2) * self.header.sectors_per_cluster as u16) as u64;

            let sectors: u16 = self.header.sectors_per_cluster as u16;

            unsafe {
                DISK.read(current_target, lba, sectors);
            }

            next_cluster = self.table[next_cluster as usize];

            //after reading a cluster, increment target by cluster size
            unsafe {
                //let cluster_size = 2048;
                let cluster_size =
                    self.header.sectors_per_cluster as u16 * self.header.bytes_per_sector;
                current_target = current_target.byte_add(cluster_size as usize);
            }

            if next_cluster == 0xffff {
                break;
            }
        }
    }

    //search by filename, returns found root entry
    pub fn search_file(&self, name: &[char]) -> &Entry {
        for entry in self.entries.iter() {
            let mut found = true;
            let mut i = 0;

            for n in name {
                let mut c = n.clone();

                if c.is_ascii_lowercase() {
                    c = c.to_ascii_uppercase();
                }

                if (c != entry.name[i] as char) && (name[i] != '\0') {
                    found = false;
                }

                i += 1;
            }

            if found {
                return entry;
            }
        }

        &NULL_ENTRY
    }
}
