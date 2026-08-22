//! Minimal read-only ext2 for the 16-bit bootloader.
//! Assumes: block size 1024, inode size 128, no extents (classic direct blocks).
//! Partition LBA is absolute on the disk (BIOS drive 0x80).

use core::mem::size_of;
use crate::disk::DISK;
use crate::print;

const SECTOR: u32 = 512;
const SUPER_MAGIC: u16 = 0xEF53;
const ROOT_INODE: u32 = 2;

/// Low buffer in conventional memory for one FS block (max 4 KiB).
const BLOCK_BUF: u16 = 0x9000;

#[repr(C, packed)]
struct Superblock {
    inodes_count: u32,
    blocks_count: u32,
    _r1: u32,
    free_blocks: u32,
    free_inodes: u32,
    first_data_block: u32,
    log_block_size: u32,
    _log_frag: u32,
    blocks_per_group: u32,
    _frags_per_group: u32,
    inodes_per_group: u32,
    _mtime: u32,
    _wtime: u32,
    _mnt_count: u16,
    _max_mnt: u16,
    magic: u16,
    // ... rest ignored
}

#[repr(C, packed)]
struct GroupDesc {
    block_bitmap: u32,
    inode_bitmap: u32,
    inode_table: u32,
    // ...
}

#[repr(C, packed)]
struct Inode {
    mode: u16,
    uid: u16,
    size: u32,
    _atime: u32,
    _ctime: u32,
    _mtime: u32,
    _dtime: u32,
    gid: u16,
    links: u16,
    blocks: u32,
    flags: u32,
    _osd1: u32,
    block: [u32; 15],
    // ...
}

#[repr(C, packed)]
struct DirEntry {
    inode: u32,
    rec_len: u16,
    name_len: u8,
    file_type: u8,
    // name follows
}

pub struct Ext2 {
    part_lba: u32,
    block_size: u32,
    inodes_per_group: u32,
    inode_size: u32,
    first_data_block: u32,
}

impl Ext2 {
    /// Mount partition starting at `part_lba` (absolute disk LBA).
    pub fn mount(part_lba: u32) -> Option<Self> {
        // Superblock at byte 1024 of the FS = part_lba + 2 sectors
        unsafe {
            DISK.init(part_lba + 2, BLOCK_BUF);
            DISK.read_sectors(2, BLOCK_BUF as u32); // 1024 bytes into buffer
        }

        let sb = unsafe { &*(BLOCK_BUF as *const Superblock) };
        let magic = sb.magic;
        if magic != SUPER_MAGIC {
            println!("[ext2] bad magic {:x}", magic as u32);
            return None;
        }

        let log_bs = sb.log_block_size;
        let block_size = 1024u32 << log_bs;
        if block_size != 1024 && block_size != 2048 && block_size != 4096 {
            println!("[ext2] unsupported bs {}", block_size);
            return None;
        }

        // inode size: for rev0 = 128; we force -I 128 in mkfs
        let inode_size = 128u32;

        Some(Ext2 {
            part_lba,
            block_size,
            inodes_per_group: sb.inodes_per_group,
            inode_size,
            first_data_block: sb.first_data_block,
        })
    }

    fn sectors_per_block(&self) -> u16 {
        (self.block_size / SECTOR) as u16
    }

    fn read_block(&self, block: u32, dest_phys: u32) {
        if block == 0 {
            return;
        }
        let lba = self.part_lba + block * (self.block_size / SECTOR);
        let n = self.sectors_per_block();
        unsafe {
            DISK.init(lba, BLOCK_BUF);
            DISK.read_sectors(n, dest_phys);
        }
    }

    fn read_block_to_buf(&self, block: u32) {
        self.read_block(block, BLOCK_BUF as u32);
    }

    fn inode_block_and_off(&self, ino: u32) -> (u32, u32) {
        let idx = ino - 1;
        let group = idx / self.inodes_per_group;
        let index = idx % self.inodes_per_group;

        // group descriptor: block after superblock
        // for 1k: first_data_block=1 (super in block1), GDT in block2
        // for 4k: first_data_block=0, super in block0, GDT in block1
        let gdt_block = if self.block_size == 1024 {
            self.first_data_block + 1
        } else {
            self.first_data_block + 1
        };

        self.read_block_to_buf(gdt_block);
        let gd = unsafe {
            &*((BLOCK_BUF as u32 + group * size_of::<GroupDesc>() as u32) as *const GroupDesc)
        };
        let table = gd.inode_table;
        let byte_off = index * self.inode_size;
        let blk = table + byte_off / self.block_size;
        let off = byte_off % self.block_size;
        (blk, off)
    }

