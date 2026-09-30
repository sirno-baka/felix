// BIOS disk reader for stage2.
//
// Keep every BIOS transfer below 1 MiB. Reads destined for high memory use a
// 64 KiB-aligned bounce buffer at 0x10000, then stage2 restores unreal mode
// and copies the chunk to its final physical address. This avoids depending on
// EDD 3.0 flat-address DAP support, which is inconsistent across BIOSes.

use core::arch::asm;
use core::mem;

pub static mut DISK: Disk = Disk { lba: 0, buffer: 0 };

static mut FAST_HIGH_READS: bool = false;

pub fn set_fast_high_reads(enabled: bool) {
    unsafe { FAST_HIGH_READS = enabled; }
}

pub fn fast_high_reads() -> bool {
    unsafe { FAST_HIGH_READS }
}

const HIGH_BOUNCE_PHYS: u32 = 0x0001_0000;
const HIGH_BOUNCE_SEG: u16 = 0x1000;
const MAX_BIOS_SECTORS: u16 = 127;

#[repr(C, packed)]
struct DiskAddressPacket {
    size: u8,
    zero: u8,
    sectors: u16,
    offset: u16,
    segment: u16,
    lba: u64,
}

pub struct Disk {
    lba: u64,
    buffer: u16,
}

impl Disk {
    pub fn init(&mut self, lba: u64, buffer: u16) {
        self.lba = lba;
        self.buffer = buffer;
    }

    fn int13_to(&self, sectors: u16, segment: u16, offset: u16) {
        let dap = DiskAddressPacket {
            size: mem::size_of::<DiskAddressPacket>() as u8,
            zero: 0,
            sectors,
            offset,
            segment,
            lba: self.lba,
        };
        let dap_address = &dap as *const DiskAddressPacket as u16;

        unsafe {
            asm!(
                "push ds",
                "push es",
                "push si",
                "push ax",
                "xor ax, ax",
                "mov ds, ax",
                "mov es, ax",
                "pop ax",
                "mov si, {dap:x}",
                "sti",
                "int 0x13",
                "cli",
                "cld",
                "jc fail",
                "pop si",
                "pop es",
                "pop ds",
                dap = in(reg) dap_address,
                in("ax") 0x4200u16,
                in("dx") crate::boot_drive() as u16,
            );
        }
    }

    fn int13_low(&self, sectors: u16) {
        self.int13_to(sectors, 0, self.buffer);
    }

    /// EDD version reported by INT 13h AH=41h. Zero means extensions absent.
    pub fn edd_version() -> u8 {
        let mut ax = 0x4100u16;
        let mut bx = 0x55AAu16;
        let mut cx = 0u16;
        let ok: u8;
        unsafe {
            asm!(
                "sti",
                "int 0x13",
                "cli",
                "setnc {ok}",
                ok = lateout(reg_byte) ok,
                inout("ax") ax,
                inout("bx") bx,
                inout("cx") cx,
                in("dx") crate::boot_drive() as u16,
            );
        }
        if ok != 0 && bx == 0xAA55 && (cx & 1) != 0 {
            (ax >> 8) as u8
        } else {
            0
        }
    }

    pub fn read_sector(&self) {
        self.int13_low(1);
    }

    pub fn read_low(&self, sectors: u16) {
        self.int13_low(sectors);
    }

    /// Compatibility path for isolated filesystem blocks.
    /// Legacy IDE/PXE keeps the old low-buffer path. USB/non-ATA may opt into
    /// the larger 0x10000 bounce-buffer path.
    pub fn read_sectors(&mut self, sectors: u16, target: u32) {
        if fast_high_reads() {
            let lba = self.lba;
            self.read_extent_to_high(lba, sectors as u32, target);
        } else {
            self.int13_low(sectors);
            crate::restore_unreal();
            copy_high(self.buffer as u32, target, sectors as u32 * 512);
        }
    }

