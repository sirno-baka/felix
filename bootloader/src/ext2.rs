//! Minimal read-only ext2 for stage2.
use crate::disk::DISK;

const SECTOR: u32 = 512;
const ROOT_INO: u32 = 2;
const EXT2_S_IFDIR: u16 = 0x4000;
const EXT2_S_IFREG: u16 = 0x8000;
const EXT2_FT_REG: u8 = 1;
const INDIRECT_BUF: u16 = 0x2000;
const L1_BUF: u16 = 0x3000;

// On-disk inode field offsets (128-byte inode).
const I_MODE: usize = 0;
const I_SIZE: usize = 4;
const I_FLAGS: usize = 32;
const I_BLOCK: usize = 40;
const EXT4_EXTENTS_FL: u32 = 0x0008_0000;

#[repr(C, packed)]
struct Superblock {
    s_inodes_count: u32,
    s_blocks_count: u32,
    s_r_blocks_count: u32,
    s_free_blocks_count: u32,
    s_free_inodes_count: u32,
    s_first_data_block: u32,
    s_log_block_size: u32,
    s_log_frag_size: u32,
    s_blocks_per_group: u32,
    s_frags_per_group: u32,
    s_inodes_per_group: u32,
    s_mtime: u32,
    s_wtime: u32,
    s_mnt_count: u16,
    s_max_mnt_count: u16,
    s_magic: u16,
}

#[repr(C, packed)]
struct GroupDesc {
    bg_block_bitmap: u32,
    bg_inode_bitmap: u32,
    bg_inode_table: u32,
}

struct Inode {
    mode: u16,
    size: u32,
    flags: u32,
    blocks: [u32; 15],
}

pub struct Ext2Fs {
    part_lba: u32,
    block_size: u32,
    inodes_per_group: u32,
    inode_size: u32,
    inode_table_block: u32,
    buf: u16,
}