    fn read_inode(&self, ino: u32) -> Inode {
        let (blk, off) = self.inode_block_and_off(ino);
        self.read_block_to_buf(blk);
        unsafe { *((BLOCK_BUF as u32 + off) as *const Inode) }
    }

    /// Look up `name` in directory inode `dir_ino`. Returns child inode or 0.
    fn lookup(&self, dir_ino: u32, name: &[u8]) -> u32 {
        let inode = self.read_inode(dir_ino);
        // Walk direct blocks only (enough for small dirs)
        for bi in 0..12 {
            let b = inode.block[bi];
            if b == 0 {
                break;
            }
            self.read_block_to_buf(b);
            let mut pos = 0u32;
            while pos + 8 <= self.block_size {
                let de = unsafe { &*((BLOCK_BUF as u32 + pos) as *const DirEntry) };
                let rec = de.rec_len as u32;
                if rec == 0 {
                    break;
                }
                if de.inode != 0 && de.name_len as usize == name.len() {
                    let nptr = (BLOCK_BUF as u32 + pos + 8) as *const u8;
                    let mut same = true;
                    for i in 0..name.len() {
                        if unsafe { *nptr.add(i) } != name[i] {
                            same = false;
                            break;
                        }
                    }
                    if same {
                        return de.inode;
                    }
                }
                pos += rec;
            }
        }
        0
    }

    /// Resolve absolute path like "/boot/kernel.bin" (leading slash required).
    pub fn resolve(&self, path: &[u8]) -> Option<u32> {
        let mut ino = ROOT_INODE;
        let mut i = 0usize;
        // skip leading '/'
        if !path.is_empty() && path[0] == b'/' {
            i = 1;
        }
        while i < path.len() {
            let start = i;
            while i < path.len() && path[i] != b'/' {
                i += 1;
            }
            let comp = &path[start..i];
            if !comp.is_empty() {
                ino = self.lookup(ino, comp);
                if ino == 0 {
                    return None;
                }
            }
            if i < path.len() && path[i] == b'/' {
                i += 1;
            }
        }
        Some(ino)
    }

    /// Load file inode into physical memory at `dest`. Returns file size.
    pub fn load_file(&self, ino: u32, dest: u32) -> Option<u32> {
        let inode = self.read_inode(ino);
        let size = inode.size;
        if size == 0 {
            return Some(0);
        }
        // Direct blocks only: 12 * block_size (12 KiB / 24 / 48)
        let mut left = size;
        let mut dst = dest;
        for bi in 0..12 {
            if left == 0 {
                break;
            }
            let b = inode.block[bi];
            if b == 0 {
                break;
            }
            self.read_block(b, dst);
            let chunk = core::cmp::min(left, self.block_size);
            left -= chunk;
            dst += self.block_size;
        }
        // Single indirect
        if left > 0 && inode.block[12] != 0 {
            self.read_block_to_buf(inode.block[12]);
            let ptrs = (self.block_size / 4) as usize;
            for pi in 0..ptrs {
                if left == 0 {
                    break;
                }
                let pb = unsafe { *((BLOCK_BUF as u32 + (pi as u32) * 4) as *const u32) };
                if pb == 0 {
                    break;
                }
                // Need a second buffer for data — reuse high copy via DISK target
                self.read_block(pb, dst);
                let chunk = core::cmp::min(left, self.block_size);
                left -= chunk;
                dst += self.block_size;
            }
        }
        if left > 0 {
            println!("[ext2] file too big / no double-indirect");
            return None;
        }
        Some(size)
    }

    /// Open path and load to dest.
    pub fn load_path(&self, path: &[u8], dest: u32) -> Option<u32> {
        let ino = self.resolve(path)?;
        self.load_file(ino, dest)
    }
}