    /// Read a contiguous LBA extent to high physical memory.
    /// Each BIOS request is at most 127 sectors and targets 0x10000, so it
    /// never crosses the conventional 64 KiB DMA/BIOS transfer boundary.
    pub fn read_extent_to_high(&mut self, start_lba: u64, total_sectors: u32, target: u32) {
        let mut done = 0u32;
        while done < total_sectors {
            let n = core::cmp::min(MAX_BIOS_SECTORS as u32, total_sectors - done) as u16;
            self.lba = start_lba + done as u64;
            self.int13_to(n, HIGH_BOUNCE_SEG, 0);

            // BIOS calls are allowed to reload segment registers and destroy
            // unreal-mode hidden limits. Rebuild them once per chunk.
            crate::restore_unreal();
            copy_high(
                HIGH_BOUNCE_PHYS,
                target + done * 512,
                n as u32 * 512,
            );
            done += n as u32;
        }
    }

    /// BIOS INT 13h AH=48h — total sector count (LBA). Falls back to `fallback`.
    pub fn drive_sector_count(fallback: u32) -> u32 {
        #[repr(C, packed)]
        struct DriveParams {
            size: u16,
            flags: u16,
            cylinders: u32,
            heads: u32,
            sectors_per_track: u32,
            sectors: u64,
            bytes_per_sector: u16,
        }

        let mut params = DriveParams {
            size: 0x1A,
            flags: 0,
            cylinders: 0,
            heads: 0,
            sectors_per_track: 0,
            sectors: 0,
            bytes_per_sector: 0,
        };

        let ok: u8;
        let params_off = &mut params as *mut DriveParams as u16;
        unsafe {
            asm!(
                "push ds",
                "push si",
                "push ax",
                "xor ax, ax",
                "mov ds, ax",
                "pop ax",
                "mov si, {params:x}",
                "sti",
                "int 0x13",
                "cli",
                "setnc {ok}",
                "pop si",
                "pop ds",
                params = in(reg) params_off,
                ok = lateout(reg_byte) ok,
                in("ax") 0x4800u16,
                in("dx") crate::boot_drive() as u16,
            );
        }

        if ok != 0 && params.sectors > 0 && params.sectors < 0x1000_0000 {
            params.sectors as u32
        } else {
            fallback
        }
    }

    /// Copy the complete BIOS boot disk to high RAM.
    /// PXE uses the old 16-sector bounce path; USB/non-ATA uses larger extents.
    pub fn copy_disk_to_ram(&mut self, total: u32, target: u32) {
        if !fast_high_reads() {
            const CHUNK: u16 = 16;
            let mut done = 0u32;
            while done < total {
                let n = core::cmp::min(CHUNK as u32, total - done) as u16;
                self.lba = done as u64;
                self.int13_low(n);
                crate::restore_unreal();
                copy_high(
                    self.buffer as u32,
                    target + done * 512,
                    n as u32 * 512,
                );
                done += n as u32;
                if done % 2048 == 0 || done == total {
                    println!("[disk] ram {} / {} sectors", done, total);
                }
            }
            return;
        }

        const PROGRESS_CHUNK: u32 = 2048; // 1 MiB
        let mut done = 0u32;
        while done < total {
            let n = core::cmp::min(PROGRESS_CHUNK, total - done);
            self.read_extent_to_high(done as u64, n, target + done * 512);
            done += n;
            println!("[disk] ram {} / {} sectors", done, total);
        }
    }
}

fn copy_high(src: u32, dst: u32, len: u32) {
    unsafe {
        asm!(
            "2:",
            "mov eax, [{0:e}]",
            "mov [{1:e}], eax",
            "add {0:e}, 4",
            "add {1:e}, 4",
            "sub {2:e}, 4",
            "jnz 2b",
            inout(reg) src => _,
            inout(reg) dst => _,
            inout(reg) len => _,
            out("eax") _,
        );
    }
}