fn names_eq(a: *const u8, a_len: usize, b: &[u8]) -> bool {
    if a_len != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a_len {
        if unsafe { *a.add(i) } != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

fn ru16(p: *const u8) -> u16 {
    unsafe { u16::from_le_bytes([*p, *p.add(1)]) }
}

fn ru32(p: *const u8) -> u32 {
    unsafe { u32::from_le_bytes([*p, *p.add(1), *p.add(2), *p.add(3)]) }
}

pub fn find_ext2_part_lba() -> u32 {
    let mbr = 0x7C00u16 as *const u8;
    unsafe {
        for i in 0..4 {
            let entry = mbr.add(0x1BE + i * 16);
            if *entry.add(4) == 0x83 {
                let lba = ru32(entry.add(8));
                println!("[ext2] partition LBA {}", lba);
                return lba;
            }
        }
    }
    println!("[ext2] no 0x83 partition, fallback LBA 2048");
    2048
}

impl Ext2Fs {
    pub fn mount(part_lba: u32, buf: u16) -> Option<Self> {
        print!("[ext2] superblock...");
        unsafe {
            DISK.init((part_lba + 2) as u64, buf);
            DISK.read_sector();
        }

        let base = buf as *const u8;
        if ru16(unsafe { base.add(56) }) != 0xEF53 {
            println!(" bad magic");
            return None;
        }
        println!(" ok");

        let sb = unsafe { core::ptr::read_unaligned(buf as *const Superblock) };
        let block_size = 1024u32 << sb.s_log_block_size;
        let inodes_per_group = sb.s_inodes_per_group;
        let first_data = sb.s_first_data_block;

        let mut inode_size = ru16(unsafe { base.add(88) }) as u32;
        if inode_size < 128 {
            inode_size = 128;
        }

        let gd_lba = part_lba + ((first_data + 1) * block_size) / SECTOR;
        unsafe {
            DISK.init(gd_lba as u64, buf);
            DISK.read_sector();
        }
        let gd = unsafe { core::ptr::read_unaligned(buf as *const GroupDesc) };
        let inode_table_block = gd.bg_inode_table;

        println!(
            "[ext2] bs={} isize={} itable={}",
            block_size, inode_size, inode_table_block
        );

        Some(Self {
            part_lba,
            block_size,
            inodes_per_group,
            inode_size,
            inode_table_block,
            buf,
        })
    }

    fn block_to_lba(&self, block: u32) -> u32 {
        self.part_lba + (block * self.block_size) / SECTOR
    }

    fn sectors_per_block(&self) -> u16 {
        (self.block_size / SECTOR) as u16
    }

    fn read_block_low(&self, block: u32, buf: u16) -> bool {
        if block == 0 {
            return false;
        }
        unsafe {
            DISK.init(self.block_to_lba(block) as u64, buf);
            DISK.read_low(self.sectors_per_block());
        }
        true
    }

    fn read_block_to_high(&self, block: u32, dest: u32) {
        if block == 0 {
            return;
        }
        unsafe {
            DISK.init(self.block_to_lba(block) as u64, self.buf);
            DISK.read_sectors(self.sectors_per_block(), dest);
        }
    }

    fn parse_inode(p: *const u8) -> Inode {
        let mut blocks = [0u32; 15];
        for i in 0..15 {
            blocks[i] = ru32(unsafe { p.add(I_BLOCK + i * 4) });
        }
        Inode {
            mode: ru16(p),
            size: ru32(unsafe { p.add(I_SIZE) }),
            flags: ru32(unsafe { p.add(I_FLAGS) }),
            blocks,
        }
    }

    fn read_inode(&self, ino: u32) -> Option<Inode> {
        if ino == 0 {
            return None;
        }
        let index = (ino - 1) % self.inodes_per_group;
        let byte_off = index * self.inode_size;
        let block_offset = byte_off / self.block_size;
        let within = (byte_off % self.block_size) as usize;
        if !self.read_block_low(self.inode_table_block + block_offset, self.buf) {
            return None;
        }
        Some(Self::parse_inode((self.buf as usize + within) as *const u8))
    }

    fn dir_lookup(&self, dir: &Inode, name: &[u8]) -> Option<(u32, u8)> {
        if dir.mode & EXT2_S_IFDIR == 0 {
            return None;
        }
        let size = dir.size as usize;
        let mut offset = 0usize;

        for bi in 0..12 {
            if offset >= size {
                break;
            }
            let b = dir.blocks[bi];
            if b == 0 {
                break;
            }
            if !self.read_block_low(b, self.buf) {
                return None;
            }

            let mut pos = 0usize;
            while pos + 8 <= self.block_size as usize && offset < size {
                let base = (self.buf as usize + pos) as *const u8;
                let inode = ru32(base);
                let rec_len = ru16(unsafe { base.add(4) }) as usize;
                let name_len = unsafe { *base.add(6) } as usize;
                let file_type = unsafe { *base.add(7) };
                if rec_len < 8 || rec_len > self.block_size as usize - pos {
                    break;
                }
                if inode != 0
                    && name_len > 0
                    && pos + 8 + name_len <= self.block_size as usize
                    && names_eq(unsafe { base.add(8) }, name_len, name)
                {
                    return Some((inode, file_type));
                }
                pos += rec_len;
                offset += rec_len;
            }
        }
        None
    }

    fn copy_file_blocks(&self, inode: &Inode, dest: u32) -> u32 {
        let size = inode.size;
        let mut written = 0u32;
        let mut dest_ptr = dest;

        for i in 0..12 {
            if written >= size {
                break;
            }
            let b = inode.blocks[i];
            if b == 0 {
                break;
            }
            self.read_block_to_high(b, dest_ptr);
            written += core::cmp::min(self.block_size, size - written);
            dest_ptr += self.block_size;
        }

        if written < size && inode.blocks[12] != 0 {
            if self.read_block_low(inode.blocks[12], INDIRECT_BUF) {
                let entries = (self.block_size / 4) as usize;
                for i in 0..entries {
                    if written >= size {
                        break;
                    }
                    let b = ru32((INDIRECT_BUF as usize + i * 4) as *const u8);
                    if b == 0 {
                        break;
                    }
                    self.read_block_to_high(b, dest_ptr);
                    written += core::cmp::min(self.block_size, size - written);
                    dest_ptr += self.block_size;
                }
            }
        }

        if written < size
            && inode.blocks[13] != 0
            && self.read_block_low(inode.blocks[13], INDIRECT_BUF)
        {
            let bs = self.block_size as usize;
            unsafe {
                core::ptr::copy_nonoverlapping(INDIRECT_BUF as *const u8, L1_BUF as *mut u8, bs);
            }
            let entries = (self.block_size / 4) as usize;
            for i in 0..entries {
                if written >= size {
                    break;
                }
                let l2 = ru32((L1_BUF as usize + i * 4) as *const u8);
                if l2 == 0 {
                    break;
                }
                if !self.read_block_low(l2, INDIRECT_BUF) {
                    break;
                }
                for j in 0..entries {
                    if written >= size {
                        break;
                    }
                    let b = ru32((INDIRECT_BUF as usize + j * 4) as *const u8);
                    if b == 0 {
                        break;
                    }
                    self.read_block_to_high(b, dest_ptr);
                    written += core::cmp::min(self.block_size, size - written);
                    dest_ptr += self.block_size;
                }
            }
        }

        written
    }

    pub fn load_file(&self, path: &str, dest: u32) -> Option<u32> {
        let mut node = self.read_inode(ROOT_INO)?;
        let mut file_type = 2u8;
        let mut ino = ROOT_INO;

        let mut start = 0usize;
        let bytes = path.as_bytes();
        while start < bytes.len() {
            if bytes[start] == b'/' {
                start += 1;
                continue;
            }
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'/' {
                end += 1;
            }
            match self.dir_lookup(&node, &bytes[start..end]) {
                Some((child, ft)) => {
                    file_type = ft;
                    ino = child;
                    node = self.read_inode(child)?;
                }
                None => {
                    println!("[ext2] missing file");
                    return None;
                }
            }
            start = end;
        }

        let is_reg = file_type == EXT2_FT_REG || (node.mode & 0xF000) == EXT2_S_IFREG;
        if !is_reg {
            println!("[ext2] not a file ft={} mode={}", file_type, node.mode);
            return None;
        }
        if node.flags & EXT4_EXTENTS_FL != 0 {
            println!("[ext2] extents not supported");
            return None;
        }

        println!("[ext2] ino={} sz={} b0={}", ino, node.size, node.blocks[0]);
        let written = self.copy_file_blocks(&node, dest);
        println!("[ext2] loaded {} / {}", written, node.size);
        Some(node.size)
    }
}
